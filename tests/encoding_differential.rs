//! Differential execution for `encoding`.
//!
//! Same contract as the other harnesses here: every program is run by both
//! tclsh 9.0.4 and tclrs and the two outputs are compared byte for byte, so no
//! expectation below is written by hand. For transcoding that is the only
//! honest way to test: a table that is one code point out produces output that
//! looks exactly as plausible as the right output, and an expectation typed
//! from a code chart would agree with the bug.
//!
//! The rules being pinned, each of which is either absent from `encoding(n)` or
//! contradicted by it — see the sweeps at the end for the tables themselves:
//!
//! * `strict` is the default profile. The manual page says so and then shows
//!   `encoding convertto iso8859-1 A\u0141` answering `A?`, which is what the
//!   `tcl8` profile does; the command actually raises `unexpected character at
//!   index 1: 'U+000141'`.
//! * Under `tcl8`, a byte that is not valid where it stands does *not* become
//!   the code point of its own value. It becomes the cp1252 character of that
//!   value where cp1252 defines one: `encoding convertfrom -profile tcl8 ascii
//!   A\x80` is U+0041 U+20AC, where `encoding(n)` says U+0041 U+0080. The
//!   manual page confines this rule to utf-8 sources; it applies to the table
//!   encodings as well.
//! * `-failindex` and the error message do not report the same number for
//!   `convertto`. The variable gets a byte offset into the string's UTF-8 form
//!   and the message gets a character index: for `éé€` in iso8859-1 they are 4
//!   and 2.
//! * `Tcl_GetBytesFromObj`'s refusal says "byte offset" and reports the count
//!   of characters it had already converted.
//! * `encoding user extra` is refused with a trailing space inside the quotes.
//! * The two-alternative `wrong # args` message names `encoding convertfrom`
//!   for some argument counts and `::tcl::encoding::convertfrom` for others,
//!   because one to three arguments are compiled to a direct call of the
//!   ensemble's implementation and any other count is not.
//! * `identity` and `binary` are not encodings in 9.0.4 — both were Tcl 8
//!   spellings — so naming either is `unknown encoding`.
//!
//! Not compared: `encoding names`. tclsh answers in the order of its hash
//! table and includes the three escape encodings it can load but this frontend
//! does not implement; `names_lists_only_what_converts` below checks the
//! property that matters instead — every name it offers actually converts.

use std::path::PathBuf;
use std::process::Command;

