//! Stable-Rust smoke test for the cargo-fuzz targets.
//!
//! cargo-fuzz needs nightly. These tests replay each target's logic on its
//! committed seed corpus under stable, so a regression in the fuzz scaffolding —
//! a seed that stopped parsing, a target that no longer builds against the
//! library's API — is caught by `cargo test` rather than the next time somebody
//! runs the fuzzer.
//!
//! The assertion is only that the call does not panic. What a given input
//! parses or compiles to is not constrained here; that is what the differential
//! suites are for.
//!
//! When adding a target under `fuzz/fuzz_targets/`, add its
//! `<target>_corpus_does_not_panic` test here.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

fn corpus(target: &str) -> Vec<(PathBuf, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("corpus")
        .join(target);
    let mut entries: Vec<(PathBuf, String)> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .map(|p| {
            let text =
                fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            (p, text)
        })
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "fuzz/corpus/{target} must have seed files"
    );
    entries
}

/// Run `f` over every seed and report every input that panicked, rather than
/// dying on the first one.
fn no_panics(target: &str, f: impl Fn(&str)) {
    let mut panicked = Vec::new();
    for (path, text) in corpus(target) {
        // The seeds are text this crate should survive whatever it thinks of
        // them; a panic hook is not installed, so a panic here prints its own
        // message and is collected as a failure.
        if catch_unwind(AssertUnwindSafe(|| f(&text))).is_err() {
            panicked.push(path);
        }
    }
    assert!(
        panicked.is_empty(),
        "{} seed(s) panicked: {:?}",
        panicked.len(),
        panicked
    );
}

#[test]
fn parse_corpus_does_not_panic() {
    no_panics("parse", |src| {
        let _ = tclrs::parse(src);
    });
}

#[test]
fn compile_corpus_does_not_panic() {
    no_panics("compile", |src| {
        let _ = tclrs::runtime::compile(src);
    });
    // The parse seeds go through the compiler too: an input the parser accepts
    // is exactly what the compiler has to survive.
    no_panics("parse", |src| {
        let _ = tclrs::runtime::compile(src);
    });
}

/// The inputs a grammar-driven generator never produces: bytes, not programs.
/// This is the overlap between the two fuzzers, and the reason the cargo-fuzz
/// targets exist alongside the differential one.
///
/// Run on a thread of [`tclrs::runtime::RECOMMENDED_STACK`], because the
/// parser's recursion is proportional to the input's nesting and that is the
/// stack the crate documents a host must give it — the `tclrs` binary spawns
/// exactly this thread (`src/main.rs`). The depths below are measured, not
/// guessed: on a 2 MiB thread the bracket case aborts between 400 and 800
/// levels, and on an 8 MiB one between 1600 and 3200 (BUGS.md, "input nesting
/// is bounded only by the stack"). A stack overflow is not a catchable panic —
/// it aborts the process — so a regression here takes the whole test binary
/// down rather than reporting a failure. That is the loudest available signal.
#[test]
fn hostile_inputs_do_not_panic() {
    std::thread::Builder::new()
        .stack_size(tclrs::runtime::RECOMMENDED_STACK)
        .spawn(hostile_inputs)
        .expect("spawn")
        .join()
        .expect("the hostile-input sweep panicked");
}

fn hostile_inputs() {
    let cases: Vec<String> = vec![
        String::new(),
        "\0".to_string(),
        "puts \\u{".to_string(),
        "puts \\x".to_string(),
        "puts \\".to_string(),
        "\"".to_string(),
        "[".to_string(),
        "]".to_string(),
        "}".to_string(),
        "$".to_string(),
        "${".to_string(),
        "set a( ".to_string(),
        "puts $a(".to_string(),
        "#".to_string(),
        ";;;".to_string(),
        "\u{1e}\u{7f}\u{feff}".to_string(),
        // Depth: the parser and the compiler both recurse on nesting.
        "{".repeat(2_000),
        "[".repeat(2_000),
        format!("puts {}", "[expr {".repeat(200)),
        format!("if {{1}} {}", "{if {1} ".repeat(200)),
        // A word made only of substitutions.
        "$a$b$c[list][list]".to_string(),
        // Very long single word.
        format!("puts {}", "a".repeat(100_000)),
    ];
    for src in cases {
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            let _ = tclrs::parse(&src);
        }));
        assert!(
            parsed.is_ok(),
            "parse panicked on {:?}",
            &src[..src.len().min(40)]
        );
        let compiled = catch_unwind(AssertUnwindSafe(|| {
            let _ = tclrs::runtime::compile(&src);
        }));
        assert!(
            compiled.is_ok(),
            "compile panicked on {:?}",
            &src[..src.len().min(40)]
        );
    }
}
