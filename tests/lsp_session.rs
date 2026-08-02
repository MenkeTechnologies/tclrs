//! `tclrs --lsp` against a client, over the wire it actually speaks.
//!
//! The unit tests in `src/lsp.rs` cover what each request answers. This one
//! covers the parts only a running process has: the `Content-Length` framing,
//! the initialize handshake, the diagnostics that arrive unasked after a
//! document is opened, and a clean shutdown. A server that computes the right
//! answer and frames it wrongly is a server no editor can use.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const TCLRS: &str = env!("CARGO_BIN_EXE_tclrs");

/// A client: writes framed requests, reads framed messages back.
struct Client {
    child: Child,
    /// Taken to close the pipe: the server's reader thread ends at end of
    /// input, and its `io_threads.join()` waits for that thread, so a client
    /// that holds the pipe open after `exit` keeps the process alive. An editor
    /// closes it; so does [`Client::close_input`].
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(TCLRS)
            .arg("--lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tclrs --lsp");
        let input = child.stdin.take().expect("stdin");
        let output = BufReader::new(child.stdout.take().expect("stdout"));
        Client {
            child,
            input: Some(input),
            output,
        }
    }

    fn send(&mut self, message: serde_json::Value) {
        let body = serde_json::to_string(&message).expect("serialize");
        let input = self.input.as_mut().expect("input still open");
        write!(input, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write");
        input.flush().expect("flush");
    }

    /// Close the pipe, as an editor does when it is done with the server.
    fn close_input(&mut self) {
        self.input.take();
    }

    /// One framed message, headers consumed.
    fn recv(&mut self) -> serde_json::Value {
        let mut length = 0;
        loop {
            let mut header = String::new();
            self.output.read_line(&mut header).expect("read header");
            assert!(!header.is_empty(), "server closed the connection early");
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length: ") {
                length = value.trim().parse().expect("length");
            }
        }
        let mut body = vec![0; length];
        self.output.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).expect("parse body")
    }

    /// Messages until one satisfies `wanted` — notifications the server sends
    /// on its own arrive interleaved with responses.
    fn recv_until(&mut self, wanted: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        for _ in 0..16 {
            let message = self.recv();
            if wanted(&message) {
                return message;
            }
        }
        panic!("no matching message in 16 messages");
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {"uri": uri, "languageId": "tcl", "version": 1, "text": text}
            }
        }));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Bring a server up to the point where it is serving documents.
fn initialized() -> Client {
    let mut client = Client::start();
    client.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"processId": null, "rootUri": null, "capabilities": {}}
    }));
    let response = client.recv_until(|m| m["id"] == 1);
    let capabilities = &response["result"]["capabilities"];
    assert_eq!(capabilities["hoverProvider"], true, "{response}");
    assert!(capabilities["completionProvider"].is_object(), "{response}");
    assert_eq!(capabilities["documentSymbolProvider"], true, "{response}");
    client.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));
    client
}

#[test]
fn a_session_initializes_answers_and_shuts_down() {
    let mut client = initialized();
    client.open(
        "file:///t.tcl",
        "proc double {x} {expr {$x * 2}}\nputs [double 21]\n",
    );

    // Diagnostics arrive without being asked for, and a script that runs has
    // none.
    let published = client.recv_until(|m| m["method"] == "textDocument/publishDiagnostics");
    assert_eq!(published["params"]["diagnostics"], serde_json::json!([]));

    // Hover on `puts`, which is on line 1.
    client.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/hover",
        "params": {
            "textDocument": {"uri": "file:///t.tcl"},
            "position": {"line": 1, "character": 1}
        }
    }));
    let hover = client.recv_until(|m| m["id"] == 2);
    let value = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    // tclsh 9.0.4's own wording, which names the channel argument.
    assert!(
        value.contains("puts ?-nonewline? ?channel? string"),
        "{hover}"
    );

    // The document's own procedure is a symbol.
    client.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/documentSymbol",
        "params": {"textDocument": {"uri": "file:///t.tcl"}}
    }));
    let symbols = client.recv_until(|m| m["id"] == 3);
    assert_eq!(symbols["result"][0]["name"], "double", "{symbols}");

    client.send(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}));
    let shutdown = client.recv_until(|m| m["id"] == 4);
    assert!(shutdown["error"].is_null(), "{shutdown}");
    client.send(serde_json::json!({"jsonrpc": "2.0", "method": "exit"}));
    client.close_input();

    let status = client.child.wait().expect("wait");
    assert!(status.success(), "server exited with {status:?}");
}

/// Editing a document republishes: the error appears when it is typed and goes
/// away when it is fixed.
#[test]
fn diagnostics_follow_the_document() {
    let mut client = initialized();
    client.open("file:///t.tcl", "puts {unclosed\n");

    let published = client.recv_until(|m| m["method"] == "textDocument/publishDiagnostics");
    let first = &published["params"]["diagnostics"][0];
    assert!(
        first["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing close-brace"),
        "{published}"
    );
    assert_eq!(first["source"], "tclrs");

    client.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {"uri": "file:///t.tcl", "version": 2},
            "contentChanges": [{"text": "puts {closed}\n"}]
        }
    }));
    let published = client.recv_until(|m| m["method"] == "textDocument/publishDiagnostics");
    assert_eq!(published["params"]["diagnostics"], serde_json::json!([]));
}

/// Nothing but the protocol on stdout: a stray `println!` anywhere in the
/// server would be read as a malformed message by every editor.
#[test]
fn the_server_writes_only_framed_messages() {
    let mut client = initialized();
    client.open("file:///t.tcl", "puts hi\n");
    let published = client.recv_until(|m| m["method"] == "textDocument/publishDiagnostics");
    assert_eq!(published["jsonrpc"], "2.0");
}