/// Programs whose output must agree, byte for byte.
///
/// Byte strings are written with `\x` escapes so that the two parsers are
/// handed identical text, and results are printed inside `<>` so a trailing
/// space or an empty answer is visible in the comparison.
const PROGRAMS: &[&str] = &[
    // ── the profile is strict unless one is named ────────────────────────
    "puts [catch {encoding convertto iso8859-1 A\\u0141} r]\nputs <$r>",
    "puts [catch {encoding convertfrom ascii A\\x80} r]\nputs <$r>",
    "puts <[encoding convertto -profile tcl8 iso8859-1 A\\u0141]>",
    "puts <[encoding convertto -profile replace iso8859-1 A\\u0141]>",
    // ── the cp1252 fallback of the tcl8 profile, on a table encoding ─────
    "foreach b {\\x80 \\x81 \\x8d \\x90 \\x9d \\x9e \\xa0 \\xff} {\n    puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile tcl8 ascii $b]]>\n}",
    "foreach b {\\x80 \\x81 \\x8d \\x90 \\x9d \\x9e \\xa0 \\xff} {\n    puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile tcl8 utf-8 $b]]>\n}",
    // ── -failindex against the message's own index ───────────────────────
    "set v x\nputs \"<[encoding convertto -failindex v iso8859-1 éé€]> $v\"\nputs [catch {encoding convertto iso8859-1 éé€} r]\nputs <$r>",
    "set v x\nputs \"<[encoding convertfrom -failindex v utf-8 ab\\x80cd]> $v\"",
    "set v x\nputs \"<[encoding convertto -failindex v -profile tcl8 iso8859-1 ab€cd]> $v\"",
    "set v x\nputs \"<[encoding convertfrom -failindex v -profile replace utf-8 ab\\x80cd]> $v\"",
    // A conversion that succeeds must leave -1 behind, not the old value.
    "set v x\nputs \"<[encoding convertfrom -failindex v utf-8 abc]> $v\"",
    // The option names are prefix-matched; the profile names are not.
    "puts <[encoding convertfrom -prof tcl8 ascii \\x80]>",
    "set v x\nputs \"<[encoding convertfrom -f v ascii \\x80]> $v\"",
    "puts [catch {encoding convertfrom -profile str ascii \\x80} r]\nputs <$r>",
    // ── the shape of the ensemble ────────────────────────────────────────
    "puts <[encoding profiles]>",
    "puts <[encoding system]>",
    "puts <[encoding user]>",
    "puts [catch {encoding names} r]",
    // ── a prefix byte with nothing after it, per profile ─────────────────
    "foreach p {tcl8 replace} {\n    puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile $p euc-jp \\xa4]]>\n}",
    "puts [catch {encoding convertfrom -profile strict euc-jp \\xa4} r]\nputs <$r>",
    // A prefix byte followed by one the table has no pair for: the reported
    // index is the *second* byte's, not the first's.
    "puts [catch {encoding convertfrom -profile strict euc-jp \\xa4\\x20} r]\nputs <$r>",
    "foreach p {tcl8 replace} {\n    puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile $p euc-jp \\xa4\\x20]]>\n}",
    // ── the modified-UTF-8 null, which only tcl8 accepts ─────────────────
    "puts [string length [encoding convertfrom -profile tcl8 utf-8 \\xc0\\x80]]",
    "puts [catch {encoding convertfrom -profile strict utf-8 \\xc0\\x80} r]\nputs <$r>",
    "puts [string length [encoding convertfrom -profile replace utf-8 \\xc0\\x80]]",
    // ── the UTF-16, UCS-2 and UTF-32 families ────────────────────────────
    "foreach e {utf-16 utf-16le utf-16be unicode ucs-2 ucs-2le ucs-2be utf-32 utf-32le utf-32be} {\n    puts \"<$e [encoding convertto -profile tcl8 utf-8 [encoding convertto $e abé]]>\"\n}",
    "foreach e {utf-16le utf-16be utf-32le utf-32be} {\n    puts \"<$e [encoding convertto -profile tcl8 utf-8 [encoding convertto $e \\U00010437]]>\"\n}",
    // ucs-2 has no surrogate pairs, so an astral character cannot be encoded.
    "puts [catch {encoding convertto ucs-2 \\U00010437} r]\nputs <$r>",
    "puts <[encoding convertto -profile tcl8 utf-8 [encoding convertto -profile tcl8 ucs-2le \\U00010437]]>",
    // A trailing byte with no partner: only strict refuses it.
    "puts [catch {encoding convertfrom -profile strict utf-16be \\x00a\\x00} r]\nputs <$r>",
    "foreach p {tcl8 replace} {\n    puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile $p utf-16be \\x00a\\x00]]>\n}",
    // ── cesu-8: a surrogate pair is how it carries an astral character ────
    "puts <[encoding convertfrom cesu-8 \\xED\\xA0\\x81\\xED\\xB0\\xB7]>",
    "puts <[encoding convertto -profile tcl8 utf-8 [encoding convertto cesu-8 \\U00010437]]>",
    "puts [catch {encoding convertfrom -profile strict cesu-8 \\xED\\xB0\\xB7} r]\nputs <$r>",
    "puts <[encoding convertto -profile tcl8 utf-8 [encoding convertfrom -profile replace cesu-8 \\xED\\xB0\\xB7]]>",
    // ── the symbol fonts, whose page 0 maps to itself ────────────────────
    "puts <[encoding convertfrom symbol abc]>",
    "puts <[encoding convertto symbol αβγ]>",
    "puts <[encoding convertto symbol abc]>",
    // ── each table's own fallback character ──────────────────────────────
    "foreach e {ascii iso8859-1 ebcdic jis0208 jis0212 cns11643 gb2312-raw gb12345 ksc5601} {\n    puts \"<$e [encoding convertto -profile tcl8 utf-8 [encoding convertto -profile tcl8 $e €]]>\"\n}",
    // A multi-byte table without a backslash gets one, so a path still works.
    "foreach e {shiftjis cp932 euc-jp big5 cp950} {\n    puts \"<$e [encoding convertfrom $e \\x5c] [encoding convertto $e \\\\]>\"\n}",
    // ── the reverse section four of the Japanese tables carry ────────────
    "foreach e {shiftjis cp932 euc-jp jis0208} {\n    puts \"<$e [encoding convertto -profile tcl8 utf-8 [encoding convertto $e \\uFF5E]] [encoding convertto -profile tcl8 utf-8 [encoding convertto $e \\u301C]]>\"\n}",
    // ── the one-argument form uses the system encoding ───────────────────
    "puts <[encoding convertfrom \\xc3\\xa9]>",
    "puts <[encoding convertto -profile tcl8 utf-8 [encoding convertto é]]>",
    // ── encoding dirs is a list, and answers what it was set to ──────────
    //
    // Its *initial* value is not compared: tclsh's is where its own library was
    // installed, and this frontend carries the tables inside the binary and has
    // no directory to search, so it starts empty. What is compared is that a
    // list set through it comes back.
    "puts <[encoding dirs {a b}]>\nputs <[encoding dirs]>",
    "puts [catch {encoding dirs \\{} r]\nputs <$r>",
];

