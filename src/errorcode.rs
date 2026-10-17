//! The `-errorcode` of an error a builtin raised.
//!
//! tclsh sets the code at the raise site (`Tcl_SetErrorCode`). tclrs raises
//! its builtin errors as their message text, which the differential suites hold
//! byte-identical to tclsh's, so the code is recovered from that text instead —
//! but only for a message template that Tcl 9.0.4 emits with exactly ONE code.
//! A template two raise sites share with different codes is left out: the
//! option is then absent, which is a visible gap, rather than present and
//! wrong. Left out for that reason:
//!
//! - `expected integer but got "x"` — `TCL VALUE INTEGER` from
//!   `generic/tclObj.c:2702` and `TCL VALUE NUMBER` from `tclStrToD.c:1540`.
//! - `can't read "x": no such variable` — `TCL LOOKUP VARNAME`
//!   (`tclVar.c:719`) and `TCL READ VARNAME` (`tclVar.c:1472`).
//! - `integer value too large to represent` — `ARITH IOVERFLOW`
//!   (`tclExecute.c:5986`) and `CLOCK dateTooLarge` (`tclClockFmt.c:2696`).
//! - `bad <what> "x": must be …` — `TCL LOOKUP INDEX <what> x` from
//!   `Tcl_GetIndexFromObj` (`tclIndexObj.c:360`), but `clock`'s own option
//!   errors carry `CLOCK badOption` (`tclClock.c:3516`) and `bad completion
//!   code` carries `TCL RESULT ILLEGAL_CODE` (`tclIndexObj.c:1388`).
//!
//! The code is built as a Tcl list, so an element holding a space (a command
//! name, a dictionary key) keeps its structure.

