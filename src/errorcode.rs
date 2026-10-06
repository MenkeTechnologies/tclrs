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