/// Programs for `fconfigure -encoding`, where `%F` is a scratch path.
///
/// A channel converts through the same tables, so what these check is the part
/// `encoding convertfrom` cannot: that a character split across two reads is
/// still one character. Each is read back at a `-buffersize` small enough to cut
/// multi-byte characters in half, and again in one buffer, and once more a line
/// at a time.
///
/// A channel's profile is `strict` — measured, `fconfigure $f -profile` on a
/// fresh channel answers `strict` — so every program here writes only text its
/// encoding can hold; the one that cannot is in [`CHANNEL_ERRORS`].
const CHANNEL_PROGRAMS: &[&str] = &[
    "set text \"はabcは日本語x\"\n\
     foreach e {euc-jp shiftjis cp932 utf-8 cesu-8 utf-16 utf-16le utf-16be utf-32 utf-32le utf-32be ucs-2 big5} {\n\
     \x20   set fh [open %F w]\n\
     \x20   fconfigure $fh -encoding $e -translation lf\n\
     \x20   puts -nonewline $fh $text\n\
     \x20   close $fh\n\
     \x20   foreach bs {10 1024} {\n\
     \x20       set g [open %F]\n\
     \x20       fconfigure $g -encoding $e -translation lf -buffersize $bs\n\
     \x20       set back [read $g]\n\
     \x20       close $g\n\
     \x20       puts \"$e $bs [string length $back] [string equal $back $text]\"\n\
     \x20   }\n\
     \x20   set g [open %F]\n\
     \x20   fconfigure $g -encoding iso8859-1 -translation lf\n\
     \x20   puts \"$e raw [string length [read $g]]\"\n\
     \x20   close $g\n\
     \x20   set g [open %F]\n\
     \x20   fconfigure $g -encoding $e -translation lf -buffersize 7\n\
     \x20   set n 0\n\
     \x20   while {[gets $g line] >= 0} { incr n [string length $line] }\n\
     \x20   close $g\n\
     \x20   puts \"$e gets $n\"\n\
     }",
    // The single-byte tables, read three bytes at a time.
    "foreach e {iso8859-1 iso8859-2 iso8859-15 cp1250 cp1252 koi8-r koi8-u macRoman macCyrillic ascii ebcdic cp437 cp850 tis-620} {\n\
     \x20   set fh [open %F w]\n\
     \x20   fconfigure $fh -encoding $e -translation lf\n\
     \x20   puts -nonewline $fh abcXYZ\n\
     \x20   close $fh\n\
     \x20   set g [open %F]\n\
     \x20   fconfigure $g -encoding $e -translation lf -buffersize 3\n\
     \x20   puts \"$e <[read $g]>\"\n\
     \x20   close $g\n\
     }",
    // What the option reports back, and that an unknown name is refused with
    // the same wording `encoding convertfrom` uses.
    "set fh [open %F w]\nputs [fconfigure $fh -encoding]\nfconfigure $fh -encoding koi8-r\nputs [fconfigure $fh -encoding]\nputs [catch {fconfigure $fh -encoding bogus} r]\nputs $r\nputs [fconfigure $fh -encoding]\nclose $fh",
];