/// The code tclsh attaches to `msg`, when the template determines it.
pub(crate) fn classify(msg: &str) -> Option<String> {
    let code: Vec<&str> = if msg.starts_with("wrong # args:") {
        // `Tcl_WrongNumArgs` (`tclIndexObj.c:962`) and the `if` parser's own
        // "wrong # args: …" forms (`tclCmdIL.c:226-361`).
        vec!["TCL", "WRONGARGS"]
    } else if msg == "divide by zero" {
        // `tclExecute.c:7523`.
        vec!["ARITH", "DIVZERO", msg]
    } else if msg == "domain error: argument not in valid range" {
        // `tclExecute.c:5915`, `tclBasic.c:7319`.
        vec!["ARITH", "DOMAIN", msg]
    } else if let Some(desc) = operand_description(msg) {
        // `IllegalExprOperandType` (`tclExecute.c:9095-9121`).
        vec!["ARITH", "DOMAIN", desc]
    } else if let Some(name) = quoted(msg, "invalid command name \"", "\"") {
        // `tclBasic.c:4999`, `tclExecute.c:4384`, `tclNamesp.c:4049`.
        vec!["TCL", "LOOKUP", "COMMAND", name]
    } else if let Some(name) = quoted(msg, "can't rename \"", "\": command doesn't exist")
        .or_else(|| quoted(msg, "can't delete \"", "\": command doesn't exist"))
    {
        // `tclBasic.c:3179`.
        vec!["TCL", "LOOKUP", "COMMAND", name]
    } else if let Some(key) = quoted(msg, "key \"", "\" not known in dictionary") {
        // `tclDictObj.c:814`, `:1774`, `tclExecute.c:6818`.
        vec!["TCL", "LOOKUP", "DICT", key]
    } else if let Some(name) = quoted(msg, "can not find channel named \"", "\"") {
        // `tclIO.c:1467`.
        vec!["TCL", "LOOKUP", "CHANNEL", name]
    } else if let Some(name) = quoted(msg, "unknown encoding \"", "\"") {
        // `tclEncoding.c:1849`.
        vec!["TCL", "LOOKUP", "ENCODING", name]
    } else if let Some(sub) = msg
        .strip_prefix("unknown or ambiguous subcommand \"")
        .and_then(|rest| rest.split_once("\": must be "))
        .map(|(sub, _)| sub)
    {
        // The ensemble dispatcher (`tclEnsemble.c:2013`).
        vec!["TCL", "LOOKUP", "SUBCOMMAND", sub]
    } else if msg.starts_with("bad index \"")
        && msg.ends_with("\": must be integer?[+-]integer? or end?[+-]integer?")
    {
        // `TclIndexEncode`'s parse failure (`tclUtil.c:3766`).
        vec!["TCL", "VALUE", "INDEX"]
    } else if msg.starts_with("expected number but got ") {
        // `TclParseNumber` asked for a "number" (`tclStrToD.c:1540`).
        vec!["TCL", "VALUE", "NUMBER"]
    } else if quoted(msg, "can't read \"", "\": variable is array").is_some() {
        // `ISARRAY` under "read" is raised only by `TclPtrGetVarIdx`
        // (`tclVar.c:1459`), whose error exit sets `TCL READ VARNAME` (`:1472`).
        vec!["TCL", "READ", "VARNAME"]
    } else if quoted(msg, "can't set \"", "\": variable is array").is_some() {
        // `ISARRAY` under "set" is raised only by `TclPtrSetVarIdx`
        // (`tclVar.c:1977`); its `WRITE ARRAY` is overwritten on the way out by
        // the `cleanup` label's `TCL WRITE VARNAME` (`:2073`), which is what
        // tclsh 9.0.4 reports (measured: `set a 1` on an array).
        vec!["TCL", "WRITE", "VARNAME"]
    } else if quoted(
        msg,
        "bad completion code \"",
        "\": must be ok, error, return, break, continue, or an integer",
    )
    .is_some()
    {
        // `TclGetCompletionCodeFromObj` (`tclIndexObj.c:1384-1387`).
        vec!["TCL", "RESULT", "ILLEGAL_CODE"]
    } else if msg.starts_with("bad -level value: expected non-negative integer but got \"") {
        // `TclMergeReturnOptions` (`tclResult.c:904-906`).
        vec!["TCL", "RESULT", "ILLEGAL_LEVEL"]
    } else if let Some(name) = quoted(msg, "\"", "\" isn't an array") {
        // `NotArrayError` (`tclVar.c:358-361`), the template's one raise site.
        vec!["TCL", "LOOKUP", "ARRAY", name]
    } else if let Some(handle) = quoted(msg, "illegal search identifier \"", "\"")
        .or_else(|| quoted(msg, "couldn't find search \"", "\""))
        .or_else(|| {
            msg.strip_prefix("search identifier \"")
                .and_then(|r| r.split_once("\" isn't for variable \""))
                .filter(|(_, v)| v.ends_with('"'))
                .map(|(h, _)| h)
        })
    {
        // `ParseSearchId` (`tclVar.c:5326-5338`).
        vec!["TCL", "LOOKUP", "ARRAYSEARCH", handle]
    } else if let Some(code) = list_parse(msg) {
        return Some(code);
    } else if msg == "missing value to go with key" {
        // `SetDictFromAny` (`tclDictObj.c:719-720`).
        vec!["TCL", "VALUE", "DICTIONARY"]
    } else if quoted(msg, "illegal access mode \"", "\"").is_some() {
        // `TclGetOpenMode` (`tclIOUtil.c:1519`).
        vec!["TCL", "OPENMODE", "INVALID"]
    } else if let Some((name, text)) = posix(msg) {
        // `Tcl_PosixError`: `POSIX <errno name> <message>`.
        return Some(crate::list::join(&["POSIX", name, text.as_str()]));
    } else {
        return None;
    };
    Some(crate::list::join(&code))
}

/// `FindElement`'s refusals (`tclUtil.c:682-777`): `TCL VALUE <type> <what>`,
/// where the type is `LIST` or `DICTIONARY` as the message's own `list` or
/// `dict` says, and what went wrong is `JUNK` after a closing brace or quote,
/// or `BRACE` / `QUOTE` for one never closed.
fn list_parse(msg: &str) -> Option<String> {
    let (kind, rest) = if let Some(rest) = msg.strip_prefix("list ") {
        ("LIST", rest)
    } else if let Some(rest) = msg.strip_prefix("dict ") {
        ("DICTIONARY", rest)
    } else {
        let (what, typ) = msg.strip_prefix("unmatched open ")?.split_once(" in ")?;
        let kind = match typ {
            "list" => "LIST",
            "dict" => "DICTIONARY",
            _ => return None,
        };
        let what = match what {
            "brace" => "BRACE",
            "quote" => "QUOTE",
            _ => return None,
        };
        return Some(format!("TCL VALUE {kind} {what}"));
    };
    let junk = (rest.starts_with("element in braces followed by \"")
        || rest.starts_with("element in quotes followed by \""))
        && rest.ends_with("\" instead of space");
    junk.then(|| format!("TCL VALUE {kind} JUNK"))
}

