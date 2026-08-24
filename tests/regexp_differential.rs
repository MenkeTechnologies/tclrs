//! Differential execution for `regexp` and `regsub`.
//!
//! Same contract as the other differential suites: every program is run by both
//! tclsh and tclrs and the two outputs are compared byte for byte, so no
//! expectation about matching, indices or substitution is written by hand here.
//!
//! That matters more for regular expressions than for anything else in this
//! crate, because the engine underneath is *not* the one Tcl uses. tclsh
//! matches with Henry Spencer's ARE; this frontend translates onto the `regex`
//! crate. Two of the defaults differ silently — a pattern that compiles under
//! both and means something else — and only the reference interpreter settles
//! which is right:
//!
//! * `.` matches a newline in ARE and does not in Rust.
//! * `-line` is `-lineanchor` *and* `-linestop`, so it moves both `^`/`$` and
//!   what `.` will cross.
//!
//! `empty_match_iteration_matches_tclsh` pins the third one, which is not a
//! default but a loop: where an empty match leaves the cursor, and whether the
//! position at the very end of the subject is one that matches.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Matching, the switches, and the two commands' return values.
const PROGRAMS: &[&str] = &[
    // The return value is 1/0, a count under -all, and the text under -inline.
    "puts [regexp {b} abc]",
    "puts [regexp {z} abc]",
    "puts [regexp {} abc]",
    "puts [regexp -all {a} aaa]",
    "puts [regexp -inline {(a)(b)} xab]",
    "puts <[regexp -inline {z} abc]>",
    "puts [regexp -all -inline {(a)(b)} abab]",
    "puts [regexp -all -inline {\\d+} \"a12b345\"]",
    // Match variables, including the ones that do not participate.
    "set m {}\nregexp {b} abc m\nputs $m",
    "set a {}\nset b {}\nregexp {(a)(b)} ab a b\nputs \"$a|$b\"",
    "set a {}\nset b {}\nset c {}\nregexp {(a)} a a b c\nputs \"$a|$b|$c\"",
    "set a {}\nset b {}\nset c {}\nregexp {(a)(z)?} a a b c\nputs \"$a|$b|$c\"",
    "set m {}\nregexp -all {a} aaa m\nputs $m",
    "set m {}\nputs [regexp {z} abc m]\nputs <$m>",
    // Indices are character offsets, not byte offsets, and an unmatched group
    // is -1 -1.
    "set m {}\nregexp -indices {b} abc m\nputs $m",
    "set m {}\nregexp -indices {b} \u{e9}b m\nputs $m",
    "set a {}\nset b {}\nset c {}\nregexp -indices {(a)(z)?} a a b c\nputs [list $a $b $c]",
    "puts [regexp -inline -indices {(b)} abc]",
    "set m {}\nregexp -indices {b*} ac m\nputs $m",
    // The newline defaults, which is where the two engines disagree by default.
    "puts [regexp {a.b} \"a\\nb\"]",
    "puts [regexp -line {a.b} \"a\\nb\"]",
    "puts [regexp -linestop {a.b} \"a\\nb\"]",
    "puts [regexp -lineanchor {a.b} \"a\\nb\"]",
    "puts [regexp {^b} \"a\\nb\"]",
    "puts [regexp -line {^b} \"a\\nb\"]",
    "puts [regexp -lineanchor {^b} \"a\\nb\"]",
    "puts [regexp {a$} \"a\\nb\"]",
    "puts [regexp -line {a$} \"a\\nb\"]",
    "puts [regexp -all -line {^a} \"a\\na\"]",
    // -start, whose offset does not move where an anchor thinks the string
    // begins.
    "puts [regexp -start 1 {^b} ab]",
    "puts [regexp -start 1 {b} ab]",
    "puts [regexp -start -3 {a} ab]",
    "puts [regexp -start 5 {a} ab]",
    "set m {}\nregexp -start 1 -indices {b} ab m\nputs $m",
    "puts [regsub -start 1 -all {a} aaa X]",
    // The rest of the switches.
    "puts [regexp -nocase {ABC} abc]",
    "set m {}\nregexp -nocase -indices {B} ab m\nputs $m",
    "puts [regexp -expanded { a \\# b } ab]",
    "puts [regexp -- {-x} -x]",
    "puts [regexp {a{2,3}} aaa]",
    // ARE spellings that translate rather than pass through.
    "puts [regexp {\\yfoo\\y} \"a foo b\"]",
    "puts [regexp {[[:digit:]]+} ab123]",
    "puts [regexp {(?i)ABC} abc]",
    "puts [regexp {\\Aab\\Z} ab]",
    "puts [regexp {\\d+} ab123]",
    "puts [regexp {\\w+} !!abc]",
    "puts [regexp {\\s} \"a b\"]",
    // The directors, which are ARE-only.
    "puts [regexp {***=a.b} {a.b}]",
    "puts [regexp {***=a.b} {axb}]",
    "puts [regexp {***:a+} aaa]",
    // regsub: the return value, the variable form, and the replacement spec.
    "puts [regsub {b+} abbc {[&]}]",
    "puts [regsub {(b+)} abbc {<\\1>}]",
    "puts [regsub -all {b} abbc X]",
    "set v {}\nputs [regsub -all {a} aaa X v]\nputs $v",
    "set v {}\nputs [regsub {z} abc X v]\nputs <$v>",
    "puts [regsub {b} abc {\\&}]",
    "puts [regsub {b+} abbc {[\\0]}]",
    "puts [regsub {(a)(z)?} a {<\\1|\\2>}]",
    "puts [regsub -all {,} a,b,c {;}]",
    "puts [regsub -nocase {B} abc X]",
    "puts [regsub {b} abc {\\\\}]",
    // A subject and a pattern that are values rather than literals.
    "set p {b+}\nset s abbc\nputs [regexp $p $s]",
    "set p {b+}\nset s abbc\nputs [regsub $p $s X]",
    // The commands that take a regular expression without being one.
    "puts [switch -regexp abc {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp bcd {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp zzz {^a {list one} ^b {list two} default {list none}}]",
    "puts [switch -regexp -- abc {b+ {list hit} default {list miss}}]",
    "puts [switch -regexp -nocase ABC {^a {list ci} default {list no}}]",
    "puts [switch -regexp abc {{^[abc]+$} {list class} default {list no}}]",
    "puts [lsearch -regexp {abc bcd} {^b}]",
    "puts [lsearch -all -regexp {abc bcd cde} {c}]",
    "puts [lsearch -regexp {abc bcd} {^z}]",
    // `regsub -command`: the third word is a command prefix, called once per
    // match with the whole match and every subexpression appended. Its result
    // is the replacement verbatim, so `&` and `\1` are ordinary characters in
    // it — the second program below is what proves that.
    "proc up {args} {return [string toupper [lindex $args 0]]}\n\
     puts [regsub -command {a(.)} banana up]",
    "proc amp {m} {return {&\\1}}\nputs [regsub -command {a} banana amp]",
    "proc up {args} {return [string toupper [lindex $args 0]]}\n\
     puts [regsub -all -command {a(.)} banana up]",
    "proc up {args} {return [string toupper [lindex $args 0]]}\n\
     puts [regsub -command {x} banana up]",
    // An empty match is a call too, and an empty result substitutes nothing.
    "proc up {args} {return [string toupper [lindex $args 0]]}\n\
     puts [regsub -all -command {b*} abc up]",
    "proc up {args} {return [string toupper [lindex $args 0]]}\n\
     puts [regsub -command {a} {} up]",
    // A group that did not participate arrives as an empty argument, which is
    // the shape of the whole argument list this pins.
    "proc show {args} {return <[join $args -]>}\n\
     puts [regsub -command {(x)?(b)} abc show]",
    // The prefix is a *list*: its own words come first and the match after.
    "puts [regsub -command {a} abc {string toupper}]",
    "puts [regsub -all -command {a} banana {list x}]",
    // With a variable name the answer is the count, and the string is written
    // there — the same split the template form has.
    "proc up {m} {return [string toupper $m]}\n\
     set n [regsub -all -command {a} banana up out]\nputs \"$n $out\"",
    // The calls run in order and against the interpreter's own variables, so a
    // command that counts sees each match once.
    "set s 0\nproc bump {m} {global s\nincr s\nreturn $s}\n\
     puts [regsub -all -command {a} aaa bump]\nputs $s",
    "proc r {m} {return $m}\nputs [regsub -start 2 -all -command {a} aaaa r]",
    // `switch -matchvar` / `-indexvar`: what matched and where, per clause.
    "puts [switch -matchvar m -regexp abc {{(b)(c)} {set m}}]",
    "puts [switch -indexvar i -regexp abc {{(b)(c)} {set i}}]",
    "puts [switch -matchvar m -indexvar i -regexp abc {b {list $m $i}}]",
    "puts [switch -matchvar m -regexp abc {{(x)?(b)} {set m}}]",
    "puts [switch -indexvar i -regexp abc {{(x)?(b)} {set i}}]",
    // The `default` clause ran no pattern, so tclsh empties both rather than
    // leaving what the script put there.
    "set m PRE\nputs <[switch -matchvar m -regexp abc {z {list} default {set m}}]>",
    "set i PRE\nputs <[switch -indexvar i -regexp abc {z {list} default {set i}}]>",
    // Nothing matched and there was no `default`: both keep their old values.
    "set m PRE\nset i PRE\nswitch -matchvar m -indexvar i -regexp zzz {a {}}\n\
     puts \"$m $i\"",
    // A `-` body shares the *next* clause's, and the pattern that matched is
    // the one reported.
    "set m {}\nswitch -matchvar m -regexp abc {x - b {}}\nputs $m",
    // Character offsets, and the rule that is `switch`'s own: an empty match
    // whose end is 0 is `-1 -1` here, where `regexp -indices` says `0 -1`.
    "set i {}\nswitch -indexvar i -regexp abc {{} {}}\nputs $i",
    "puts [regexp -indices -inline {} abc]",
    "set i {}\nswitch -indexvar i -regexp abc {{c*} {}}\nputs $i",
    "set i {}\nswitch -indexvar i -regexp abc {{x*$} {}}\nputs $i",
    "set i {}\nswitch -indexvar i -regexp \u{e9}llo {{(l)(l)} {}}\nputs $i",
    "set m {}\nswitch -matchvar m -regexp \u{e9}llo {l+ {}}\nputs $m",
    "set i {}\nswitch -indexvar i -nocase -regexp ABC {b {}}\nputs $i",
    // Inside a procedure the two variables are frame slots.
    "proc p {} {switch -matchvar m -regexp abc {b {return $m}}}\nputs [p]",
    // ARE reads a `{` that does not begin a bound as an ordinary character,
    // where `regex` reads three of these four as malformed repetitions and the
    // fourth as a two-fold one.
    "puts [regexp -- \"a\\{\" \"a\\{\"]",
    "puts [regexp -- {a{,2}} \"a{,2}\"]",
    "puts [regexp -- {a{x}} \"a{x}\"]",
    "puts [regexp -- {a{ 2}} \"a{ 2}\"]",
    "puts [regexp -inline -- {a{2,3}} aaaa]",
    "puts [regexp -inline -- {a{2,}} aaaa]",
    "puts [regexp -inline -- {a{2}} aaaa]",
];