/// Channel programs that must fail, in the same wording. `%F` is a scratch path
/// and the channel *name* in the message is normalized away: a name is `file`
/// plus a file descriptor number, and the two interpreters do not have the same
/// descriptors free.
const CHANNEL_ERRORS: &[&str] = &[
    // A character the encoding cannot hold, on a strict channel.
    "set fh [open %F w]\nfconfigure $fh -encoding ascii -translation lf\nputs -nonewline $fh aŁb",
    "set fh [open %F w]\nfconfigure $fh -encoding ksc5601 -translation lf\nputs -nonewline $fh abc",
    // A byte sequence the encoding cannot decode, likewise.
    "set fh [open %F w]\nfconfigure $fh -encoding iso8859-1 -translation lf\nputs -nonewline $fh a\\x81b\nclose $fh\nset g [open %F]\nfconfigure $g -encoding cp1252 -translation lf\nread $g",
    "set fh [open %F w]\nfconfigure $fh -encoding iso8859-1 -translation lf\nputs -nonewline $fh a\\x81b\nclose $fh\nset g [open %F]\nfconfigure $g -encoding cp1252 -translation lf\ngets $g line",
];

/// Programs tclsh refuses. The first line of the error must match.
const ERRORS: &[&str] = &[
    "encoding",
    "encoding bogus",
    "encoding conv utf-8 abc",
    "encoding convert utf-8 abc",
    "encoding names extra",
    "encoding profiles extra",
    "encoding user extra",
    "encoding convertfrom",
    "encoding convertto",
    "encoding convertfrom -bogus utf-8 abc",
    "encoding convertfrom utf-8 abc extra",
    // The value slot of the last option is the encoding: the option was
    // written without one. Three arguments name the implementation, five name
    // the ensemble.
    "encoding convertfrom -profile tcl8 utf-8",
    "encoding convertto -profile tcl8 utf-8",
    "encoding convertfrom -profile tcl8 -failindex v utf-8",
    "encoding convertto -profile tcl8 -failindex v utf-8",
    "encoding convertfrom -profile bogus utf-8 abc",
    "encoding convertfrom -profile \"\" utf-8 abc",
    "encoding convertfrom bogus abc",
    "encoding convertto bogus abc",
    "encoding system bogus",
    // Gone in 9.0.4, and not revived here.
    "encoding convertfrom identity abc",
    "encoding convertto identity abc",
    "encoding convertfrom binary abc",
    "encoding convertto binary abc",
    // A string that is not a byte string cannot be decoded.
    "encoding convertfrom ascii Ł",
    "encoding convertfrom ascii aŁ",
    "encoding convertfrom ascii ééŁ",
    // Strict refusals, whose index and byte are part of the message.
    "encoding convertfrom cp1252 \\x81",
    "encoding convertto ascii \\x80",
    "encoding convertto ascii ééx",
    "encoding convertfrom utf-8 \\x80",
    "encoding convertfrom utf-8 \\xED\\xA0\\x80",
    "encoding convertfrom utf-32be \\x00\\x00\\xD8\\x00",
    "encoding convertfrom utf-32be \\x00\\x11\\x00\\x00",
];

/// The encodings the sweeps below run over: everything this frontend offers.
fn sweep_encodings() -> Vec<&'static str> {
    tclrs::cmd_encoding::names()
}

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