/// The `REG_*` name `regerror` words as `reason` (`generic/regerrs.h`).
const REGEX_ERRORS: &[(&str, &str)] = &[
    ("invalid regexp (reg version 0.8)", "REG_BADPAT"),
    ("invalid collating element", "REG_ECOLLATE"),
    ("invalid character class", "REG_ECTYPE"),
    ("invalid escape \\ sequence", "REG_EESCAPE"),
    ("invalid backreference number", "REG_ESUBREG"),
    ("brackets [] not balanced", "REG_EBRACK"),
    ("parentheses () not balanced", "REG_EPAREN"),
    ("braces {} not balanced", "REG_EBRACE"),
    ("invalid repetition count(s)", "REG_BADBR"),
    ("invalid character range", "REG_ERANGE"),
    ("out of memory", "REG_ESPACE"),
    ("invalid quantifier operand", "REG_BADRPT"),
    ("invalid argument to regex function", "REG_INVARG"),
    ("character widths of regex and string differ", "REG_MIXED"),
    ("invalid embedded option", "REG_BADOPT"),
    ("regular expression too big", "REG_ETOOBIG"),
    ("too many colors", "REG_ECOLORS"),
    ("can't match", "REG_CANTMATCH"),
];

/// [`classify`], for the templates whose code depends on where the error was
/// raised: whether the command that failed ran in a procedure body — a local
/// variable is found by slot there and by name elsewhere, and the two raise
/// different codes — and which command it was.
///
/// Anything this still cannot place is `None`, and the error carries tclsh's
/// default code.
pub(crate) fn classify_in(msg: &str, in_proc: bool, command: &str) -> Option<String> {
    if command == "clock" && msg.starts_with("wrong # args: should be \"clock ") {
        if let Some(code) = clock_code(msg.lines().next().unwrap_or(msg)) {
            return Some(code);
        }
    }
    if let Some(code) = classify(msg) {
        return Some(code);
    }
    let first = msg.lines().next().unwrap_or(msg);
    // `Tcl_ParseExpr`'s own refusals: the message carries the expression after a
    // line break, with the offset marked.
    if msg.contains("\nin expression \"") {
        let kind = if first.starts_with("missing operand") || first.starts_with("missing operator")
        {
            "MISSING"
        } else if first == "empty expression" || first.starts_with("empty subexpression") {
            "EMPTY"
        } else if first.starts_with("unbalanced")
            || first.starts_with("missing close-bracket")
            || first == "missing \""
            || first == "missing )"
        {
            "UNBALANCED"
        } else if first.starts_with("invalid character") {
            "BADCHAR"
        } else if first.starts_with("invalid bareword") {
            if msg.ends_with("(invalid binary number?)") {
                "BADNUMBER BINARY"
            } else if msg.ends_with("(invalid octal number?)") {
                "BADNUMBER OCTAL"
            } else if msg.ends_with("(invalid hexadecimal number?)") {
                "BADNUMBER HEXADECIMAL"
            } else {
                "BAREWORD"
            }
        } else {
            return None;
        };
        return Some(format!("TCL PARSE EXPR {kind}"));
    }
    if first.starts_with("not enough arguments for math function")
        || first.starts_with("too many arguments for math function")
    {
        return Some("TCL WRONGARGS".to_string());
    }
    if let Some(name) = quoted(msg, "can't read \"", "\": no such variable") {
        // A local read is by slot (`TclObjVarErrMsg` from `TclPtrGetVarIdx`); a
        // global one is found by name.
        return Some(if in_proc {
            "TCL READ VARNAME".to_string()
        } else {
            crate::list::join(&[
                "TCL",
                "LOOKUP",
                "VARNAME",
                name.split('(').next().unwrap_or(name),
            ])
        });
    }
    if quoted(msg, "can't read \"", "\": no such element in array").is_some() {
        return Some("TCL READ VARNAME".to_string());
    }
    if let Some(name) = quoted(msg, "can't unset \"", "\": no such variable") {
        return Some(if in_proc {
            "TCL UNSET VARNAME".to_string()
        } else {
            crate::list::join(&["TCL", "LOOKUP", "VARNAME", name])
        });
    }
    if let Some(element) = quoted(msg, "can't unset \"", "\": no such element in array")
        .and_then(|n| n.split_once('(').map(|(_, e)| e.trim_end_matches(')')))
    {
        return Some(crate::list::join(&["TCL", "LOOKUP", "ELEMENT", element]));
    }
    if let Some(text) = quoted(msg, "expected integer but got \"", "\"") {
        // A value that is a number but not an integer is refused by a different
        // site than a word that is no number at all.
        let numeric = crate::list::double(text).is_ok();
        return Some(
            if numeric {
                "TCL VALUE INTEGER"
            } else {
                "TCL VALUE NUMBER"
            }
            .to_string(),
        );
    }
    if first.starts_with("expected floating-point number but got ") {
        return Some("TCL VALUE NUMBER".to_string());
    }
    if let Some(code) = operation_code(first, command) {
        return Some(code);
    }
    if first == "format string ended in middle of field specifier" {
        return Some("TCL FORMAT INCOMPLETE".to_string());
    }
    if first.starts_with("can't interpret \"") && first.ends_with("\" as a lambda expression") {
        return Some("TCL VALUE LAMBDA".to_string());
    }
    if first == "yield can only be called in a coroutine" {
        return Some("TCL COROUTINE ILLEGAL_YIELD".to_string());
    }
    if let Some(reason) = msg.strip_prefix("cannot compile regular expression pattern: ") {
        if let Some((_, name)) = REGEX_ERRORS.iter().find(|(text, _)| *text == reason) {
            return Some(crate::list::join(&["REGEXP", name, reason]));
        }
    }
    if let Some(rest) = msg.strip_prefix("bad level \"") {
        if let Some(level) = rest.strip_suffix('"') {
            return Some(crate::list::join(&[
                "TCL",
                "LOOKUP",
                if command == "info" {
                    "STACK_LEVEL"
                } else {
                    "LEVEL"
                },
                level,
            ]));
        }
    }
    if let Some(name) = msg
        .strip_prefix("unknown namespace \"")
        .and_then(|r| r.split_once('"'))
        .map(|(name, _)| name)
    {
        return Some(crate::list::join(&["TCL", "LOOKUP", "NAMESPACE", name]));
    }
    // `Tcl_GetIndexFromObjStruct`'s refusal: `bad <what> "<value>": must be …`.
    // `clock`'s Tcl-level option errors are `CLOCK badOption` (see
    // [`clock_code`]); only its `unit` and `clicks` refusals come from here.
    let refused = msg
        .strip_prefix("bad ")
        .or_else(|| msg.strip_prefix("ambiguous "));
    if let Some((what, tail)) = refused.and_then(|r| r.split_once(" \"")) {
        if let Some((value, after)) = tail.split_once("\": must be ") {
            let own = command != "clock" || what == "unit" || after.starts_with("-milliseconds");
            if own && !what.contains(' ') && !after.is_empty() {
                return Some(crate::list::join(&["TCL", "LOOKUP", "INDEX", what, value]));
            }
        }
    }
    None
}