/// Programs whose *error* must agree with the interpreter's, message included.
const ERRORS: &[&str] = &[
    "puts [catch {regexp -bogus {a} b} e]\nputs $e",
    "puts [catch {regsub -inline {a} abc X} e]\nputs $e",
    "puts [catch {regsub -bogus {a} b X} e]\nputs $e",
    // `-about` is a `regexp` option and not a `regsub` one, so `regsub -about`
    // really is a bad option and its wording is the reference implementation's.
    // The `regexp` half cannot be here — tclsh answers it — and is pinned as a
    // named refusal by `unsupported_are_constructs_are_refused`.
    "puts [catch {regsub -about {a} b X} e]\nputs $e",
    // `-command`'s prefix has to be a list of at least one element, and a
    // failure inside the call is the command's failure.
    "puts [catch {regsub -command {a} abc {}} e]\nputs $e",
    "puts [catch {regsub -command {a} abc nosuchcmd} e]\nputs $e",
    "proc bad {m} {error boom}\nputs [catch {regsub -command {a} abc bad} e]\nputs $e",
    // Both `switch` variables are filled from capture information, so neither
    // means anything without `-regexp`; `-indexvar` is tested first.
    "puts [catch {switch -matchvar m -glob abc {a* {}}} e]\nputs $e",
    "puts [catch {switch -indexvar i -exact abc {abc {}}} e]\nputs $e",
    "puts [catch {switch -matchvar m -indexvar i -glob x {a b}} e]\nputs $e",
    "puts [catch {switch -indexvar i -matchvar m -glob x {a b}} e]\nputs $e",
    "puts [catch {switch -matchvar} e]\nputs $e",
    "puts [catch {switch -indexvar} e]\nputs $e",
    // The variables are untouched when the pattern will not compile.
    "set m PRE\nputs [catch {switch -matchvar m -regexp abc {{a[} {}}} e]\nputs $e\nputs $m",
    // `Tcl_SwitchObjCmd` reaches these while running, so a `switch` that is
    // never executed costs a script nothing and `catch` answers 1.
    "puts [catch {switch -- x {a}} e]\nputs $e",
    "if {0} {switch -- x {a}}\nputs reached",
    "puts [catch {switch -bogus x {a b}} e]\nputs $e",
    "if {0} {switch -bogus x {a b}}\nputs reached",
    "puts [catch {switch -- x {a - }} e]\nputs $e",
    "puts [catch {switch -- x \"a \\{b\"} e]\nputs $e",
    // The reference interpreter's own name for a rejected pattern, which is
    // the construct rather than the parse state the engine was in.
    "puts [catch {regexp -- {a[} x} e]\nputs $e",
    "puts [catch {regexp -- {(a} x} e]\nputs $e",
    "puts [catch {regexp -- {a)} x} e]\nputs $e",
    "puts [catch {regexp -- {*} x} e]\nputs $e",
    "puts [catch {regexp -- {a{2,1}} x} e]\nputs $e",
    "puts [catch {regexp -- {[z-a]} x} e]\nputs $e",
    "puts [catch {regexp -- \"a\\{1,\" x} e]\nputs $e",
    // One quantifier per atom, plus a `?` on it meaning non-greedy. A second
    // is `invalid quantifier operand` — which `regex` accepts with a different
    // meaning, so these are wrong answers rather than missing errors.
    "puts [catch {regexp -- {a**} aaa} e]\nputs $e",
    "puts [catch {regexp -- {a?*} aaa} e]\nputs $e",
    "puts [catch {regexp -- {a{2}{3}} aaa} e]\nputs $e",
    "puts [catch {regexp -- {a*{2}} aaa} e]\nputs $e",
    "puts [catch {regexp -- {a*??} aaa} e]\nputs $e",
    // ... and the shapes that stay legal, so the rule does not over-reach.
    "puts [catch {regexp -- {a*?} aaa} e]\nputs $e",
    "puts [catch {regexp -- {a{2}?} aaa} e]\nputs $e",
    "puts [catch {regexp -- {(a*)*} aaa} e]\nputs $e",
    "puts [catch {regexp -- {[*]*} {*}} e]\nputs $e",
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

/// What tclsh prints for a program, run from a file so the shell never sees it.
fn reference(tclsh: &PathBuf, program: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tclrs-regexp-{}-{n}.tcl", std::process::id()));
    std::fs::write(&path, program).expect("write program");
    let out = Command::new(tclsh).arg(&path).output().expect("run tclsh");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "tclsh rejected program:\n{program}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn compare_all(programs: &[&str], what: &str) {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in programs {
        let expected = reference(&tclsh, program);
        match tclrs::eval(program) {
            Ok(outcome) if outcome.output == expected => {}
            Ok(outcome) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs: {:?}",
                outcome.output
            )),
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected:?}\n  tclrs failed: {e}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} {what} programs diverge:\n\n{}",
        failures.len(),
        programs.len(),
        failures.join("\n\n")
    );
}