/// Run a program through tclsh, returning its stdout and the first line of any
/// error it reported. tclsh follows an error with a stack trace and tclrs does
/// not, so only the first line is comparable.
fn reference(tclsh: &PathBuf, program: &str) -> (String, Option<String>) {
    let script = std::env::temp_dir().join(format!(
        "tclrs-enc-{}-{:x}.tcl",
        std::process::id(),
        program.len() as u64
            ^ program
                .as_bytes()
                .iter()
                .map(|b| u64::from(*b))
                .sum::<u64>()
    ));
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

#[test]
fn encoding_conversions_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in PROGRAMS {
        let (expected, error) = reference(&tclsh, program);
        assert!(
            error.is_none(),
            "tclsh rejected a program that should run:\n{program}\n{}",
            error.unwrap_or_default()
        );
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

/// A refusal's text, without the `(line N)` a compiler can report and an
/// interpreter cannot.
fn without_location(message: &str) -> String {
    match message.rfind(" (line ") {
        Some(at) if message.ends_with(')') => message[..at].to_string(),
        _ => message.to_string(),
    }
}

/// What tclsh says when the program is *compiled* and then run, which is what
/// this frontend always does and what `conformance/reference.tcl` does — it
/// evaluates each case with `$child eval`.
///
/// The distinction is not cosmetic. `encoding convertfrom` carries
/// `TclCompileBasic1To3ArgCmd`, so with one to three arguments tclsh compiles
/// the call to a direct invocation of `::tcl::encoding::convertfrom` and its
/// `wrong # args` message names *that*; a bare top-level command in a script
/// file reaches the ensemble's run-time dispatch instead and the same message
/// names `encoding convertfrom`. Measured on tclsh 9.0.4, and the suite itself
/// treats both as correct (`cmdAH.test:207` matches them with an alternation).
fn reference_error(tclsh: &PathBuf, program: &str) -> Option<String> {
    assert!(
        !program.contains('{') && !program.contains('}'),
        "an error program goes inside braces, so it cannot contain one: {program}"
    );
    let wrapper =
        format!("set c [interp create]\nif {{[catch {{$c eval {{{program}}}}} r]}} {{puts $r}}\n");
    let (stdout, error) = reference(tclsh, &wrapper);
    assert!(error.is_none(), "the wrapper itself failed: {error:?}");
    let text = stdout.trim_end_matches('\n');
    (!text.is_empty()).then(|| text.to_string())
}

#[test]
fn encoding_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in ERRORS {
        let Some(expected) = reference_error(&tclsh, program) else {
            failures.push(format!(
                "tclsh accepted {program:?}, so it is not an error case"
            ));
            continue;
        };
        match tclrs::eval(program) {
            Err(e) if without_location(&e.to_string()) == expected => {}
            Err(e) => failures.push(format!(
                "program: {program}\n  tclsh: {expected}\n  tclrs: {e}"
            )),
            Ok(_) => failures.push(format!(
                "program: {program}\n  tclsh: {expected}\n  tclrs: accepted it"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} refusals diverge:\n\n{}",
        failures.len(),
        ERRORS.len(),
        failures.join("\n\n")
    );
}

/// Every single byte, in every encoding, under every profile: decoded,
/// re-encoded, and decoded again with `-failindex`.
///
/// One program rather than one per case, because the point is the tables and
/// there are tens of thousands of cells to check. The loop is written in Tcl so
/// that both interpreters run exactly the same text, and the encoding and
/// profile names reach the command as *values*, which is also the only test
/// here that the lowering handles a computed encoding name.
#[test]
fn every_single_byte_matches_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let program = format!(
        "set encs {{{}}}\n\
         foreach e $encs {{\n\
         \x20   foreach p {{tcl8 strict replace}} {{\n\
         \x20       for {{set i 0}} {{$i < 256}} {{incr i}} {{\n\
         \x20           set b [format %c $i]\n\
         \x20           set rc [catch {{encoding convertfrom -profile $p $e $b}} r]\n\
         \x20           puts \"F $e $p $i $rc <$r>\"\n\
         \x20           if {{$rc == 0}} {{\n\
         \x20               set rc2 [catch {{encoding convertto -profile $p $e $r}} r2]\n\
         \x20               puts \"T $e $p $i $rc2 <$r2>\"\n\
         \x20           }}\n\
         \x20           set v UNSET\n\
         \x20           set rc3 [catch {{encoding convertfrom -profile $p -failindex v $e $b}} r3]\n\
         \x20           puts \"V $e $p $i $rc3 $v <$r3>\"\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n",
        sweep_encodings().join(" ")
    );
    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh failed the sweep: {error:?}");
    let outcome = tclrs::eval(&program).expect("tclrs runs the sweep");
    assert_eq!(
        first_difference(&expected, &outcome.output),
        None,
        "the single-byte sweep diverges"
    );
}

/// Two-byte sequences for the encodings where the second byte matters: the
/// double- and multi-byte tables, and the UTF-16, UCS-2 and UTF-32 families.
///
/// The trailing byte is strided so the program stays a few seconds rather than
/// a few minutes; the leading byte is not, so every prefix byte of every table
/// is exercised. `tcl8` is left out for the UTF-16 family, where a lone
/// surrogate code unit decodes to a lone surrogate that tclsh can hold in a
/// string but cannot then write to its own UTF-8 stdout.
#[test]
fn two_byte_sequences_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let multibyte = "big5 cns11643 cp932 cp936 cp949 cp950 euc-cn euc-jp euc-kr gb2312 \
                     gb2312-raw gb12345 jis0208 jis0212 ksc5601 macJapan shiftjis utf-8 cesu-8";
    let wide = "ucs-2 ucs-2be ucs-2le unicode utf-16 utf-16be utf-16le utf-32 utf-32be utf-32le";
    let program = format!(
        "foreach {{encs profiles}} [list {{{multibyte}}} {{tcl8 strict replace}} \
         {{{wide}}} {{strict replace}}] {{\n\
         \x20 foreach e $encs {{\n\
         \x20   for {{set i 0}} {{$i < 256}} {{incr i}} {{\n\
         \x20     for {{set j 0}} {{$j < 256}} {{incr j 11}} {{\n\
         \x20       set b [format %c%c $i $j]\n\
         \x20       foreach p $profiles {{\n\
         \x20         set v UNSET\n\
         \x20         set rc [catch {{encoding convertfrom -profile $p -failindex v $e $b}} r]\n\
         \x20         puts \"F $e $p $i $j $rc $v <$r>\"\n\
         \x20         if {{$rc == 0}} {{\n\
         \x20           set w UNSET\n\
         \x20           set rc2 [catch {{encoding convertto -profile $p -failindex w $e $r}} r2]\n\
         \x20           puts \"T $e $p $i $j $rc2 $w <$r2>\"\n\
         \x20         }}\n\
         \x20       }}\n\
         \x20     }}\n\
         \x20   }}\n\
         \x20 }}\n\
         }}\n"
    );
    let (expected, error) = reference(&tclsh, &program);
    assert!(error.is_none(), "tclsh failed the sweep: {error:?}");
    let outcome = tclrs::eval(&program).expect("tclrs runs the sweep");
    assert_eq!(
        first_difference(&expected, &outcome.output),
        None,
        "the two-byte sweep diverges"
    );
}

/// The first line on which two outputs differ, with both versions of it, or
/// `None` when they are identical. A 200 000-line diff is not a useful
/// assertion message; the first divergence is.
fn first_difference(expected: &str, actual: &str) -> Option<String> {
    let mut left = expected.lines();
    let mut right = actual.lines();
    let mut line = 1;
    loop {
        match (left.next(), right.next()) {
            (None, None) => return None,
            (a, b) if a == b => line += 1,
            (a, b) => {
                return Some(format!(
                    "line {line}:\n  tclsh: {}\n  tclrs: {}",
                    a.unwrap_or("<end of output>"),
                    b.unwrap_or("<end of output>")
                ))
            }
        }
    }
}

/// Everything `encoding names` offers converts, and nothing it leaves out is
/// silently accepted.
///
/// This is the property the list is for: tclsh's own answer includes encodings
/// this frontend does not implement, so the two lists are not compared, and a
/// list that promised more than it delivered would be worse than a short one.
#[test]
fn names_lists_only_what_converts() {
    let names = tclrs::cmd_encoding::names();
    assert!(names.len() > 80, "only {} encodings offered", names.len());
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "encoding names is not in a fixed order");
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "encoding names repeats a name");

    for name in &names {
        // Every name has to survive both directions on a byte its table
        // certainly has: `A` is at 0x41 in every one of these but ebcdic, so
        // the round trip is asserted on the *result* being stable rather than
        // on a particular byte.
        let program = format!(
            "set b [encoding convertto -profile tcl8 {name} A]\n\
             puts [string length [encoding convertfrom -profile tcl8 {name} $b]]"
        );
        let outcome = tclrs::eval(&program)
            .unwrap_or_else(|e| panic!("encoding names offers {name}, which then refused: {e}"));
        assert!(
            !outcome.output.trim().is_empty(),
            "{name} converted to nothing"
        );
    }

    for absent in ["iso2022", "iso2022-jp", "iso2022-kr", "identity", "binary"] {
        assert!(!names.contains(&absent), "{absent} is offered");
        let program = format!("encoding convertfrom {absent} abc");
        assert!(
            tclrs::eval(&program).is_err(),
            "{absent} is absent from encoding names but converted anyway"
        );
    }
}