/// The codes of the refusals that name an operation rather than a lookup:
/// `TCL OPERATION <command> <what>` and its relatives, plus the `clock` and
/// `binary` families, each from the site that raises it.
fn operation_code(first: &str, command: &str) -> Option<String> {
    // `format` and `scan` raise these; `binary` words the first one the same
    // way and leaves the default code.
    let format_fixed = [
        (
            "not enough arguments for all format specifiers",
            "TCL FORMAT FIELDVARMISMATCH",
        ),
        (
            "\"%n$\" argument index out of range",
            "TCL FORMAT INDEXRANGE",
        ),
        (
            "cannot mix \"%\" and \"%n$\" conversion specifiers",
            "TCL FORMAT MIXEDSPECTYPES",
        ),
    ];
    if command != "binary" {
        if let Some((_, code)) = format_fixed.iter().find(|(text, _)| *text == first) {
            return Some((*code).to_string());
        }
    }
    let fixed = [
        (
            "list size must be a multiple of the stride length",
            "TCL OPERATION LSORT BADSTRIDE",
        ),
        (
            "list must have an even number of elements",
            "TCL ARGUMENT FORMAT",
        ),
        ("foreach varlist is empty", "TCL OPERATION FOREACH NEEDVARS"),
        (
            "extra switch pattern with no body",
            "TCL OPERATION SWITCH BADARM",
        ),
        (
            "finally clause must be last",
            "TCL OPERATION TRY FINALLY NONTERMINAL",
        ),
        (
            "wrong # args to finally clause: must be \"... finally script\"",
            "TCL OPERATION TRY FINALLY ARGUMENT",
        ),
        (
            "must have exactly two variable names",
            "TCL SYNTAX dict for",
        ),
    ];
    if let Some((_, code)) = fixed.iter().find(|(text, _)| *text == first) {
        return Some((*code).to_string());
    }
    if first.starts_with("expected boolean value but got \"") {
        return Some("TCL VALUE NUMBER".to_string());
    }
    if first.starts_with("bad scan conversion character") {
        return Some("TCL FORMAT BADTYPE".to_string());
    }
    if first.starts_with("element ") && first.contains(" missing from sublist ") {
        return Some("TCL OPERATION LSORT INDEXFAILED".to_string());
    }
    if first.starts_with("bad count \"") && first.ends_with("\": must be integer >= 0") {
        return Some("TCL OPERATION LREPEAT NEGARG".to_string());
    }
    if first.starts_with("too many fields in argument specifier ") {
        return Some("TCL OPERATION PROC FORMALARGUMENTFORMAT".to_string());
    }
    if let Some(rest) = first.strip_prefix("wrong # args to ") {
        for (clause, what) in [("on", "ON"), ("trap", "TRAP")] {
            if rest.starts_with(&format!("{clause} clause: must be ")) {
                return Some(format!("TCL OPERATION TRY {what} ARGUMENT"));
            }
        }
    }
    if let Some(value) = quoted(
        first,
        "bad handler type \"",
        "\": must be finally, on, or trap",
    ) {
        return Some(crate::list::join(&[
            "TCL",
            "LOOKUP",
            "INDEX",
            "handler type",
            value,
        ]));
    }
    if let Some(name) = quoted(first, "\"", "\" isn't a procedure") {
        return Some(crate::list::join(&["TCL", "LOOKUP", "PROCEDURE", name]));
    }
    if let Some(pattern) = quoted(first, "unknown namespace in import pattern \"", "\"") {
        return Some(crate::list::join(&["TCL", "LOOKUP", "NAMESPACE", pattern]));
    }
    if first.starts_with("missing argument to \"") && first.ends_with('"') {
        return Some("TCL ARGUMENT MISSING".to_string());
    }
    if first.starts_with("can't incr \"") && first.ends_with("\": variable is a constant") {
        return Some("TCL WRITE CONST".to_string());
    }
    if quoted(first, "can't unset \"", "\": variable is a constant").is_some() {
        return Some("TCL UNSET CONST".to_string());
    }
    if quoted(first, "can't set \"", "\": variable is a constant").is_some() {
        return Some("TCL WRITE VARNAME".to_string());
    }
    if first.starts_with("can't find package ") {
        return Some("TCL PACKAGE UNFOUND".to_string());
    }
    if first.starts_with("expected version number but got \"") {
        return Some("TCL VALUE VERSION".to_string());
    }
    if let Some(name) = first
        .strip_prefix("package ")
        .and_then(|r| r.strip_suffix(" is not present"))
    {
        return Some(crate::list::join(&["TCL", "LOOKUP", "PACKAGE", name]));
    }
    if command == "binary" {
        if first.starts_with("invalid ")
            && first.contains(" (U+")
            && first.contains(") at position ")
        {
            return Some("TCL BINARY DECODE INVALID".to_string());
        }
        if let Some(sub) = first
            .strip_prefix("unknown subcommand \"")
            .and_then(|r| r.split_once('"'))
        {
            return Some(crate::list::join(&["TCL", "LOOKUP", "SUBCOMMAND", sub.0]));
        }
    }
    if command == "clock" {
        return clock_code(first);
    }
    None
}

