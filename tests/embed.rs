//! `tclrs::run_program`: a script with in-memory stdin and captured stdout.

use tclrs::run_program;

/// `puts` with and without a channel, and a write through `chan puts` after
/// `fconfigure`, all land in the returned output.
#[test]
fn stdout_is_captured() {
    let src = r#"
        puts "hello"
        puts -nonewline stdout "a"
        fconfigure stdout -buffering full
        chan puts stdout "b"
        flush stdout
    "#;
    assert_eq!(run_program(src, "").unwrap(), "hello\nab\n");
}

/// `gets stdin` reads `input` line by line until end of file, and `read stdin`
/// takes the rest.
#[test]
fn stdin_is_the_input() {
    let src = r#"
        while {[gets stdin line] >= 0} { puts [string toupper $line] }
        puts [eof stdin]
    "#;
    assert_eq!(run_program(src, "ab\ncd\n").unwrap(), "AB\nCD\n1\n");
    let src = r#"gets stdin first; puts "$first|[read stdin]""#;
    assert_eq!(run_program(src, "1\n2\n3\n").unwrap(), "1|2\n3\n\n");
}

/// An uncaught `error`, and a script that does not parse, are `Err`.
#[test]
fn errors_are_err() {
    let e = run_program("puts before; error {went wrong}", "").unwrap_err();
    assert!(e.starts_with("went wrong"), "{e:?}");
    assert!(run_program("puts {unterminated", "").is_err());
}

/// `exit` ends the script without ending the process: status 0 keeps the
/// output so far, any other status is an error. `catch` cannot intercept it.
#[test]
fn exit_ends_the_script() {
    assert_eq!(run_program("puts a; exit; puts b", "").unwrap(), "a\n");
    assert_eq!(run_program("puts a; catch {exit 0}; puts b", "").unwrap(), "a\n");
    assert_eq!(run_program("puts a; exit 3", "").unwrap_err(), "exit status 3");
}

/// Concurrent runs each read their own input and return their own output.
#[test]
fn concurrent_runs_do_not_mix() {
    let runs: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                let input: String = (0..500).map(|n| format!("{i}-{n}\n")).collect();
                let src = "while {[gets stdin l] >= 0} { puts \"<$l>\" }";
                let got = run_program(src, &input).unwrap();
                let want: String = (0..500).map(|n| format!("<{i}-{n}>\n")).collect();
                assert_eq!(got, want, "run {i}");
            })
        })
        .collect();
    for r in runs {
        r.join().unwrap();
    }
}