#[test]
fn regexp_matches_tclsh() {
    compare_all(PROGRAMS, "regexp");
}

#[test]
fn regexp_errors_match_tclsh() {
    compare_all(ERRORS, "error");
}

/// Where an empty match leaves the cursor, and whether the end of the subject
/// is a position that matches.
///
/// The two commands disagree with each other in tclsh — `regexp -all {x*} ab`
/// is 2 while `regsub -all {x*} ab -` substitutes three times — and the
/// literally empty pattern disagrees with every other pattern that can match
/// empty. `(?:)`, `()` and `a{0}` all behave like `x*`; only `{}` does not.
/// None of that is derivable, so it is measured.
#[test]
fn empty_match_iteration_matches_tclsh() {
    let mut programs: Vec<String> = Vec::new();
    for pattern in ["", "x*", "z*", "(?:)", "()", "a{0}", "b*", "a*"] {
        for subject in ["", "a", "ab", "abc", "aab"] {
            programs.push(format!(
                "puts [regexp -all {{{pattern}}} \"{subject}\"]\n\
                 puts [regsub -all {{{pattern}}} \"{subject}\" -]\n\
                 puts <[regexp -all -inline {{{pattern}}} \"{subject}\"]>\n"
            ));
        }
    }
    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    compare_all(&refs, "empty-match");
}