/// `clock`'s own codes (`library/clock.tcl`): a usage error is `CLOCK
/// wrongNumArgs` for the subcommands written in Tcl, and an option or a
/// base it cannot read is `CLOCK badOption`.
fn clock_code(first: &str) -> Option<String> {
    if let Some(rest) = first.strip_prefix("wrong # args: should be \"clock ") {
        let sub = rest.split_whitespace().next().unwrap_or_default();
        return matches!(sub, "format" | "scan" | "add").then(|| "CLOCK wrongNumArgs".to_string());
    }
    if first.contains("must be -milliseconds") {
        return None;
    }
    if let Some(name) = first
        .strip_prefix("bad option \"")
        .and_then(|r| r.split_once('"'))
    {
        return Some(crate::list::join(&["CLOCK", "badOption", name.0]));
    }
    if let Some(name) = first
        .strip_prefix("bad seconds \"")
        .and_then(|r| r.split_once('"'))
    {
        return Some(crate::list::join(&["CLOCK", "badOption", name.0]));
    }
    if let Some(zone) = quoted(first, "time zone \"", "\" not found") {
        return Some(crate::list::join(&["CLOCK", "badTimeZone", zone]));
    }
    if first.starts_with("unable to convert date-time string \"") {
        // `TclClockFreeScan`: a part found twice, or the grammar's refusal.
        let kind = if first.contains("\": more than one ") {
            "MULTIPLE"
        } else {
            "PARSE"
        };
        return Some(format!("TCL VALUE DATE {kind}"));
    }
    if let Some(what) = first.strip_prefix("unable to convert input string: ") {
        // `ClockValidDate`. The one message two of its checks share — `invalid
        // time` for a 24:mm:ss that is not midnight and for seconds out of
        // range — is given the code of the commoner, the seconds.
        let code = match what {
            "invalid iso year" => "iso year",
            "invalid year" | "ambiguous year" => "year",
            "invalid month" => "month",
            "invalid day" | "ambiguous day" => "day",
            "invalid day of year" => "day of year",
            "invalid day of week" => "day of week",
            "invalid time (hour)" => "hour",
            "invalid time (minutes)" => "minutes",
            "invalid time" => "seconds",
            "invalid time (does not exist in this time-zone)" => "out-of-time",
            _ => return None,
        };
        return Some(crate::list::join(&["CLOCK", "invInpStr", code]));
    }
    match first {
        "input string does not match supplied format" => Some("CLOCK badInputString".to_string()),
        "requested date too large to represent" => Some("CLOCK dateTooLarge".to_string()),
        "legacy [clock scan] does not support -locale" => {
            Some("CLOCK flagWithLegacyFormat".to_string())
        }
        _ => None,
    }
}

