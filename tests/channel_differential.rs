//! Differential execution for the channel commands: `open`, `close`, `gets`,
//! `read`, `puts` to a channel, `flush`, `eof`, `seek`, `tell` and
//! `fconfigure`.
//!
//! Same contract as the other harnesses here: every program is run by both
//! tclsh 9.0.4 and tclrs and the two outputs are compared byte for byte, so no
//! expectation is written by hand. That matters more for channels than for
//! most of this crate, because almost every rule below is one the manual pages
//! state loosely or not at all:
//!
//! * `eof` is not "the position is at the end of the file". It is 1 only once
//!   a read has *asked* the device for more and been told there is none, so a
//!   file ending in a newline reads its last line with `eof` still 0.
//! * `gets` on a final line with no terminator answers the line and the
//!   character count, and only *then* is `eof` 1.
//! * `seek` clears that condition, so `seek $f 0` after reading to the end
//!   makes the whole file readable again.
//! * `tell` is the device's position less whatever was read ahead, which is
//!   why it answers 9 after `gets` on a 9-byte first line rather than the
//!   buffer size.
//! * `open ... a` positions at the end of the file immediately, so `tell` on a
//!   freshly opened append channel is the file's size and not 0
//!   (`generic/tclIOUtil.c:2232`). `a+` is `O_RDWR` *without* `O_APPEND`
//!   (`:1494-1501`).
//! * `-translation binary` is not a translation. It is `lf` plus an encoding
//!   change to `iso8859-1` (`generic/tclIO.c:8389-8393`, `:8439-8442`).
//! * `-translation auto` on input accepts all three line endings and answers
//!   `\n` for each; on *output* it is not a mode at all and means the
//!   platform's (`:8427-8438`).
//! * `fconfigure $f` with no option answers seven options in
//!   `Tcl_GetChannelOption`'s own order, `-blocking` first.
//! * `puts stdout` and `puts` reach the same place in the same order, which is
//!   the one thing that would break silently if the channel layer had its own
//!   buffer in front of the interpreter's output.
//!
//! Every program writes to a file under the process's own temporary directory
//! and removes it, so two runs of the suite cannot collide. None of them
//! prints a channel *name*: a name is `file` plus the file descriptor number
//! (`unix/tclUnixChan.c:1845`), and tclsh and tclrs do not have the same
//! descriptors free, so the names legitimately differ.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Programs whose output must agree, byte for byte.
///
/// `%F` is replaced with a scratch path unique to the program, so the same
/// text is handed to both interpreters and neither can see the other's file.
const PROGRAMS: &[&str] = &[
    // ── writing, then reading back ───────────────────────────────────────
    "set f [open %F w]\nputs $f {line one}\nputs $f {line two}\nclose $f\nset g [open %F]\nputs <[read $g]>\nclose $g",
    "set f [open %F w]\nputs -nonewline $f abc\nclose $f\nset g [open %F]\nputs <[read $g]>\nclose $g",
    "set f [open %F w]\nputs $f {}\nclose $f\nset g [open %F]\nputs [string length [read $g]]\nclose $g",
    // `flush` before the channel is closed: the bytes have to be on disk.
    "set f [open %F w]\nputs $f hello\nflush $f\nset g [open %F]\nputs <[read $g]>\nclose $g\nclose $f",
    // ── gets ─────────────────────────────────────────────────────────────
    "set f [open %F w]\nputs $f one\nputs $f two\nclose $f\nset g [open %F]\nputs [gets $g]\nputs [gets $g]\nputs <[gets $g]>\nclose $g",
    "set f [open %F w]\nputs $f one\nputs $f two\nclose $f\nset g [open %F]\nwhile {[gets $g line] >= 0} {puts \"<$line>\"}\nclose $g",
    // A final line with no terminator: the count is the line's, and eof is 1
    // only after it has been handed over.
    "set f [open %F w]\nputs -nonewline $f abc\nclose $f\nset g [open %F]\nputs \"[gets $g v] <$v> [eof $g]\"\nputs \"[gets $g v] <$v> [eof $g]\"\nclose $g",
    // A file ending in a newline leaves eof at 0 until the read past it.
    "set f [open %F w]\nputs $f abc\nclose $f\nset g [open %F]\nputs \"[gets $g v] <$v> [eof $g]\"\nputs \"[gets $g v] <$v> [eof $g]\"\nclose $g",
    "set f [open %F w]\nputs $f {}\nputs $f x\nclose $f\nset g [open %F]\nputs \"[gets $g v] <$v>\"\nputs \"[gets $g v] <$v>\"\nclose $g",
    // The count is in characters, not bytes.
    "set f [open %F w]\nputs $f \"h\\u00e9llo \\u2603\"\nclose $f\nset g [open %F]\nputs [gets $g v]\nputs [string length $v]\nclose $g",
    // ── read ─────────────────────────────────────────────────────────────
    "set f [open %F w]\nputs -nonewline $f 0123456789\nclose $f\nset g [open %F]\nputs <[read $g 4]>\nputs <[read $g 3]>\nputs <[read $g]>\nputs [eof $g]\nclose $g",
    "set f [open %F w]\nputs -nonewline $f abc\nclose $f\nset g [open %F]\nputs \"<[read $g 100]> [eof $g]\"\nclose $g",
    "set f [open %F w]\nputs -nonewline $f abc\nclose $f\nset g [open %F]\nputs \"<[read $g 0]> [eof $g]\"\nclose $g",
    "set f [open %F w]\nputs $f abc\nputs $f {}\nclose $f\nset g [open %F]\nputs <[read -nonewline $g]>\nclose $g",
    "set f [open %F w]\nputs $f abc\nclose $f\nset g [open %F]\nputs <[read -nonewline $g]>\nclose $g",
    // Reading an empty file.
    "set f [open %F w]\nclose $f\nset g [open %F]\nputs \"<[read $g]> [eof $g]\"\nclose $g",
    // ── seek and tell ────────────────────────────────────────────────────
    "set f [open %F w]\nputs -nonewline $f 0123456789\nclose $f\nset g [open %F]\nputs [tell $g]\nseek $g 3\nputs [tell $g]\nseek $g 2 current\nputs [tell $g]\nseek $g -1 end\nputs \"[tell $g] <[read $g]>\"\nclose $g",
    // A seek clears end of file, so the whole file reads again.
    "set f [open %F w]\nputs -nonewline $f abcdef\nclose $f\nset g [open %F]\nputs \"<[read $g]> [eof $g]\"\nseek $g 0\nputs \"<[read $g]> [eof $g]\"\nclose $g",
    // tell after a gets is the file position, not the buffer's.
    "set f [open %F w]\nputs $f {line one}\nputs $f {line two}\nclose $f\nset g [open %F]\ngets $g\nputs [tell $g]\ngets $g\nputs [tell $g]\nclose $g",
    "set f [open %F w]\nputs -nonewline $f 0123456789\nclose $f\nset g [open %F]\nseek $g 0 end\nputs [tell $g]\nclose $g",
    // ── access modes ─────────────────────────────────────────────────────
    "set f [open %F w]\nputs -nonewline $f ABCDEF\nclose $f\nforeach m {r r+ w+ a a+} {\n  set g [open %F $m]\n  puts \"$m [tell $g]\"\n  close $g\n}",
    "set f [open %F w]\nputs $f one\nclose $f\nset f [open %F a]\nputs $f two\nclose $f\nset g [open %F]\nputs <[read $g]>\nclose $g",
    "set f [open %F w]\nputs $f one\nclose $f\nset f [open %F w]\nputs $f two\nclose $f\nset g [open %F]\nputs <[read $g]>\nclose $g",
    // r+ writes in place without truncating.
    "set f [open %F w]\nputs -nonewline $f ABCDEF\nclose $f\nset f [open %F r+]\nseek $f 2\nputs -nonewline $f xy\nclose $f\nset g [open %F]\nputs <[read $g]>\nclose $g",
    // ── translation ──────────────────────────────────────────────────────
    "set f [open %F w]\nfconfigure $f -translation crlf\nputs $f x\nputs $f y\nclose $f\nset g [open %F]\nfconfigure $g -translation binary\nputs [string length [read $g]]\nclose $g",
    "set f [open %F w]\nfconfigure $f -translation cr\nputs $f x\nclose $f\nset g [open %F]\nfconfigure $g -translation binary\nset t [read $g]\nputs [string length $t]\nputs [string is space [string index $t 1]]\nclose $g",
    // `auto` on input answers \\n for all three endings.
    "set f [open %F w]\nfconfigure $f -translation binary\nputs -nonewline $f \"a\\rb\\r\\nc\\nd\"\nclose $f\nset g [open %F]\nwhile {[gets $g line] >= 0} {puts \"<$line>\"}\nclose $g",
    "set f [open %F w]\nfconfigure $f -translation binary\nputs -nonewline $f \"a\\r\\nb\"\nclose $f\nset g [open %F]\nfconfigure $g -translation lf\nputs [string length [read $g]]\nclose $g",
    "set f [open %F w]\nfconfigure $f -translation binary\nputs -nonewline $f \"a\\r\\nb\"\nclose $f\nset g [open %F]\nfconfigure $g -translation crlf\nputs <[gets $g]>\nputs <[gets $g]>\nclose $g",
    // `binary` is lf plus iso8859-1.
    "set f [open %F w]\nfconfigure $f -translation binary\nputs \"[fconfigure $f -translation] [fconfigure $f -encoding]\"\nclose $f",
    // A round trip through iso8859-1 is one byte per character.
    "set f [open %F w]\nfconfigure $f -translation binary\nputs -nonewline $f \"\\u00e9\\u00ff\"\nclose $f\nset g [open %F]\nfconfigure $g -translation binary\nputs [string length [read $g]]\nclose $g",
    // ── fconfigure ───────────────────────────────────────────────────────
    "set f [open %F w]\nputs [fconfigure $f]\nclose $f",
    "set f [open %F w]\nputs [fconfigure $f -buffering]\nputs [fconfigure $f -buffersize]\nputs [fconfigure $f -encoding]\nputs [fconfigure $f -blocking]\nputs [fconfigure $f -translation]\nclose $f",
    "set f [open %F w]\nclose $f\nset f [open %F r+]\nputs [fconfigure $f -translation]\nclose $f",
    "set f [open %F w]\nfconfigure $f -buffering none\nputs [fconfigure $f -buffering]\nfconfigure $f -buffering line\nputs [fconfigure $f -buffering]\nfconfigure $f -buffersize 8192\nputs [fconfigure $f -buffersize]\nclose $f",
    "set f [open %F w]\nfconfigure $f -translation lf -buffering none\nputs \"[fconfigure $f -translation] [fconfigure $f -buffering]\"\nclose $f",
    "puts [fconfigure stdout -translation]",
    "puts [fconfigure stdout -buffering]",
    "puts [fconfigure stdin -translation]",
    "puts [fconfigure stderr -buffering]",
    "puts [fconfigure stdout -encoding]",
    "puts [fconfigure stdout]",
    // ── puts to a channel ────────────────────────────────────────────────
    // The whole point of routing the standard channel through the
    // interpreter's own output: these must interleave in the order written.
    "puts a\nputs stdout b\nputs -nonewline stdout c\nputs d\nputs stdout e",
    "puts -nonewline stdout x\nputs -nonewline stdout y\nputs {}",
    // `puts` yields the empty string, like every other command that only acts.
    "puts <[puts stdout hi]>",
    // Buffering `none` on a file still reaches the file.
    "set f [open %F w]\nfconfigure $f -buffering none\nputs $f a\nset g [open %F]\nputs <[read $g]>\nclose $g\nclose $f",
    // A line-buffered channel flushes on the newline and not before.
    "set f [open %F w]\nfconfigure $f -buffering line\nputs -nonewline $f a\nset g [open %F]\nputs \"partial <[read $g]>\"\nclose $g\nputs $f {}\nset g [open %F]\nputs \"whole <[read $g]>\"\nclose $g\nclose $f",
    // ── close ────────────────────────────────────────────────────────────
    // `close $f r` on a channel that has only a read side is a plain close,
    // not a half-close.
    "set f [open %F w]\nclose $f\nset f [open %F]\nclose $f r\nputs ok",
    // ── inside a procedure, where the channel name is a frame slot ────────
    "proc w {p} {set f [open $p w]; puts $f body; close $f}\nproc r {p} {set f [open $p]; set t [read $f]; close $f; return $t}\nw %F\nputs <[r %F]>",
    "proc lines {p} {set f [open $p]; set n 0; while {[gets $f l] >= 0} {incr n}; close $f; return $n}\nset f [open %F w]\nputs $f a\nputs $f b\nputs $f c\nclose $f\nputs [lines %F]",
    // ── catch ────────────────────────────────────────────────────────────
    "puts [catch {gets nosuchchannel} e]\nputs $e",
    // The channel name inside the message is a file-descriptor number, so only
    // the completion code and the message's tail are comparable.
    "set f [open %F w]\nputs [catch {gets $f} e]\nputs [string range $e [string first {wasn} $e] end]\nclose $f",
    "puts [catch {open /nonexistent-directory-for-tclrs/x} e]\nputs $e",
];

