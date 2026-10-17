//! `clock scan` with no `-format`: the free-form date parser.
//!
//! Every row below is the answer tclsh 9.0.4 gave to
//! `clock scan STRING -base 1600000000 -timezone ZONE`, built from the release's
//! own source: the completion code, the result or message, and the `-errorcode`.
//! The strings cover the grammar of `generic/tclGetDate.y` — every kind of date,
//! time, zone, relative term and weekday — and its refusals, including the
//! column at which the parser reports a syntax error, and the words it reads
//! oddly (`this` is read as `thi`, so it is not a word at all).
//!
//! The rows are data; nothing here is computed by tclrs.

fn scan(zone: &str, input: &str) -> (i32, String, String) {
    let script = format!(
        "set c [catch {{clock scan {{{input}}} -base 1600000000 -timezone {zone}}} m o]\n\
         puts $c\nputs $m\nputs [dict getdef $o -errorcode -]"
    );
    let out = tclrs::eval(&script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output;
    let mut lines = out.lines();
    let code = lines.next().expect("code").parse().expect("integer code");
    let message = lines.next().expect("message").to_string();
    let errorcode = lines.next().expect("errorcode").to_string();
    (code, message, errorcode)
}

#[test]
fn free_form_scans_answer_as_tclsh_does() {
    let rows: &[(&str, &str, i32, &str, &str)] = &[
        ("UTC", "2020-01-02", 0, "1577923200", "-"),
        ("UTC", "2020-01-02 03:04:05", 0, "1577934245", "-"),
        ("UTC", "2020-01-02T03:04:05", 0, "1577934245", "-"),
        ("UTC", "2020-01-02 03:04:05 PM", 0, "1577977445", "-"),
        ("UTC", "1/2/2020", 0, "1577923200", "-"),
        ("UTC", "1/2/20", 0, "1577923200", "-"),
        ("UTC", "Jan 2 2020", 0, "1577996400", "-"),
        ("UTC", "Jan 2, 2020", 0, "1577923200", "-"),
        ("UTC", "2 Jan 2020", 0, "1577923200", "-"),
        ("UTC", "2-Jan-2020", 0, "1577923200", "-"),
        ("UTC", "sept. 3, 2021", 1, "unable to convert date-time string \"sept. 3, 2021\": syntax error (characters 0-4)", "TCL VALUE DATE PARSE"),
        ("UTC", "20200102", 0, "1577923200", "-"),
        ("UTC", "20200102T030405", 0, "1577934245", "-"),
        ("UTC", "20200102030405", 0, "1577934245", "-"),
        ("UTC", "202001021530", 0, "1577979000", "-"),
        ("UTC", "now", 0, "1600000000", "-"),
        ("UTC", "today", 0, "1600000000", "-"),
        ("UTC", "tomorrow", 0, "1600041600", "-"),
        ("UTC", "yesterday", 0, "1599868800", "-"),
        ("UTC", "epoch", 0, "0", "-"),
        ("UTC", "stardate 1234.5", 0, "-718459200", "-"),
        ("UTC", "next week", 0, "1600560000", "-"),
        ("UTC", "last week", 0, "1599350400", "-"),
        ("UTC", "next month", 0, "1602547200", "-"),
        ("UTC", "1 day ago", 0, "1599868800", "-"),
        ("UTC", "2 days ago", 0, "1599782400", "-"),
        ("UTC", "3 hours ago", 0, "1599989200", "-"),
        ("UTC", "+2 weeks", 0, "1601164800", "-"),
        ("UTC", "-1 year", 0, "1568332800", "-"),
        ("UTC", "1 fortnight", 0, "1601164800", "-"),
        ("UTC", "next monday", 0, "1600646400", "-"),
        ("UTC", "monday", 0, "1600041600", "-"),
        ("UTC", "last friday", 0, "1599782400", "-"),
        ("UTC", "2 fridays ago", 1, "unable to convert date-time string \"2 fridays ago\": syntax error (characters 2-8)", "TCL VALUE DATE PARSE"),
        ("UTC", "next december", 0, "1607817600", "-"),
        ("UTC", "next 2 january", 0, "1642032000", "-"),
        ("UTC", "Monday, January 4, 2021", 0, "1609718400", "-"),
        ("UTC", "tomorrow 3pm", 0, "1600095600", "-"),
        ("UTC", "12am", 0, "1599955200", "-"),
        ("UTC", "12pm", 0, "1599998400", "-"),
        ("UTC", "3:15pm", 0, "1600010100", "-"),
        ("UTC", "23:59:59", 0, "1600041599", "-"),
        ("UTC", "0:00", 0, "1599955200", "-"),
        ("UTC", "2020-01-31 +1 month", 0, "1582934400", "-"),
        ("UTC", "2020-03-31 -1 month", 0, "1582934400", "-"),
        ("UTC", "2020-02-29 +1 year", 0, "1614470400", "-"),
        ("UTC", "2020-01-02 EST", 0, "1577941200", "-"),
        ("UTC", "03:00 PST", 0, "1599994800", "-"),
        ("UTC", "03:00 PDT", 0, "1599991200", "-"),
        ("UTC", "03:00 BST", 0, "1599962400", "-"),
        ("UTC", "2020-01-02 03:00 +0530", 0, "1577914200", "-"),
        ("UTC", "2020-01-02 03:00 -0800", 0, "1577962800", "-"),
        ("UTC", "now GMT+1", 0, "1599996400", "-"),
        ("UTC", "now GMT+0100", 0, "1599996400", "-"),
        ("UTC", "+0100 12", 0, "1599962400", "-"),
        ("UTC", "12 +0100", 0, "1599994800", "-"),
        ("UTC", "10:00 -05", 0, "1600009200", "-"),
        ("UTC", "(comment) 2020-01-02", 1, "unable to convert date-time string \"(comment) 2020-01-02\": syntax error (characters 0-1)", "TCL VALUE DATE PARSE"),
        ("UTC", "2020-01-02 (a (nested) comment) 03:04", 0, "1577934240", "-"),
        ("UTC", "this", 1, "unable to convert date-time string \"this\": syntax error (characters 0-3)", "TCL VALUE DATE PARSE"),
        ("UTC", "this friday", 1, "unable to convert date-time string \"this friday\": syntax error (characters 0-3)", "TCL VALUE DATE PARSE"),
        ("UTC", "months", 1, "unable to convert date-time string \"months\": syntax error (characters 0-5)", "TCL VALUE DATE PARSE"),
        (":America/New_York", "2020-03-07 +1 day", 0, "1583643600", "-"),
        (":America/New_York", "2020-03-07 12:00 +24 hours", 0, "1583686800", "-"),
        (":America/New_York", "tomorrow", 0, "1600056000", "-"),
        (":America/New_York", "next monday 9am", 0, "1600693200", "-"),
        (":Europe/Berlin", "now +90 minutes", 0, "1600005400", "-"),
        ("UTC", "bad-string", 1, "unable to convert date-time string \"bad-string\": syntax error (characters 0-2)", "TCL VALUE DATE PARSE"),
        ("UTC", "", 0, "1600000000", "-"),
        ("UTC", "   ", 0, "1600000000", "-"),
        ("UTC", "foo", 1, "unable to convert date-time string \"foo\": syntax error (characters 0-2)", "TCL VALUE DATE PARSE"),
        ("UTC", "ago", 1, "unable to convert date-time string \"ago\": syntax error (characters 0-2)", "TCL VALUE DATE PARSE"),
        ("UTC", "2020-13-45", 1, "unable to convert input string: invalid month", "CLOCK invInpStr month"),
        ("UTC", "2020-02-30", 1, "unable to convert input string: invalid day", "CLOCK invInpStr day"),
        ("UTC", "25:00", 1, "unable to convert input string: invalid time (hour)", "CLOCK invInpStr hour"),
        ("UTC", "3 2 1", 1, "unable to convert date-time string \"3 2 1\": more than one time of day in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "3pm 4pm", 1, "unable to convert date-time string \"3pm 4pm\": more than one time of day in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "2020-01-01 2020-01-02", 1, "unable to convert date-time string \"2020-01-01 2020-01-02\": more than one date in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "EST EST", 1, "unable to convert date-time string \"EST EST\": more than one time zone in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "monday tuesday", 1, "unable to convert date-time string \"monday tuesday\": more than one weekday in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "next jan next feb", 1, "unable to convert date-time string \"next jan next feb\": more than one ordinal month in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "2020-01-02 03:00 +99999999999999999999", 1, "unable to convert date-time string \"2020-01-02 03:00 +99999999999999999999\": syntax error (characters 18-17)", "TCL VALUE DATE PARSE"),
        ("UTC", "99999999999999999999", 1, "unable to convert date-time string \"99999999999999999999\": syntax error (characters 0-1)", "TCL VALUE DATE PARSE"),
        ("UTC", "tomorrow noon", 1, "unable to convert date-time string \"tomorrow noon\": syntax error (characters 9-12)", "TCL VALUE DATE PARSE"),
        ("UTC", "2 fridays", 1, "unable to convert date-time string \"2 fridays\": syntax error (characters 2-8)", "TCL VALUE DATE PARSE"),
        ("UTC", "1,2", 1, "unable to convert date-time string \"1,2\": syntax error (characters 1-1)", "TCL VALUE DATE PARSE"),
        ("UTC", "1 2 3 4", 1, "unable to convert date-time string \"1 2 3 4\": more than one time of day in string", "TCL VALUE DATE MULTIPLE"),
        ("UTC", "a.m.", 1, "unable to convert date-time string \"a.m.\": syntax error (characters 0-3)", "TCL VALUE DATE PARSE"),
        ("UTC", "12 a.m.", 0, "1599955200", "-"),
    ];
    let mut failures = Vec::new();
    for &(zone, input, code, message, errorcode) in rows {
        let got = scan(zone, input);
        if got != (code, message.to_string(), errorcode.to_string()) {
            failures.push(format!(
                "{zone} {input:?}\n  tclsh: {:?}\n  tclrs: {got:?}",
                (code, message, errorcode)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