/// Multi-byte subjects, where a byte offset and a character offset differ and
/// an empty-match step of one byte would land inside a character.
#[test]
fn character_offsets_match_tclsh() {
    let mut programs: Vec<String> = Vec::new();
    for subject in ["\u{e9}b", "a\u{e9}b", "\u{1f600}b", "\u{65e5}\u{672c}b"] {
        programs.push(format!(
            "set m {{}}\nregexp -indices {{b}} \"{subject}\" m\nputs $m\n\
             puts [regexp -all {{x*}} \"{subject}\"]\n\
             puts [regsub -all {{x*}} \"{subject}\" -]\n\
             puts [string length [regsub -all {{}} \"{subject}\" -]]\n"
        ));
    }
    let refs: Vec<&str> = programs.iter().map(String::as_str).collect();
    compare_all(&refs, "character-offset");
}

/// What this frontend will not approximate must say so, and must say it at the
/// point of use rather than matching something wrong.
///
/// These are the constructs a finite-automaton engine cannot express. tclsh
/// accepts all of them, so there is no reference wording to copy — what is
/// pinned here is that the refusal happens and names the construct.
#[test]
fn unsupported_are_constructs_are_refused() {
    for (program, expected) in [
        ("puts [regexp {(a+)\\1} aaaa]", "back-reference"),
        ("puts [regexp {a(?=b)} ab]", "look-ahead"),
        ("puts [regexp {a(?!b)} ac]", "look-ahead"),
        ("puts [regexp {\\mfoo} \"a foo\"]", "word-start"),
        ("puts [regexp {foo\\M} \"foo b\"]", "word-end"),
        ("puts [regexp {[[.hyphen-minus.]]} -]", "collating element"),
        ("puts [regexp {[[=a=]]} a]", "equivalence class"),
        // Not a construct but an option, and refused for a related reason: its
        // second element is the reference engine's report on its own compile.
        // Named rather than reported as a bad option, which is what it was —
        // and `bad option "-about": must be … -about …` contradicts itself.
        ("puts [regexp -about {(a)}]", "regexp -about is not supported yet"),
    ] {
        let err = tclrs::eval(program)
            .map(|o| format!("no error, printed {:?}", o.output))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(expected),
            "expected a refusal naming {expected:?} for {program}, got: {err}"
        );
    }
}