/// The text between `prefix` and `suffix` when `msg` is exactly that shape.
fn quoted<'a>(msg: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    msg.strip_prefix(prefix)?.strip_suffix(suffix)
}

/// The operand description in `cannot use <description> "<value>" as
/// ?left |right ?operand of "<op>"` and `cannot use a list as …`.
fn operand_description(msg: &str) -> Option<&'static str> {
    let rest = msg.strip_prefix("cannot use ")?;
    if rest.starts_with("a list as ") && rest.contains("operand of \"") {
        return Some("list");
    }
    [
        "non-numeric string",
        "non-numeric floating-point value",
        "floating-point value",
        "(big) integer",
    ]
    .into_iter()
    .find(|d| {
        rest.strip_prefix(d)
            .is_some_and(|r| r.starts_with(" \"") && r.contains("operand of \""))
    })
}

/// The errno a message's trailing `: <reason>` names, when the reason is the
/// system's `strerror` text for one — the form `Tcl_PosixError` reports.
fn posix(msg: &str) -> Option<(&'static str, String)> {
    let (_, reason) = msg.rsplit_once(": ")?;
    ERRNO_NAMES.iter().find_map(|&(code, name)| {
        let text = strerror(code)?;
        (text == reason).then_some((name, text))
    })
}

/// `strerror(code)`, lowercased as every tclrs message quotes it.
fn strerror(code: i32) -> Option<String> {
    // SAFETY: `strerror` returns a pointer to a static string for every errno
    // value; it is copied before returning.
    let text = unsafe { libc::strerror(code) };
    if text.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(text) }
            .to_string_lossy()
            .to_lowercase(),
    )
}