/// Programs whose *error* must agree, first line for first line.
const ERRORS: &[&str] = &[
    "gets nosuchchannel",
    "read nosuchchannel",
    "eof nosuchchannel",
    "tell nosuchchannel",
    "flush nosuchchannel",
    "close nosuchchannel",
    "fconfigure nosuchchannel -buffering",
    "puts nosuchchannel hi",
    "seek nosuchchannel 0",
    // `seek stdin 0` is not here: whether it is refused depends on what stdin
    // is bound to, and a test harness does not control that.
    "open /nonexistent-directory-for-tclrs/x",
    "open %F zz",
    "gets",
    "gets stdin a b",
    "puts",
    "read",
    "tell",
    "eof",
    "seek stdin",
    "flush stdin",
    "read stdout",
    "gets stdout",
    "fconfigure stdout -bogus",
    "fconfigure stdout -translation bogus",
    "fconfigure stdout -buffering bogus",
    "set f [open %F]; read -nonewline $f 2",
    "set f [open %F]; close $f; gets $f",
    "set f [open %F]; close $f w",
    "puts stdout hi nonewline",
    "set f [open %F]; puts $f hi",
    "set f [open %F w]; gets $f",
];

fn tclsh() -> Option<PathBuf> {
    for name in ["tclsh", "tclsh9.0", "tclsh8.6"] {
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// A scratch path for one program, unique to this process and this run.
fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "tclrs-chan-{tag}-{}-{}.dat",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Run a program through tclsh, returning its stdout and the first line of any
/// error it reported. tclsh follows an error with a stack trace and tclrs does
/// not, so only the first line is comparable.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    let path = scratch("prog");
    let script = path.with_extension("tcl");
    std::fs::write(&script, program).expect("write program");
    let out = Command::new(tclsh)
        .arg(&script)
        .output()
        .expect("run tclsh");
    let _ = std::fs::remove_file(&script);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let error = (!out.status.success()).then(|| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    (stdout, error)
}

/// One program's text with `%F` bound to a fresh path, and that path, so the
/// caller can remove it afterwards.
fn bind(program: &str, tag: &str) -> (String, PathBuf) {
    let path = scratch(tag);
    let text = program.replace("%F", path.to_str().expect("utf-8 temporary path"));
    (text, path)
}

#[test]
fn channel_commands_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        // A fresh file for each interpreter: the two runs must not see each
        // other's bytes, or a program that only reads would pass by accident.
        let (reference_program, reference_path) = bind(program, "ref");
        let (subject_program, subject_path) = bind(program, "sub");
        let (expected, error) = reference(&tclsh, &reference_program);
        let _ = std::fs::remove_file(&reference_path);
        assert!(
            error.is_none(),
            "tclsh rejected a program that should run:\n{program}\n{}",
            error.unwrap_or_default()
        );
        match tclrs::eval(&subject_program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
        let _ = std::fs::remove_file(&subject_path);
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// A program tclsh refuses must be refused here too, in the same wording.
///
/// The one thing not compared is a channel *name* inside a message: it is the
/// file descriptor number, and the two interpreters do not have the same
/// descriptors free. `normalize` replaces it so the rest of the wording is
/// still checked.
#[test]
fn channel_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in ERRORS {
        let (reference_program, reference_path) = bind(program, "referr");
        let (subject_program, subject_path) = bind(program, "suberr");
        // A program that opens for reading needs the file to exist first.
        std::fs::write(&reference_path, b"seed\n").expect("seed");
        std::fs::write(&subject_path, b"seed\n").expect("seed");
        let (_, error) = reference(&tclsh, &reference_program);
        let _ = std::fs::remove_file(&reference_path);
        let Some(expected) = error else {
            panic!("tclsh accepted a program the test expects it to refuse:\n{program}");
        };
        let expected = normalize(&expected, &reference_path);
        match tclrs::eval(&subject_program) {
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh refused: {expected:?}\n  tclrs ran it: {:?}",
                outcome.output
            )),
            Err(e) => {
                let got = normalize(
                    e.to_string().lines().next().unwrap_or_default().trim(),
                    &subject_path,
                );
                if got != expected {
                    failures.push(format!(
                        "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {got:?}"
                    ));
                }
            }
        }
        let _ = std::fs::remove_file(&subject_path);
    }
    assert!(
        failures.is_empty(),
        "{} of {} errors diverge:\n\n{}",
        failures.len(),
        ERRORS.len(),
        failures.join("\n\n")
    );
}

/// Replace the three things that legitimately differ between the runs: the
/// scratch path, a `fileN` channel name whose N is a file descriptor number
/// (`unix/tclUnixChan.c:1845`), and the `(line N)` this crate appends to a
/// refusal it located while compiling — tclsh reports the same refusal from a
/// running command and has no line to give.
fn normalize(message: &str, path: &Path) -> String {
    let message = match message.rfind(" (line ") {
        Some(at) if message.ends_with(')') => &message[..at],
        _ => message,
    };
    let mut out = message.replace(path.to_str().unwrap_or_default(), "SCRATCH");
    while let Some(at) = out.find("\"file") {
        let rest = &out[at + 5..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || !rest[digits..].starts_with('"') {
            break;
        }
        out.replace_range(at..at + 5 + digits + 1, "\"fileN\"");
    }
    out
}