/// A pattern reused across calls must answer as if it had been compiled afresh
/// each time.
///
/// `src/regexp.rs` keeps compiled patterns in a per-thread map so that a
/// `regexp` in a loop compiles once — the entry is an `Arc<Regex>`, shared
/// rather than cloned, because a clone of a `regex::Regex` carries an empty
/// pool of match caches and the lazy DFA would be rebuilt on every call. Three
/// things that arrangement can get wrong, each pinned below:
///
/// * **Per-match state leaking between calls.** Every program repeats a pattern
///   over several subjects, in an order where a shared cursor, a shared capture
///   set or a leftover anchor would show as a wrong answer rather than an error.
/// * **A key that ignores the flags.** The same pattern text is matched with
///   and without `-nocase` and `-line` in one program: an entry found by text
///   alone answers the second with the first one's engine.
/// * **A refusal remembered as a success, or a success as a refusal.** A
///   pattern that will not compile is used repeatedly, alternating with one that
///   does, so a cache that stored the failure — or that stopped raising after
///   the first call — is visible.
#[test]
fn a_reused_pattern_answers_as_a_fresh_one_would() {
    compare_all(
        &[
            // The same pattern over many subjects, captures included.
            "set out {}\nforeach s {abc1 xyz22 q 333 a0} {\n  if {[regexp {^([a-z]+)([0-9]+)$} $s m a b]} {\n    append out \"$m|$a|$b \"\n  } else {\n    append out \"-|$s \"\n  }\n}\nputs $out",
            // Anchors and `-all`, whose iteration keeps a cursor.
            "set out {}\nforeach s {aaa {} bab aXa} {\n  append out [regexp -all {a} $s],\n  append out [regexp -all -inline {a.} $s],\n}\nputs $out",
            // One pattern text, three different option sets.
            "set p {^ab}\nset s \"xy\\nABc\"\nputs [regexp $p $s][regexp -nocase $p $s][regexp -line $p $s][regexp -line -nocase $p $s][regexp $p ab]",
            // The same, through regsub, which compiles by the same route.
            "set p {a+}\nputs [regsub -all $p aaabaa X][regsub $p aaabaa X][regsub -all -nocase $p AaAbaa X]",
            // `switch -regexp` and `lsearch -regexp` reach the same cache.
            "set out {}\nforeach s {a1 b2 c3} {\n  switch -regexp -- $s {\n    {^a} {append out A}\n    {^[bc]} {append out B}\n    default {append out ?}\n  }\n}\nappend out [lsearch -regexp {a1 b2 c3} {^b}][lsearch -regexp {a1 b2 c3} {^a}]\nputs $out",
            // A pattern that cannot compile, used repeatedly and interleaved
            // with one that can.
            "for {set i 0} {$i < 4} {incr i} {\n  puts [catch {regexp {a(} x} m]:$m\n  puts [regexp {a+} aa]\n}",
            "for {set i 0} {$i < 3} {incr i} {\n  puts [catch {regexp {a[} x} m]:$m\n  puts [catch {regexp {a{2,1}} x} n]:$n\n  puts [regexp {b} ab]\n}",
            // Enough distinct patterns in one script that the map is doing real
            // work, each answered twice so a wrong entry shows on the repeat.
            "set ps {a b {a|b} {[ab]} {a*} {a+} {^a} {a$} {(a)(b)} {a{2}} {\\d} {[[:alpha:]]} {(?i)A} {a.c}}\nset out {}\nforeach p $ps { append out [regexp -- $p abc] }\nappend out |\nforeach p $ps { append out [regexp -- $p abc] }\nputs $out",
        ],
        "reused pattern",
    );
}
