//! Range checks and error codes of `clock`, and `clock add weekdays` from a
//! weekend. Expectations were produced by tclsh 9.0.4.

fn run(script: &str) -> String {
    tclrs::eval(script)
        .unwrap_or_else(|e| panic!("script failed: {e}\n{script}"))
        .output
}

#[test]
fn clock_values_past_the_supported_range_are_too_large() {
    let out = run(
        "foreach s {{clock format 0x8000000000000000} {clock format 99999999999999999999}\n\
                    {clock format 0x7fffffffffffffff} {clock scan 1 -base 0x8000000000000000}\n\
                    {clock add 0x7fffffffffffffff 1 second} {clock add 1 9223372036854775807 days}\n\
                    {clock add 1 9223372036854775807 weekdays} {clock format 1.5}} {\n\
           catch $s m o\n\
           puts \"$m | [dict get $o -errorcode]\"\n\
         }",
    );
    assert_eq!(
        out,
        "integer value too large to represent | CLOCK badOption 0x8000000000000000\n\
         integer value too large to represent | CLOCK badOption 99999999999999999999\n\
         integer value too large to represent | CLOCK badOption 0x7fffffffffffffff\n\
         integer value too large to represent | CLOCK badOption 0x8000000000000000\n\
         integer value too large to represent | CLOCK badOption 0x7fffffffffffffff\n\
         integer value too large to represent | NONE\n\
         integer value too large to represent | NONE\n\
         bad seconds \"1.5\": must be now or integer | CLOCK badOption 1.5\n"
    );
}

#[test]
fn scan_errors_carry_their_clock_codes() {
    let out = run(
        "foreach s {{clock scan 2000W018 -format %GW%V%u -gmt true} {clock scan 1 -gmt 1 -timezone UTC}\n\
                    {clock scan 24:01:00 -format %H:%M:%S} {clock scan 12:00:60 -format %H:%M:%S}} {\n\
           catch $s m o\n\
           puts \"$m | [dict get $o -errorcode]\"\n\
         }",
    );
    assert_eq!(
        out,
        "day of week is greater than 7 | CLOCK badDayOfWeek\n\
         cannot use -gmt and -timezone in same call | CLOCK gmtWithTimezone\n\
         unable to convert input string: invalid time | CLOCK invInpStr time\n\
         unable to convert input string: invalid time | CLOCK invInpStr seconds\n"
    );
}

#[test]
fn adding_weekdays_from_a_weekend_and_a_zero_quantity() {
    // 2023-11-18 (Saturday) and 2023-11-19 (Sunday), 00:00 UTC.
    let out = run(
        "foreach start {1700265600 1700352000} {\n\
           foreach n {-6 -1 0 1 2 5 400} { puts -nonewline \"[clock format [clock add $start $n weekdays -gmt 1] -format %a -gmt 1] \" }\n\
           puts \"\"\n\
         }",
    );
    assert_eq!(
        out,
        "Thu Thu Sat Mon Tue Fri Fri \nThu Thu Sun Mon Tue Fri Fri \n"
    );
}
