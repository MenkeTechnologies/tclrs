//! Differential execution of `namespace`, `variable` and `rename`.
//!
//! Same contract as `proc_differential.rs`: no expected value below is written
//! by hand. Every program is run by tclsh and by tclrs and the two stdouts are
//! compared byte for byte, so the resolution rules — which namespace an
//! unqualified variable belongs to, which procedure an unqualified call
//! reaches, what `namespace qualifiers ::foo::` answers — are checked against
//! the reference implementation rather than against a reading of
//! `generic/tclNamesp.c`.
//!
//! Two things the programs avoid, and each for a reason that is about the
//! comparison rather than about namespaces:
//!
//! * `namespace children ::` is never asked bare. A fresh tclsh already holds
//!   `::tcl`, `::oo` and `::zlib`, which are the namespaces of packages this
//!   frontend does not have; the programs ask about namespaces they created
//!   themselves;
//! * the order is `lsort`ed wherever a list of namespaces or commands is
//!   printed. `Tcl_GetNamespaceChildren` walks a hash table, so tclsh's order is
//!   its hash order — comparing it would pin an ordering Tcl does not promise.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROGRAMS: &[&str] = &[
    // ── the name grammar: qualifiers and tail over every shape ──
    "foreach n {::foo::bar foo::bar bar :: ::foo:: foo::bar:: {} a::b::c ::::x x:::y} {\n  puts \"[list $n] q=[list [namespace qualifiers $n]] t=[list [namespace tail $n]]\"\n}",
    // The same through the runtime path, since a computed argument cannot fold.
    "set n ::a::b::c\nputs \"[namespace qualifiers $n]|[namespace tail $n]\"\nset n bare\nputs \"<[namespace qualifiers $n]>|[namespace tail $n]\"",

    // ── current, and how it nests ──
    "puts [namespace current]",
    "namespace eval a {namespace eval b {namespace eval c {puts [namespace current]}}}",
    "namespace eval ::x::y::z {puts [namespace current]}\nputs [namespace exists ::x]\nputs [namespace exists ::x::y]\nputs [namespace exists ::x::y::z]",
    // A relative name inside a namespace is relative to it.
    "namespace eval a {namespace eval b {puts [namespace current]}\nputs [namespace current]}",

    // ── parent, children, exists ──
    "namespace eval a {namespace eval b {namespace eval c {}}}\nnamespace eval a {namespace eval bb {}}\nputs [lsort [namespace children ::a]]\nputs [lsort [namespace children ::a b*]]\nputs [lsort [namespace children ::a ::a::b*]]\nputs [lsort [namespace children ::a::b]]",
    "namespace eval ::x::y::z {}\nputs [namespace parent ::x::y::z]\nputs [namespace parent ::x::y]\nputs [namespace parent ::x]\nputs \"<[namespace parent ::]>\"",
    "namespace eval foo {}\nputs [namespace exists ::foo]\nputs [namespace exists foo]\nputs [namespace exists ::nope]\nputs [namespace exists ::]",

    // ── variables: which namespace an unqualified name belongs to ──
    "namespace eval foo {set x 1}\nputs $::foo::x",
    "namespace eval foo {variable v 9\nset y 2}\nputs \"$::foo::v $::foo::y\"",
    "namespace eval foo {namespace eval bar {variable q 3}}\nputs $::foo::bar::q",
    "namespace eval foo {variable a 1 b 2 c}\nputs \"$::foo::a $::foo::b\"\nnamespace eval foo {variable c 3}\nputs $::foo::c",
    // A qualified name written inside a namespace is still absolute.
    "set g top\nnamespace eval foo {variable g inner\nputs $::g\nputs $g}\nputs $::foo::g",
    // Reaching down into a nested namespace by a relative qualified name.
    "namespace eval a {namespace eval b {variable v deep}}\nnamespace eval a {puts $b::v}\nputs $::a::b::v",

    // ── variables inside a procedure: `variable` links, `global` does not ──
    "namespace eval foo {\n  variable v 10\n  proc get {} {variable v\n    return $v}\n  proc set2 {n} {variable v\n    set v $n\n    return $v}\n  proc lok {} {set v local\n    return $v}\n}\nputs [foo::get]\nputs [foo::set2 20]\nputs [foo::get]\nputs [foo::lok]\nputs $::foo::v",
    "set v global\nnamespace eval foo {\n  variable v scoped\n  proc g {} {global v\n    return $v}\n  proc n {} {variable v\n    return $v}\n}\nputs [foo::g]\nputs [foo::n]",
    "namespace eval foo {\n  variable count 0\n  proc bump {} {variable count\n    incr count\n    return $count}\n}\nputs [foo::bump][foo::bump][foo::bump]\nputs $::foo::count",
    // A `variable` that names an unset variable declares it without creating it.
    "namespace eval foo {\n  variable v\n  proc put {x} {variable v\n    set v $x}\n  proc get {} {variable v\n    return $v}\n}\nfoo::put 7\nputs [foo::get]\nputs $::foo::v",

    // ── the variable path: every command that reaches a variable itself ──
    // `array`, `lappend`, `append` and `incr` bypass `GetVar`/`SetVar` and
    // address the variable through `Compiler::var_place`, which is where the
    // namespace qualification is applied. Each of them therefore has to be
    // asked separately.
    "namespace eval foo {set a(1) x\nset a(2) y}\nputs [lsort [array names ::foo::a]]\nputs $::foo::a(1)\nputs [array size ::foo::a]",
    "namespace eval foo {variable d [dict create k v]}\nputs [dict get $::foo::d k]",
    "namespace eval foo {\n  variable lst {}\n  proc add {x} {variable lst\n    lappend lst $x\n    return $lst}\n}\nputs [foo::add 1]\nputs [foo::add 2]\nputs $::foo::lst",
    "namespace eval foo {\n  variable s \"\"\n  proc grow {x} {variable s\n    append s $x\n    return $s}\n}\nputs [foo::grow a][foo::grow b]\nputs $::foo::s",
    "namespace eval foo {\n  variable v 1\n  proc up {} {variable v\n    incr v\n    return $v}\n  proc get {} {variable v\n    return $v}\n}\nputs [foo::up][foo::up][foo::get]",
    "namespace eval foo {variable v 0}\nfor {set i 0} {$i < 3} {incr i} {namespace eval foo {variable v\n  incr v}}\nputs $::foo::v",
    "namespace eval foo {\n  variable arr\n  proc put {k v} {variable arr\n    set arr($k) $v}\n  proc get {k} {variable arr\n    return $arr($k)}\n}\nfoo::put a 1\nputs [foo::get a]\nputs $::foo::arr(a)",
    "namespace eval a::b {variable v 5}\nputs $::a::b::v\nputs [namespace exists ::a]\nnamespace eval a {puts $b::v\nputs [namespace current]}",

    // ── commands: the two-step search ──
    "proc p {} {return G}\nnamespace eval foo {\n  proc p {} {return N}\n  proc q {} {return [p]}\n  proc r {} {return [::p]}\n}\nputs [foo::q]\nputs [foo::r]\nputs [p]\nputs [foo::p]",
    // A namespace procedure calling one defined after it.
    "namespace eval foo {\n  proc first {} {return [second]}\n  proc second {} {return S}\n}\nputs [foo::first]",
    // A global procedure is reached from a namespace when the namespace has none.
    "proc only {} {return O}\nnamespace eval foo {proc use {} {return [only]}}\nputs [foo::use]",
    // Nested namespaces each resolve in their own.
    "namespace eval outer {\n  variable n 5\n  namespace eval inner {\n    variable n 7\n    proc get {} {variable n\n      return $n}\n  }\n  proc get {} {variable n\n    return $n}\n}\nputs [outer::get]\nputs [outer::inner::get]\nputs \"$::outer::n $::outer::inner::n\"",

    // ── eval: its value, and several arguments ──
    "puts [namespace eval foo {expr {1+1}}]",
    "puts [namespace eval foo {set a 1\nset b 2\nexpr {$a+$b}}]",
    "puts \"<[namespace eval foo {}]>\"",
    "puts [namespace eval bar \"set\" \"z\" \"9\"]\nputs $::bar::z",
    // Re-entering a namespace keeps what the first entry left.
    "namespace eval foo {variable v 1}\nnamespace eval foo {variable w 2\nputs \"$v $w\"}",

    // ── export and import ──
    "namespace eval a {proc one {} {return 1}\n  namespace export one}\nnamespace eval b {namespace import ::a::one\n  proc two {} {return [one][one]}}\nputs [b::two]\nputs [namespace origin ::b::one]\nputs [namespace which -command ::b::one]\nnamespace import a::one\nputs [one]\nputs [namespace origin one]",
    "namespace eval a {proc p {} {return P}\n  proc q {} {return Q}\n  namespace export p q}\nnamespace import a::*\nputs [p][q]\nputs [namespace origin p]\nnamespace forget a::p\nputs [catch {namespace origin ::p} m]\nputs $m\nputs [namespace origin ::q]",
    "namespace eval a {proc p {} {return P}\n  namespace export p}\nputs [namespace eval a {namespace export}]\nnamespace eval a {namespace export -clear}\nputs \"<[namespace eval a {namespace export}]>\"",
    // Measured: importing something unexported or absent is not an error.
    "namespace eval a {proc hidden {} {return H}}\nputs \"A:[catch {namespace import a::hidden} m]:$m\"\nputs \"B:[catch {namespace import a::nothere} m]:$m\"\nputs \"C:[catch {namespace import ::a::*} m]:$m\"\nputs \"D:[catch {hidden} m]:$m\"\nputs \"G:[catch {namespace import ::nosuchns::x} m]:$m\"",
    // Re-importing the same command is a no-op; a different one under the same
    // name is not.
    "namespace eval a {proc p {} {return A}\n  namespace export p}\nnamespace eval b {proc p {} {return B}\n  namespace export p}\nnamespace import a::p\nputs \"1:[catch {namespace import b::p} m]:$m\"\nputs \"2:[p]\"\nproc q {} {return LOCAL}\nnamespace eval c {proc q {} {return CQ}\n  namespace export q}\nputs \"5:[catch {namespace import c::q} m]:$m\"\nputs \"6:[q]\"",

    // ── code and inscope ──
    "namespace eval foo {}\nputs [namespace code {puts hi}]\nnamespace eval foo {puts [namespace code {puts hi}]\nputs [namespace code {a b c}]}\nputs [namespace code [list x 1 2]]",
    "namespace eval foo {variable v hello}\nputs [namespace inscope ::foo {set v}]\nnamespace eval foo {variable w 1}\nputs [namespace inscope ::foo {incr w 5}]\nputs $::foo::w",

    // ── which and origin ──
    "set x 1\nnamespace eval foo {variable x 2}\nputs [namespace which -variable x]\nputs [namespace which -variable ::foo::x]\nputs \"<[namespace which -variable nosuch]>\"\nproc p {} {return P}\nputs [namespace which -command p]\nputs \"<[namespace which -command nosuch]>\"\nputs [namespace which -command puts]",
    "namespace eval foo {proc p {} {}\n  variable v 1\n  puts [namespace which -command p]\n  puts [namespace which -variable v]}",

    // ── delete ──
    "namespace eval foo {proc p {} {return P}}\nputs [namespace exists ::foo]\nnamespace delete ::foo\nputs [namespace exists ::foo]\nputs [catch {namespace delete ::foo} m]\nputs $m\nputs [catch {namespace parent ::nope} m2]\nputs $m2",
    "namespace eval a {namespace eval b {namespace eval c {}}}\nnamespace delete ::a::b\nputs [namespace exists ::a]\nputs [namespace exists ::a::b]\nputs [namespace exists ::a::b::c]",

    // ── ensemble, as far as it is answered ──
    "namespace eval ens {proc sub {} {return S}\n  namespace export sub}\nputs [namespace eval ens {namespace ensemble exists ens}]\nnamespace eval ens {namespace ensemble create}\nputs [namespace ensemble exists ::ens]\nputs [namespace ensemble exists ::nope]\nputs [catch {namespace ensemble create -bogusopt x} m]\nputs $m",

    // ── the refusals, which are errors in both ──
    "puts [catch {namespace bogus} m]\nputs $m",
    "puts [catch {namespace eval} m]\nputs $m",
    "puts [catch {namespace qualifiers} m]\nputs $m",
    "puts [catch {namespace which} m]\nputs $m",

    // ── rename ──
    "proc f {} {return F}\nputs [f]\nrename f g\nputs [g]\nputs [namespace which -command g]\nputs \"<[namespace which -command f]>\"",
    // The self-deletion `tkInit` performs: the call that follows must refuse.
    "proc h {} {rename h {}\n  return H}\nputs [h]\nputs [catch {h} m]\nputs $m",
    "proc a {} {return A}\nputs [catch {rename nosuch x} m]\nputs $m\nputs [catch {rename} m2]\nputs $m2\nputs [catch {rename onlyone} m3]\nputs $m3\nnamespace eval ns {}\nrename a ::ns::a\nputs [namespace which -command ::ns::a]\nputs \"<[namespace which -command ::a]>\"",
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

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn reference_output(tclsh: &PathBuf, program: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "tclrs-ns-{}-{}.tcl",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
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

#[test]
fn namespaces_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };

    let mut failures = Vec::new();
    for program in PROGRAMS {
        let expected = reference_output(&tclsh, program);
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
        "{} of {} programs diverge:\n\n{}",
        failures.len(),
        PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// Every namespace name and body this frontend needs while compiling has to be
/// written out, because it decides which variable a `$v` reads and which
/// procedure a call reaches. A computed one is refused, and so are the three
/// subcommands that would change resolution after the fact.
///
/// Nothing here is a weaker answer than an error: an approximation would resolve
/// a name to the wrong variable and never say so.
#[test]
fn constructs_that_cannot_be_resolved_while_compiling_are_refused() {
    for (src, expected) in [
        ("set n foo\nnamespace eval $n {set x 1}", "computed"),
        ("set b {set x 1}\nnamespace eval foo $b", "computed"),
        (
            "namespace eval foo {namespace path ::bar}",
            "\"namespace path\"",
        ),
        (
            "namespace eval foo {namespace unknown x}",
            "\"namespace unknown\"",
        ),
        (
            "namespace eval foo {namespace upvar ::a v v}",
            "\"namespace upvar\"",
        ),
        (
            "namespace eval foo {set p a::*\n  namespace import $p}",
            "computed",
        ),
        (
            "namespace eval foo {proc p {} {variable a::b}}",
            "can't create a local variable with a namespace separator",
        ),
        (
            "source -encoding shiftjis x.tcl",
            "only supported for utf-8",
        ),
        // Inside a procedure body an unqualified name is a frame slot, so the
        // body of a `namespace eval` would set a local rather than the
        // namespace's variable. Measured against tclsh, which sets `::foo::x`.
        (
            "proc p {} {namespace eval foo {set x 1}}",
            "\"namespace eval\" inside a procedure is not supported yet",
        ),
    ] {
        let err = tclrs::eval(src).expect_err(&format!("{src:?} should fail"));
        assert!(
            err.contains(expected),
            "{src:?}: expected an error mentioning {expected:?}, got {err:?}"
        );
    }
}

/// The name grammar is a pure function of the text, so the folded answer and the
/// answer the runtime op produces have to be the same one.
///
/// `tests/namespace_differential.rs`'s first two programs pin both against
/// tclsh; this pins them against *each other*, which is what a divergence
/// between the compile-time and run-time paths would look like.
#[test]
fn folded_and_computed_name_operations_agree() {
    for name in [
        "::foo::bar",
        "foo::bar",
        "bar",
        "::",
        "::foo::",
        "foo::bar::",
        "",
        "a::b::c",
    ] {
        for sub in ["qualifiers", "tail"] {
            let folded = tclrs::eval(&format!("namespace {sub} {{{name}}}"))
                .expect("the folded form compiles")
                .result;
            let computed = tclrs::eval(&format!("set n {{{name}}}\nnamespace {sub} $n"))
                .expect("the computed form compiles")
                .result;
            assert_eq!(folded, computed, "namespace {sub} {name:?}");
        }
    }
}