/// `Tcl_ErrnoId` (`generic/tclPosixStr.c`) for the errno values both macOS
/// and Linux define. `EWOULDBLOCK` and `EOPNOTSUPP` are left out: each shares
/// its value with another name on one of the two, and `Tcl_ErrnoId` reports
/// whichever its `switch` reaches first there.
const ERRNO_NAMES: &[(i32, &str)] = &[
    (libc::EPERM, "EPERM"),
    (libc::ENOENT, "ENOENT"),
    (libc::ESRCH, "ESRCH"),
    (libc::EINTR, "EINTR"),
    (libc::EIO, "EIO"),
    (libc::ENXIO, "ENXIO"),
    (libc::E2BIG, "E2BIG"),
    (libc::ENOEXEC, "ENOEXEC"),
    (libc::EBADF, "EBADF"),
    (libc::ECHILD, "ECHILD"),
    (libc::EAGAIN, "EAGAIN"),
    (libc::ENOMEM, "ENOMEM"),
    (libc::EACCES, "EACCES"),
    (libc::EFAULT, "EFAULT"),
    (libc::EBUSY, "EBUSY"),
    (libc::EEXIST, "EEXIST"),
    (libc::EXDEV, "EXDEV"),
    (libc::ENODEV, "ENODEV"),
    (libc::ENOTDIR, "ENOTDIR"),
    (libc::EISDIR, "EISDIR"),
    (libc::EINVAL, "EINVAL"),
    (libc::ENFILE, "ENFILE"),
    (libc::EMFILE, "EMFILE"),
    (libc::ENOTTY, "ENOTTY"),
    (libc::ETXTBSY, "ETXTBSY"),
    (libc::EFBIG, "EFBIG"),
    (libc::ENOSPC, "ENOSPC"),
    (libc::ESPIPE, "ESPIPE"),
    (libc::EROFS, "EROFS"),
    (libc::EMLINK, "EMLINK"),
    (libc::EPIPE, "EPIPE"),
    (libc::EDOM, "EDOM"),
    (libc::ERANGE, "ERANGE"),
    (libc::EDEADLK, "EDEADLK"),
    (libc::ENAMETOOLONG, "ENAMETOOLONG"),
    (libc::ENOLCK, "ENOLCK"),
    (libc::ENOSYS, "ENOSYS"),
    (libc::ENOTEMPTY, "ENOTEMPTY"),
    (libc::ELOOP, "ELOOP"),
    (libc::ENOTSOCK, "ENOTSOCK"),
    (libc::EDESTADDRREQ, "EDESTADDRREQ"),
    (libc::EMSGSIZE, "EMSGSIZE"),
    (libc::EPROTOTYPE, "EPROTOTYPE"),
    (libc::ENOPROTOOPT, "ENOPROTOOPT"),
    (libc::EPROTONOSUPPORT, "EPROTONOSUPPORT"),
    (libc::EAFNOSUPPORT, "EAFNOSUPPORT"),
    (libc::EADDRINUSE, "EADDRINUSE"),
    (libc::EADDRNOTAVAIL, "EADDRNOTAVAIL"),
    (libc::ENETDOWN, "ENETDOWN"),
    (libc::ENETUNREACH, "ENETUNREACH"),
    (libc::ENETRESET, "ENETRESET"),
    (libc::ECONNABORTED, "ECONNABORTED"),
    (libc::ECONNRESET, "ECONNRESET"),
    (libc::ENOBUFS, "ENOBUFS"),
    (libc::EISCONN, "EISCONN"),
    (libc::ENOTCONN, "ENOTCONN"),
    (libc::ETIMEDOUT, "ETIMEDOUT"),
    (libc::ECONNREFUSED, "ECONNREFUSED"),
    (libc::EHOSTDOWN, "EHOSTDOWN"),
    (libc::EHOSTUNREACH, "EHOSTUNREACH"),
    (libc::EALREADY, "EALREADY"),
    (libc::EINPROGRESS, "EINPROGRESS"),
];

