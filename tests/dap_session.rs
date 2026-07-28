//! `tclrs --dap` against a debugger client, over the wire it actually speaks.
//!
//! The unit tests in `src/dap.rs` cover the markers a debug compilation emits.
//! This one covers the session: a breakpoint that stops the run on the line it
//! names, variables read from the stopped VM, the debuggee's own output
//! arriving as events rather than as protocol noise, and a continue that lets
//! the program finish.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    seq: i64,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(TCLRS)
            .arg("--dap")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tclrs --dap");
        let input = child.stdin.take().expect("stdin");
        let output = BufReader::new(child.stdout.take().expect("stdout"));
        Client {
            child,
            input: Some(input),
            output,
            seq: 0,
        }
    }

    fn request(&mut self, command: &str, arguments: serde_json::Value) {
        self.seq += 1;
        let body = serde_json::json!({
            "seq": self.seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        })
        .to_string();
        let input = self.input.as_mut().expect("input open");
        write!(input, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write");
        input.flush().expect("flush");
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut length = 0;
        loop {
            let mut header = String::new();
            self.output.read_line(&mut header).expect("read header");
            assert!(!header.is_empty(), "adapter closed the connection early");
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.trim().strip_prefix("Content-Length:") {
                length = value.trim().parse().expect("length");
            }
        }
        let mut body = vec![0; length];
        self.output.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).expect("parse body")
    }

    /// Messages until one satisfies `wanted`, collecting everything seen — the
    /// adapter interleaves events with responses, and a test usually wants both.
    fn until(&mut self, wanted: impl Fn(&serde_json::Value) -> bool) -> Vec<serde_json::Value> {
        let mut seen = Vec::new();
        for _ in 0..32 {
            let message = self.recv();
            let done = wanted(&message);
            seen.push(message);
            if done {
                return seen;
            }
        }
        panic!("no matching message in 32: {seen:#?}");
    }

    fn response(&mut self, command: &str) -> serde_json::Value {
        let seen = self.until(|m| m["type"] == "response" && m["command"] == command);
        seen.into_iter().next_back().expect("a response")
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A script on disk for the adapter to launch.
fn script(name: &str, text: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tclrs-dap-{name}.tcl"));
    std::fs::write(&path, text).expect("write script");
    path
}

/// Launch `path`, with breakpoints on `lines`, up to the point where the
/// program is running.
fn launched(path: &std::path::Path, lines: &[u32]) -> Client {
    let mut client = Client::start();
    client.request("initialize", serde_json::json!({"adapterID": "tclrs"}));
    let initialize = client.response("initialize");
    assert_eq!(
        initialize["body"]["supportsConfigurationDoneRequest"], true,
        "{initialize}"
    );

    client.request(
        "launch",
        serde_json::json!({"program": path.to_string_lossy()}),
    );
    client.response("launch");

    let breakpoints: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::json!({"line": line}))
        .collect();
    client.request(
        "setBreakpoints",
        serde_json::json!({
            "source": {"path": path.to_string_lossy()},
            "breakpoints": breakpoints,
        }),
    );
    let set = client.response("setBreakpoints");
    assert_eq!(
        set["body"]["breakpoints"].as_array().map(Vec::len),
        Some(lines.len()),
        "{set}"
    );

    client.request("configurationDone", serde_json::json!({}));
    client.response("configurationDone");
    client
}

#[test]
fn a_breakpoint_stops_the_run_and_variables_read_the_stopped_vm() {
    let path = script("breakpoint", "set x 21\nset y [expr {$x * 2}]\nputs $y\n");
    let mut client = launched(&path, &[3]);

    let stopped = client.until(|m| m["event"] == "stopped");
    let event = stopped.last().expect("the stop");
    assert_eq!(event["body"]["reason"], "breakpoint", "{event}");

    client.request("stackTrace", serde_json::json!({"threadId": 1}));
    let trace = client.response("stackTrace");
    // Stopped *before* line 3 runs, so `puts` has not printed yet.
    assert_eq!(trace["body"]["stackFrames"][0]["line"], 3, "{trace}");

    client.request("scopes", serde_json::json!({"frameId": 1}));
    client.response("scopes");
    client.request("variables", serde_json::json!({"variablesReference": 1}));
    let variables = client.response("variables");
    let listed = variables["body"]["variables"]
        .as_array()
        .expect("variables")
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap_or_default().to_string(),
                v["value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        listed.contains(&("x".to_string(), "21".to_string())),
        "{listed:?}"
    );
    assert!(
        listed.contains(&("y".to_string(), "42".to_string())),
        "{listed:?}"
    );

    // Resuming runs the rest: the program's own output arrives as an event, and
    // the session terminates.
    client.request("continue", serde_json::json!({"threadId": 1}));
    let rest = client.until(|m| m["event"] == "terminated");
    let printed: String = rest
        .iter()
        .filter(|m| m["event"] == "output")
        .filter_map(|m| m["body"]["output"].as_str())
        .collect();
    assert!(
        printed.contains("42"),
        "program output missing: {printed:?}"
    );
}

/// A breakpoint inside a procedure is reachable, because the markers are
/// emitted into bodies as well as at the top level.
#[test]
fn stepping_walks_command_by_command_into_a_procedure() {
    let path = script(
        "stepping",
        "proc double {x} {\n  set doubled [expr {$x * 2}]\n  return $doubled\n}\nset out [double 21]\nputs $out\n",
    );
    let mut client = launched(&path, &[5]);

    // Stop before the call, then step: the next command is the procedure's
    // first, on line 2.
    let stopped = client.until(|m| m["event"] == "stopped");
    assert_eq!(
        stopped.last().expect("stop")["body"]["reason"],
        "breakpoint"
    );

    client.request("stepIn", serde_json::json!({"threadId": 1}));
    let stepped = client.until(|m| m["event"] == "stopped");
    assert_eq!(
        stepped.last().expect("stop")["body"]["reason"],
        "step",
        "{stepped:#?}"
    );

    client.request("stackTrace", serde_json::json!({"threadId": 1}));
    let trace = client.response("stackTrace");
    assert_eq!(trace["body"]["stackFrames"][0]["line"], 2, "{trace}");

    client.request("continue", serde_json::json!({"threadId": 1}));
    client.until(|m| m["event"] == "terminated");
}

/// With no breakpoints the program runs to the end, and everything it printed
/// arrives as output events — nothing of the debuggee's is written to the
/// channel raw.
#[test]
fn a_run_without_breakpoints_finishes_and_reports_its_output() {
    let path = script("plain", "puts one\nputs two\n");
    let mut client = launched(&path, &[]);

    let seen = client.until(|m| m["event"] == "terminated");
    let printed: String = seen
        .iter()
        .filter(|m| m["event"] == "output")
        .filter_map(|m| m["body"]["output"].as_str())
        .collect();
    assert!(printed.contains("one"), "{printed:?}");
    assert!(printed.contains("two"), "{printed:?}");
    assert!(seen.iter().all(|m| m["type"] != "request"), "{seen:#?}");
}
