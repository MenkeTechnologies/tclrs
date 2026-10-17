//! `clock clicks` takes `-milliseconds` or `-microseconds` as
//! `Tcl_GetIndexFromObj` takes a table entry: exactly, or as an abbreviation
//! only one of the two admits. Expectations were produced by tclsh 9.0.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn an_abbreviation_that_names_one_switch_is_accepted() {
    let out = run(
        "foreach s {-mil -milli -milliseconds -mic -micro -microseconds} {\n\
           puts [string is integer -strict [clock clicks $s]]\n\
         }",
    );
    assert_eq!(out, "1\n1\n1\n1\n1\n1\n");
}

#[test]
fn an_abbreviation_both_switches_admit_is_ambiguous() {
    let out = run("foreach s {{} - -m -mi} {\n\
           catch {clock clicks $s} m o\n\
           puts \"$m | [dict get $o -errorcode]\"\n\
         }");
    assert_eq!(
        out,
        "ambiguous option \"\": must be -milliseconds or -microseconds | TCL LOOKUP INDEX option {}\n\
         ambiguous option \"-\": must be -milliseconds or -microseconds | TCL LOOKUP INDEX option -\n\
         ambiguous option \"-m\": must be -milliseconds or -microseconds | TCL LOOKUP INDEX option -m\n\
         ambiguous option \"-mi\": must be -milliseconds or -microseconds | TCL LOOKUP INDEX option -mi\n"
    );
}

#[test]
fn anything_else_is_a_bad_option() {
    let out = run("foreach s {foo -microsecondsx -MILLI} {\n\
           catch {clock clicks $s} m\n\
           puts $m\n\
         }");
    assert_eq!(
        out,
        "bad option \"foo\": must be -milliseconds or -microseconds\n\
         bad option \"-microsecondsx\": must be -milliseconds or -microseconds\n\
         bad option \"-MILLI\": must be -milliseconds or -microseconds\n"
    );
}