#[cfg(test)]
mod tests {
    use super::classify;

    #[test]
    fn an_ambiguous_template_stays_unclassified() {
        assert_eq!(classify("expected integer but got \"x\""), None);
        assert_eq!(classify("can't read \"x\": no such variable"), None);
        assert_eq!(classify("bad option \"-x\": must be -a or -b"), None);
    }

    use super::classify_in;

    /// Each message and the code tclsh 9.0 reports for it, measured with
    /// `catch {...} m o; dict get $o -errorcode`.
    #[test]
    fn operation_templates_carry_their_codes() {
        let cases = [
            (
                "format",
                "not enough arguments for all format specifiers",
                "TCL FORMAT FIELDVARMISMATCH",
            ),
            (
                "format",
                "\"%n$\" argument index out of range",
                "TCL FORMAT INDEXRANGE",
            ),
            (
                "lsort",
                "element 3 missing from sublist \"a\"",
                "TCL OPERATION LSORT INDEXFAILED",
            ),
            (
                "lrepeat",
                "bad count \"-1\": must be integer >= 0",
                "TCL OPERATION LREPEAT NEGARG",
            ),
            (
                "foreach",
                "foreach varlist is empty",
                "TCL OPERATION FOREACH NEEDVARS",
            ),
            (
                "try",
                "bad handler type \"d\": must be finally, on, or trap",
                "TCL LOOKUP INDEX {handler type} d",
            ),
            (
                "info",
                "\"nosuch\" isn't a procedure",
                "TCL LOOKUP PROCEDURE nosuch",
            ),
            (
                "incr",
                "can't incr \"x\": variable is a constant",
                "TCL WRITE CONST",
            ),
            (
                "package",
                "can't find package nosuch",
                "TCL PACKAGE UNFOUND",
            ),
            (
                "clock",
                "wrong # args: should be \"clock format clockval\"",
                "CLOCK wrongNumArgs",
            ),
            (
                "clock",
                "wrong # args: should be \"clock seconds\"",
                "TCL WRONGARGS",
            ),
            (
                "clock",
                "bad option \"-bogus\": must be -format",
                "CLOCK badOption -bogus",
            ),
            (
                "clock",
                "bad unit \"u\": must be years",
                "TCL LOOKUP INDEX unit u",
            ),
            (
                "clock",
                "time zone \":Nowhere\" not found",
                "CLOCK badTimeZone :Nowhere",
            ),
            (
                "set",
                "can't read \"nosuch(x)\": no such variable",
                "TCL LOOKUP VARNAME nosuch",
            ),
        ];
        for (command, msg, code) in cases {
            assert_eq!(
                classify_in(msg, false, command).as_deref(),
                Some(code),
                "{msg}"
            );
        }
    }

    /// `binary` words the field-count refusal as `format` does and leaves the
    /// default code on it.
    #[test]
    fn binary_keeps_the_default_code_for_a_short_argument_list() {
        let msg = "not enough arguments for all format specifiers";
        assert_eq!(classify_in(msg, false, "binary"), None);
    }

    #[test]
    fn a_code_element_with_a_space_keeps_its_structure() {
        assert_eq!(
            classify("invalid command name \"a b\"").as_deref(),
            Some("TCL LOOKUP COMMAND {a b}")
        );
        assert_eq!(
            classify("couldn't open \"/x\": no such file or directory").as_deref(),
            Some("POSIX ENOENT {no such file or directory}")
        );
    }
}