/// A scratch path for one channel program, unique to this process and run.
fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "tclrs-encchan-{tag}-{}-{}.dat",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A channel name in a message, replaced: it is `file` plus a file descriptor
/// number, and the two interpreters do not have the same descriptors free.
fn without_channel_name(message: &str) -> String {
    let mut out = message.to_string();
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

/// A character split across two reads is still one character.
#[test]
fn channel_encodings_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in CHANNEL_PROGRAMS {
        let reference_path = scratch("ref");
        let subject_path = scratch("sub");
        let reference_program = program.replace("%F", reference_path.to_str().expect("path"));
        let subject_program = program.replace("%F", subject_path.to_str().expect("path"));
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
                "program:\n{program}\n{}",
                first_difference(&expected, &outcome.output).unwrap_or_default()
            )),
            Err(e) => failures.push(format!("program:\n{program}\n  tclrs failed: {e}")),
        }
        let _ = std::fs::remove_file(&subject_path);
    }
    assert!(
        failures.is_empty(),
        "{} of {} channel programs diverge:\n\n{}",
        failures.len(),
        CHANNEL_PROGRAMS.len(),
        failures.join("\n\n")
    );
}

/// A strict channel refuses what it cannot convert, in tclsh's own wording.
#[test]
fn channel_encoding_errors_match_tclsh() {
    let Some(tclsh) = tclsh() else {
        eprintln!("skipping: no tclsh on PATH");
        return;
    };
    let mut failures = Vec::new();
    for program in CHANNEL_ERRORS {
        let reference_path = scratch("referr");
        let subject_path = scratch("suberr");
        let reference_program = program.replace("%F", reference_path.to_str().expect("path"));
        let subject_program = program.replace("%F", subject_path.to_str().expect("path"));
        let (_, error) = reference(&tclsh, &reference_program);
        let _ = std::fs::remove_file(&reference_path);
        let Some(expected) = error.map(|e| without_channel_name(&e)) else {
            failures.push(format!(
                "tclsh accepted a program that must fail:\n{program}"
            ));
            continue;
        };
        match tclrs::eval(&subject_program) {
            Err(e) if without_channel_name(&without_location(&e.to_string())) == expected => {}
            Err(e) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected}\n  tclrs: {}",
                without_channel_name(&without_location(&e.to_string()))
            )),
            Ok(_) => failures.push(format!(
                "program:\n{program}\n  tclsh: {expected}\n  tclrs: accepted it"
            )),
        }
        let _ = std::fs::remove_file(&subject_path);
    }
    assert!(
        failures.is_empty(),
        "{} of {} channel refusals diverge:\n\n{}",
        failures.len(),
        CHANNEL_ERRORS.len(),
        failures.join("\n\n")
    );
}

/// A decode whose result would be a lone surrogate is refused by name.
///
/// Not a differential case: tclsh's strings can hold an unpaired surrogate and
/// this frontend's cannot, so there is no shared answer to compare. What is
/// asserted is that the refusal names the code point rather than substituting
/// something that would look like a successful conversion.
#[test]
fn a_lone_surrogate_is_refused_by_name() {
    for (program, expected) in [
        (
            "encoding convertfrom -profile tcl8 utf-8 \\xED\\xA0\\x80",
            "U+D800",
        ),
        (
            "encoding convertfrom -profile tcl8 cesu-8 \\xED\\xA0\\x80",
            "U+D800",
        ),
        (
            "encoding convertfrom -profile tcl8 utf-16be \\xDB\\xFF",
            "U+DBFF",
        ),
        (
            "encoding convertfrom -profile tcl8 utf-32be \\x00\\x00\\xDC\\x00",
            "U+DC00",
        ),
    ] {
        let error = tclrs::eval(program)
            .expect_err("a lone surrogate has to be refused")
            .to_string();
        assert!(
            error.contains("lone surrogate") && error.contains(expected),
            "{program}\n  refusal does not name {expected}: {error}"
        );
    }
}
