//! `file`, `glob`, `pwd` and `cd`.
//!
//! The path arithmetic — `dirname`, `tail`, `split`, `join`, `rootname`,
//! `extension`, `pathtype` — is a port of `SplitUnixPath`,
//! `TclpNativeJoinPath` and `TclGetExtension` in `generic/tclFileName.c` and
//! `generic/tclUtil.c`, because Tcl's answers for the awkward cases are not
//! what a fresh derivation gives: `file rootname /a/.hidden` is `/a/`,
//! `file split //a//b//` is `{//a b}` — a leading `//` is a root of its own —
//! and `file dirname a/` is `.` rather than `a`.
//!
//! `glob` walks the directory tree one pattern component at a time, as
//! `TclGlob` does, and applies the same two rules that make its answers differ
//! from a naive `readdir` filter: a component that has no special characters
//! is used literally rather than matched (so a name no `readdir` would list
//! still resolves), and a name beginning with `.` matches only a pattern that
//! also begins with `.` — which is why `glob .*` answers `. ..` and `.hidden`
//! while `glob *` answers neither.
//!
//! Refused rather than approximated: `file attributes`, `link`, `stat`,
//! `lstat`, `channels`, `system`, `tempfile`, `tempdir` and `volumes`, and
//! `glob`'s `-types` in its two-element attribute form. Each says so by name.

use std::path::Path;
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::to_tcl_string;

/// Extension opcode ids owned by this module. The inline operand is the number
/// of stack values the op consumes.
pub mod ext {
    pub use crate::compiler::ext::FILE_BASE as BASE;
    /// `[subcommand, arg …]` → the subcommand's result.
    pub const FILE: u16 = BASE;
    /// `[arg …]` → the matching names, as a list.
    pub const GLOB: u16 = BASE + 1;
    /// `[]` → the working directory.
    pub const PWD: u16 = BASE + 2;
    /// `[dir?]` → `""`, having changed the working directory.
    pub const CD: u16 = BASE + 3;
}

/// The command names this module claims, for the REPL's completion and for the
/// reference page.
pub const COMMANDS: &[&str] = &["cd", "file", "glob", "pwd"];

/// Every `file` subcommand, in the order the interpreter lists them when it
/// rejects one. The refused ones are listed because their presence decides
/// whether an abbreviation is ambiguous.
pub const SUBCOMMANDS: &[&str] = &[
    "atime",
    "attributes",
    "channels",
    "copy",
    "delete",
    "dirname",
    "executable",
    "exists",
    "extension",
    "home",
    "isdirectory",
    "isfile",
    "join",
    "link",
    "lstat",
    "mkdir",
    "mtime",
    "nativename",
    "normalize",
    "owned",
    "pathtype",
    "readable",
    "readlink",
    "rename",
    "rootname",
    "separator",
    "size",
    "split",
    "stat",
    "system",
    "tail",
    "tempdir",
    "tempfile",
    "tildeexpand",
    "type",
    "volumes",
    "writable",
];

/// The subcommands this module recognises and then refuses. They are in
/// [`SUBCOMMANDS`] so that an abbreviation of one is found ambiguous exactly
/// where tclsh finds it ambiguous, and refused here — while the command is
/// being compiled, as the `string` ensemble refuses its own unbuilt names — so
/// that the reference page, which asks the compiler rather than keeping a
/// second list, reports them as unbuilt.
const REFUSED: &[&str] = &[
    "attributes",
    "channels",
    "link",
    "lstat",
    "stat",
    "system",
    "tempdir",
    "tempfile",
    "volumes",
];

// ── compiling ────────────────────────────────────────────────────────────

pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    match name {
        "pwd" => {
            if !args.is_empty() {
                return c.error("wrong # args: should be \"pwd\"");
            }
            c.emit(Op::Extended(ext::PWD, 0), 1);
            Ok(())
        }
        "cd" => {
            if args.len() > 1 {
                return c.error("wrong # args: should be \"cd ?dirName?\"");
            }
            match args.first() {
                Some(w) => c.word(w)?,
                // The absent argument is the home directory, and travels as
                // the empty string so the handler has one shape.
                None => c.push_str(""),
            }
            c.emit(Op::Extended(ext::CD, 1), 0);
            Ok(())
        }
        "glob" => words_op(c, ext::GLOB, args),
        _ => {
            let Some(first) = args.first() else {
                return c.error("wrong # args: should be \"file subcommand ?arg ...?\"");
            };
            // The subcommand is resolved here so an unknown one is reported
            // without the command running, as `Tcl_GetIndexFromObj` does; the
            // arguments all travel as values.
            let given = c.literal_of(first, "subcommand")?.to_string();
            let Some(sub) = resolve(&given, SUBCOMMANDS) else {
                return c.error(format!(
                    "unknown or ambiguous subcommand \"{given}\": must be {}",
                    listing(SUBCOMMANDS)
                ));
            };
            if REFUSED.contains(&sub) {
                return c.error(format!(
                    "file {sub} is not supported yet: it needs an interface this frontend has not built"
                ));
            }
            c.push_str(sub);
            for w in &args[1..] {
                c.word(w)?;
            }
            let Ok(argc) = u8::try_from(args.len()) else {
                return c.error("too many arguments for one command");
            };
            c.emit(Op::Extended(ext::FILE, argc), 1 - args.len() as i32);
            Ok(())
        }
    }
}

fn words_op(c: &mut Compiler, id: u16, args: &[Word]) -> Result<(), CompileError> {
    let Ok(argc) = u8::try_from(args.len()) else {
        return c.error("too many arguments for one command");
    };
    for w in args {
        c.word(w)?;
    }
    c.emit(Op::Extended(id, argc), 1 - args.len() as i32);
    Ok(())
}

/// `Tcl_GetIndexFromObj`'s rule: an exact match wins, otherwise a prefix that
/// fits exactly one entry.
fn resolve<'t>(name: &str, table: &[&'t str]) -> Option<&'t str> {
    if let Some(exact) = table.iter().find(|c| **c == name) {
        return Some(exact);
    }
    let mut hit = None;
    for candidate in table {
        if candidate.starts_with(name) {
            if hit.is_some() {
                return None;
            }
            hit = Some(*candidate);
        }
    }
    hit
}

fn listing(table: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in table.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if i + 1 == table.len() {
            out.push_str("or ");
        }
        out.push_str(name);
    }
    out
}

// ── path arithmetic ──────────────────────────────────────────────────────

/// `SplitUnixPath` (`generic/tclFileName.c`): the root, when there is one, is
/// a single element, and `//name` is a root of its own — the network-path
/// prefix — while `///` is not.
fn split_path(path: &str) -> Vec<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    if bytes.first() == Some(&b'/') {
        at = 1;
        if bytes.get(1) == Some(&b'/') && bytes.get(2).is_some_and(|b| *b != b'/') {
            at = 2;
            while at < bytes.len() && bytes[at] != b'/' {
                at += 1;
            }
        }
        out.push(path[..at].to_string());
        while bytes.get(at) == Some(&b'/') {
            at += 1;
        }
    }
    for part in path[at..].split('/') {
        if !part.is_empty() {
            out.push(part.to_string());
        }
    }
    out
}

fn is_root(element: &str) -> bool {
    element.starts_with('/')
}

/// The separator a root element begins with: `//` only for the network-path
/// prefix `//name`, and `/` for every other run of slashes. This is the same
/// distinction `SplitUnixPath` draws, and it is why `file join // a` is `/a`
/// while `file join //a b` is `//a/b`.
fn root_prefix(element: &str) -> &'static str {
    let bytes = element.as_bytes();
    if bytes.starts_with(b"//") && bytes.get(2).is_some_and(|b| *b != b'/') {
        "//"
    } else {
        "/"
    }
}

/// `TclpNativeJoinPath`: an element that is itself absolute discards
/// everything to its left, and duplicate and trailing slashes are dropped.
fn join_paths(parts: &[String]) -> String {
    let mut out = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if is_root(part) {
            out.clear();
        } else if !out.is_empty() && !out.ends_with('/') {
            out.push('/');
        }
        let mut needs_separator = false;
        let bytes = part.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'/' {
                while bytes.get(i + 1) == Some(&b'/') {
                    i += 1;
                }
                if i + 1 < bytes.len() && needs_separator {
                    out.push('/');
                }
            } else {
                out.push(bytes[i] as char);
                needs_separator = true;
            }
            i += 1;
        }
        // A root element keeps its leading separator, which the loop above
        // dropped with the duplicates. `//name` is the one case that keeps
        // two — `//` and `///` are both just the root.
        if is_root(part) {
            out.insert_str(0, root_prefix(part));
        }
    }
    out
}

/// `TclGetExtension`: the last `.` at or after the last `/`, and nothing when
/// there is none.
fn extension_at(path: &str) -> Option<usize> {
    let dot = path.rfind('.')?;
    match path.rfind('/') {
        Some(slash) if slash > dot => None,
        _ => Some(dot),
    }
}

fn dirname(path: &str) -> String {
    let parts = split_path(path);
    match parts.len() {
        0 => ".".to_string(),
        1 if is_root(&parts[0]) => parts[0].clone(),
        1 => ".".to_string(),
        _ => join_paths(&parts[..parts.len() - 1]),
    }
}

fn tail(path: &str) -> String {
    let parts = split_path(path);
    match parts.last() {
        Some(last) if !is_root(last) => last.clone(),
        _ => String::new(),
    }
}

/// `file pathtype`. Unix has no volume-relative paths, so there are two
/// answers rather than three.
fn path_type(path: &str) -> &'static str {
    if path.starts_with('/') {
        "absolute"
    } else {
        "relative"
    }
}

/// `file normalize`: make the path absolute, then resolve every symbolic link
/// along it **except in the last component**, applying `.` and `..` as they
/// are reached rather than lexically.
///
/// Both halves of that are measured, not assumed. `file normalize /etc` is
/// `/etc` in tclsh 9.0.4 even though `/etc` is a link, while
/// `file normalize /etc/hosts` is `/private/etc/hosts`; and
/// `file normalize /tmp/..` is `/private` rather than `/`, because the link is
/// resolved before the `..` is applied. A component that does not exist is
/// kept as written, so normalizing the path of a file yet to be created still
/// answers.
fn normalize(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    let parts = if path.starts_with('/') {
        split_path(path)
    } else {
        let mut base = split_path(&working_directory()?);
        base.extend(split_path(path));
        base
    };
    let mut parts = parts.into_iter();
    let root = match parts.next() {
        Some(first) if is_root(&first) => first,
        Some(first) => {
            // The working directory is absolute, so this is only reachable
            // for a relative path with no directory at all.
            let mut rest: Vec<String> = vec![first];
            rest.extend(parts);
            return Ok(rest.join("/"));
        }
        None => return Ok("/".to_string()),
    };
    let mut components: Vec<String> = parts.collect();
    let last = components.pop();
    let mut out = root;
    let mut queue: Vec<String> = components.into_iter().rev().collect();
    let mut steps = 0;
    while let Some(part) = queue.pop() {
        match part.as_str() {
            "." => continue,
            ".." => {
                pop_component(&mut out);
                continue;
            }
            _ => {}
        }
        let candidate = appended(&out, &part);
        match std::fs::read_link(&candidate) {
            // A link is followed, and its own target may be a path with links
            // of its own, so the target's components go back on the queue.
            Ok(target) if steps < 64 => {
                steps += 1;
                let target = target.to_string_lossy().into_owned();
                let pieces = split_path(&target);
                if target.starts_with('/') {
                    out = pieces.first().cloned().unwrap_or_else(|| "/".to_string());
                    queue.extend(pieces.into_iter().skip(1).rev());
                } else {
                    queue.extend(pieces.into_iter().rev());
                }
            }
            _ => out = candidate,
        }
    }
    // The last component is never resolved, which is what keeps
    // `file normalize /etc` at `/etc`.
    match last.as_deref() {
        None | Some(".") => {}
        Some("..") => pop_component(&mut out),
        Some(name) => out = appended(&out, name),
    }
    Ok(out)
}

/// Append one component to a path that already carries its root.
fn appended(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Remove the last component, leaving the root alone.
fn pop_component(path: &mut String) {
    match path.rfind('/') {
        Some(0) => path.truncate(1),
        Some(at) if !path[..at].ends_with('/') => path.truncate(at),
        _ => {}
    }
}

/// The `~` and `~user` expansions `file tildeexpand` performs. Tcl 9 no longer
/// does this anywhere else — `file normalize ~` keeps the tilde — so this is
/// the only place it happens.
pub(crate) fn expand_tilde(path: &str) -> Result<String, String> {
    let Some(rest) = path.strip_prefix('~') else {
        return Ok(path.to_string());
    };
    let (name, tail) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let home = if name.is_empty() {
        std::env::var("HOME").map_err(|_| "couldn't find HOME environment variable".to_string())?
    } else {
        home_of(name).ok_or_else(|| format!("user \"{name}\" doesn't exist"))?
    };
    Ok(format!("{home}{tail}"))
}

/// A named user's home directory, read from the password database the same way
/// `TclpGetUserHome` does.
fn home_of(user: &str) -> Option<String> {
    let name = std::ffi::CString::new(user).ok()?;
    // SAFETY: `getpwnam` is handed a NUL-terminated name and its result is
    // read before any other call that could reuse the static buffer.
    unsafe {
        let entry = libc::getpwnam(name.as_ptr());
        if entry.is_null() {
            return None;
        }
        let dir = (*entry).pw_dir;
        if dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(dir).to_string_lossy().into_owned())
    }
}

// The working directory a script sees, which is not always the one `getcwd`
// reports: tclsh caches the *normalized* path it was told to change to, and
// normalizing leaves a link in the last component alone. So `cd /tmp; pwd` is
// `/tmp` there while `getcwd` answers `/private/tmp`. Set by `cd` and read by
// `pwd`; the process directory is changed either way, so a relative path
// still resolves through the operating system.
thread_local! {
    static LOGICAL_CWD: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn working_directory() -> Result<String, String> {
    if let Some(cached) = LOGICAL_CWD.with(|c| c.borrow().clone()) {
        return Ok(cached);
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("error getting working directory name: {}", posix(&e)))
}

// ── errors ───────────────────────────────────────────────────────────────

/// `Tcl_PosixError`'s text for an errno — the C library's own `strerror`, which
/// is where tclsh's wording comes from.
fn posix(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        // SAFETY: `strerror` returns a pointer to a static string for every
        // errno value, and the result is copied before returning.
        Some(code) => unsafe {
            let text = libc::strerror(code);
            if text.is_null() {
                return "unknown error".to_string();
            }
            std::ffi::CStr::from_ptr(text)
                .to_string_lossy()
                .to_lowercase()
        },
        None => error.to_string(),
    }
}

fn could_not_read(path: &str, error: &std::io::Error) -> String {
    format!("could not read \"{path}\": {}", posix(error))
}

// ── running ──────────────────────────────────────────────────────────────

pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    let mut words = Vec::with_capacity(arg as usize);
    for _ in 0..arg {
        words.push(to_tcl_string(&vm.pop()));
    }
    words.reverse();
    let value = match id {
        ext::PWD => Value::Str(Arc::new(working_directory()?)),
        ext::CD => {
            let target = if words[0].is_empty() {
                std::env::var("HOME")
                    .map_err(|_| "couldn't find HOME environment variable".to_string())?
            } else {
                words[0].clone()
            };
            std::env::set_current_dir(&target).map_err(|e| {
                format!(
                    "couldn't change working directory to \"{target}\": {}",
                    posix(&e)
                )
            })?;
            let normalized = normalize(&target)?;
            LOGICAL_CWD.with(|c| *c.borrow_mut() = Some(normalized));
            Value::Str(Arc::new(String::new()))
        }
        ext::GLOB => Value::Str(Arc::new(run_glob(&words)?)),
        _ => run_file(&words)?,
    };
    vm.push(value);
    Ok(())
}

/// A subcommand that takes exactly one path.
fn one(words: &[String], sub: &str) -> Result<String, String> {
    match words.len() {
        2 => Ok(words[1].clone()),
        _ => Err(format!("wrong # args: should be \"file {sub} name\"")),
    }
}

fn run_file(words: &[String]) -> Result<Value, String> {
    let sub = words[0].as_str();
    let text = |s: String| Value::Str(Arc::new(s));
    match sub {
        "dirname" => Ok(text(dirname(&one(words, sub)?))),
        "tail" => Ok(text(tail(&one(words, sub)?))),
        "rootname" => {
            let path = one(words, sub)?;
            Ok(text(match extension_at(&path) {
                Some(at) => path[..at].to_string(),
                None => path,
            }))
        }
        "extension" => {
            let path = one(words, sub)?;
            Ok(text(match extension_at(&path) {
                Some(at) => path[at..].to_string(),
                None => String::new(),
            }))
        }
        // `nativename` is `join` of the split path on this platform: it drops
        // duplicate and trailing separators and nothing else.
        "nativename" => {
            let path = one(words, sub)?;
            Ok(text(join_paths(&split_path(&path))))
        }
        "normalize" => Ok(text(normalize(&one(words, sub)?)?)),
        "pathtype" => Ok(text(path_type(&one(words, sub)?).to_string())),
        "tildeexpand" => Ok(text(expand_tilde(&one(words, sub)?)?)),
        "split" => Ok(text(crate::list::join(&split_path(&one(words, sub)?)))),
        "join" => {
            if words.len() < 2 {
                return Err("wrong # args: should be \"file join name ?name ...?\"".to_string());
            }
            Ok(text(join_paths(&words[1..])))
        }
        "separator" => {
            if words.len() > 2 {
                return Err("wrong # args: should be \"file separator ?name?\"".to_string());
            }
            Ok(text("/".to_string()))
        }
        "home" => match words.len() {
            1 => Ok(text(std::env::var("HOME").map_err(|_| {
                "couldn't find HOME environment variable".to_string()
            })?)),
            2 => Ok(text(home_of(&words[1]).ok_or_else(|| {
                format!("user \"{}\" doesn't exist", words[1])
            })?)),
            _ => Err("wrong # args: should be \"file home ?user?\"".to_string()),
        },
        "exists" | "isdirectory" | "isfile" => {
            let path = one(words, sub)?;
            let answer = match sub {
                "exists" => std::fs::metadata(&path).is_ok(),
                "isdirectory" => std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
                _ => std::fs::metadata(&path).is_ok_and(|m| m.is_file()),
            };
            Ok(Value::Int(answer as i64))
        }
        "readable" | "writable" | "executable" => {
            let path = one(words, sub)?;
            let mode = match sub {
                "readable" => libc::R_OK,
                "writable" => libc::W_OK,
                _ => libc::X_OK,
            };
            Ok(Value::Int(accessible(&path, mode) as i64))
        }
        "owned" => {
            let path = one(words, sub)?;
            // SAFETY: `geteuid` takes no arguments and cannot fail.
            let me = unsafe { libc::geteuid() };
            let owned = stat_of(&path).ok().is_some_and(|s| s.st_uid == me);
            Ok(Value::Int(owned as i64))
        }
        "size" | "mtime" | "atime" => {
            let path = one(words, sub)?;
            let s = stat_of(&path)?;
            Ok(Value::Int(match sub {
                "size" => s.st_size,
                "mtime" => s.st_mtime,
                _ => s.st_atime,
            }))
        }
        "type" => {
            let path = one(words, sub)?;
            Ok(text(kind_of(&path)?.to_string()))
        }
        "readlink" => {
            let path = one(words, sub)?;
            let target = std::fs::read_link(&path)
                .map_err(|e| format!("could not read link \"{path}\": {}", posix(&e)))?;
            Ok(text(target.to_string_lossy().into_owned()))
        }
        "mkdir" => {
            for path in &words[1..] {
                make_directory(Path::new(path))?;
            }
            Ok(text(String::new()))
        }
        "delete" => run_delete(&words[1..]).map(text),
        "copy" | "rename" => run_transfer(sub, &words[1..]).map(text),
        // Unreachable through the compiler, which refuses every name in
        // `REFUSED` before the op is emitted. Kept as the same wording so a
        // future caller that reaches the handler another way gets one answer
        // and not two.
        other => Err(format!(
            "file {other} is not supported yet: it needs an interface this frontend has not built"
        )),
    }
}

fn accessible(path: &str, mode: libc::c_int) -> bool {
    let Ok(name) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: `access` is handed a NUL-terminated path and a mode constant.
    unsafe { libc::access(name.as_ptr(), mode) == 0 }
}

fn stat_of(path: &str) -> Result<libc::stat, String> {
    std::fs::metadata(path).map_err(|e| could_not_read(path, &e))?;
    let name = std::ffi::CString::new(path).map_err(|_| format!("could not read \"{path}\""))?;
    // SAFETY: `stat` writes into the zeroed buffer and is given a
    // NUL-terminated path; its return value is checked before the buffer is
    // read.
    unsafe {
        let mut buffer: libc::stat = std::mem::zeroed();
        if libc::stat(name.as_ptr(), &mut buffer) != 0 {
            return Err(could_not_read(path, &std::io::Error::last_os_error()));
        }
        Ok(buffer)
    }
}

/// `file type`'s answers, which are the seven `Tcl_FSLstat` distinguishes.
fn kind_of(path: &str) -> Result<&'static str, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| could_not_read(path, &e))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Ok("link");
    }
    if kind.is_dir() {
        return Ok("directory");
    }
    if kind.is_file() {
        return Ok("file");
    }
    use std::os::unix::fs::FileTypeExt;
    Ok(if kind.is_fifo() {
        "fifo"
    } else if kind.is_socket() {
        "socket"
    } else if kind.is_block_device() {
        "blockSpecial"
    } else if kind.is_char_device() {
        "characterSpecial"
    } else {
        "file"
    })
}

/// `file mkdir`, which creates every missing parent and is silent about a
/// directory that already exists.
fn make_directory(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(format!(
                "can't create directory \"{}\": file already exists",
                path.display()
            ))
        }
        Err(_) => {}
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            make_directory(parent)?;
        }
    }
    std::fs::create_dir(path).map_err(|e| {
        format!(
            "can't create directory \"{}\": {}",
            path.display(),
            posix(&e)
        )
    })
}

/// `file delete ?-force? ?--? name …`. A name that does not exist is not an
/// error, which is what makes the command usable in a cleanup script.
fn run_delete(args: &[String]) -> Result<String, String> {
    let (force, names) = options_prefix(args);
    for name in names {
        let Ok(meta) = std::fs::symlink_metadata(name) else {
            continue;
        };
        let outcome = if meta.is_dir() {
            if force {
                std::fs::remove_dir_all(name)
            } else {
                std::fs::remove_dir(name)
            }
        } else {
            std::fs::remove_file(name)
        };
        outcome.map_err(|e| format!("error deleting \"{name}\": {}", posix(&e)))?;
    }
    Ok(String::new())
}

/// `file copy` and `file rename`, in the two shapes both take: a single
/// source and target, or several sources into an existing directory.
fn run_transfer(sub: &str, args: &[String]) -> Result<String, String> {
    let (force, names) = options_prefix(args);
    if names.len() < 2 {
        return Err(format!(
            "wrong # args: should be \"file {sub} ?-option value ...? source ?source ...? target\""
        ));
    }
    let (sources, target) = names.split_at(names.len() - 1);
    let target = &target[0];
    let into_directory = std::fs::metadata(target).is_ok_and(|m| m.is_dir());
    if sources.len() > 1 && !into_directory {
        return Err(format!(
            "error {}ing: target \"{target}\" is not a directory",
            if sub == "copy" { "copy" } else { "renam" }
        ));
    }
    for source in sources {
        let destination = if into_directory {
            Path::new(target)
                .join(tail(source))
                .to_string_lossy()
                .into_owned()
        } else {
            target.clone()
        };
        // `-force` overwrites a file and never a directory: tclsh answers
        // `file copy -force src d` with `error copying "src" to "d/src": file
        // exists` when `d/src` is a directory, measured, so the switch is not
        // consulted for one.
        let occupied = std::fs::symlink_metadata(&destination);
        let blocked = match &occupied {
            Ok(meta) => !force || meta.is_dir(),
            Err(_) => false,
        };
        if blocked {
            return Err(format!(
                "error {}ing \"{source}\" to \"{destination}\": file exists",
                if sub == "copy" { "copy" } else { "renam" }
            ));
        }
        let outcome = if sub == "copy" {
            copy_tree(Path::new(source), Path::new(&destination))
        } else {
            std::fs::rename(source, &destination)
        };
        outcome.map_err(|e| {
            format!(
                "error {}ing \"{source}\" to \"{destination}\": {}",
                if sub == "copy" { "copy" } else { "renam" },
                posix(&e)
            )
        })?;
    }
    Ok(String::new())
}

/// Copy one name onto another, recursing into a directory.
///
/// `create_dir` and not `create_dir_all`: tclsh does not build a missing parent
/// for the target, and neither may this. `file copy src no/such/place` is
/// `error copying "src" to "no/such/place": no such file or directory` there
/// and nothing is created — where building the parents silently made one
/// conformance case copy a whole home directory into the runner's working
/// directory before anything reported a problem.
fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if !meta.is_dir() {
        std::fs::copy(source, destination)?;
        return Ok(());
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

/// Split the leading `-force` / `--` switches off an argument list.
fn options_prefix(args: &[String]) -> (bool, &[String]) {
    let mut force = false;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "-force" => {
                force = true;
                at += 1;
            }
            "--" => {
                at += 1;
                break;
            }
            _ => break,
        }
    }
    (force, &args[at..])
}

// ── glob ─────────────────────────────────────────────────────────────────

/// Whether a pattern component has anything a `readdir` is needed for. A
/// component without one is used literally, which is how `glob foo/bar`
/// answers for a name in a directory that cannot be listed.
fn has_wildcards(pattern: &str) -> bool {
    let mut escaped = false;
    for ch in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '*' | '?' | '[' => return true,
            _ => {}
        }
    }
    false
}

/// Expand `{a,b}` alternations, which `TclDoGlob` does before anything else.
fn expand_braces(pattern: &str) -> Vec<String> {
    let bytes: Vec<char> = pattern.chars().collect();
    let mut depth = 0;
    let mut open = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            '\\' => i += 1,
            '{' => {
                if depth == 0 {
                    open = Some(i);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let start = open.expect("depth counted an opening brace");
                    let head: String = bytes[..start].iter().collect();
                    let tail: String = bytes[i + 1..].iter().collect();
                    let mut out = Vec::new();
                    for choice in split_alternatives(&bytes[start + 1..i]) {
                        for expanded in expand_braces(&format!("{head}{choice}{tail}")) {
                            out.push(expanded);
                        }
                    }
                    return out;
                }
            }
            _ => {}
        }
        i += 1;
    }
    vec![pattern.to_string()]
}

/// The comma-separated alternatives inside one brace group, respecting nested
/// groups and backslash escapes.
fn split_alternatives(body: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            '\\' if i + 1 < body.len() => {
                current.push('\\');
                current.push(body[i + 1]);
                i += 1;
            }
            '{' => {
                depth += 1;
                current.push('{');
            }
            '}' => {
                depth -= 1;
                current.push('}');
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
        i += 1;
    }
    out.push(current);
    out
}

#[derive(Default)]
struct GlobOptions {
    directory: Option<String>,
    path: Option<String>,
    join: bool,
    tails: bool,
    types: Vec<String>,
}

fn run_glob(words: &[String]) -> Result<String, String> {
    let mut opts = GlobOptions::default();
    let mut at = 0;
    let switches = [
        "-directory",
        "-join",
        "-nocomplain",
        "-path",
        "-tails",
        "-types",
        "--",
    ];
    while at < words.len() {
        let word = &words[at];
        if !word.starts_with('-') {
            break;
        }
        let Some(switch) = resolve(word, &switches) else {
            return Err(format!(
                "bad option \"{word}\": must be -directory, -join, -nocomplain, -path, -tails, -types, or --"
            ));
        };
        at += 1;
        let mut value = || -> Result<String, String> {
            let v = words
                .get(at)
                .cloned()
                .ok_or_else(|| format!("missing argument to \"{switch}\""))?;
            at += 1;
            Ok(v)
        };
        match switch {
            "-directory" => opts.directory = Some(value()?),
            "-path" => opts.path = Some(value()?),
            "-types" => {
                opts.types = crate::list::split(&value()?)?;
            }
            "-join" => opts.join = true,
            "-tails" => opts.tails = true,
            "-nocomplain" => {}
            _ => break,
        }
    }
    let patterns = &words[at..];
    if opts.directory.is_some() && opts.path.is_some() {
        return Err("\"-directory\" cannot be used with \"-path\"".to_string());
    }
    for kind in &opts.types {
        if !matches!(
            kind.as_str(),
            "f" | "d" | "l" | "b" | "c" | "p" | "s" | "r" | "w" | "x" | "hidden" | "readonly"
        ) {
            return Err(format!(
                "glob: the -types element \"{kind}\" is not supported yet"
            ));
        }
    }
    // `-join` makes one pattern out of every remaining word.
    let joined;
    let patterns: Vec<String> = if opts.join {
        joined = vec![patterns.join("/")];
        joined
    } else {
        patterns.to_vec()
    };
    let mut found: Vec<String> = Vec::new();
    for pattern in &patterns {
        let full = match (&opts.directory, &opts.path) {
            (Some(dir), _) => format!("{}/{pattern}", dir.trim_end_matches('/')),
            (_, Some(path)) => format!("{path}{pattern}"),
            _ => pattern.clone(),
        };
        for hit in walk(&full, &opts)? {
            if !found.contains(&hit) {
                found.push(hit);
            }
        }
    }
    if opts.tails {
        let prefix = opts
            .directory
            .as_ref()
            .map(|d| format!("{}/", d.trim_end_matches('/')));
        if let Some(prefix) = prefix {
            found = found
                .into_iter()
                .map(|p| p.strip_prefix(&prefix).unwrap_or(&p).to_string())
                .collect();
        }
    }
    Ok(crate::list::join(&found))
}

/// Walk one whole pattern, component by component.
fn walk(pattern: &str, opts: &GlobOptions) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for expanded in expand_braces(pattern) {
        let components = component_list(&expanded);
        let start = if expanded.starts_with('/') {
            vec!["/".to_string()]
        } else {
            vec![String::new()]
        };
        let mut level = start;
        for (i, component) in components.iter().enumerate() {
            let last = i + 1 == components.len();
            let mut next = Vec::new();
            for base in &level {
                extend(base, component, last, opts, &mut next);
            }
            level = next;
        }
        out.extend(level);
    }
    Ok(out)
}

/// The pattern's components, with the leading root removed — it is the walk's
/// starting point rather than a component to match.
fn component_list(pattern: &str) -> Vec<String> {
    pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// One step of the walk: everything under `base` that `component` names.
fn extend(base: &str, component: &str, last: bool, opts: &GlobOptions, out: &mut Vec<String>) {
    let joined = |name: &str| -> String {
        if base.is_empty() {
            name.to_string()
        } else if base.ends_with('/') {
            format!("{base}{name}")
        } else {
            format!("{base}/{name}")
        }
    };
    if !has_wildcards(component) {
        let literal = unescape(component);
        let candidate = joined(&literal);
        let exists = std::fs::symlink_metadata(&candidate).is_ok();
        if exists && (!last || matches_types(&candidate, &opts.types)) {
            out.push(candidate);
        }
        return;
    }
    let directory = if base.is_empty() { "." } else { base };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    // `readdir` does not list `.` and `..`, and tclsh's answer for `glob .*`
    // includes both, so they are put back where the pattern can reach them.
    if component.starts_with('.') {
        names.push(".".to_string());
        names.push("..".to_string());
    }
    names.sort();
    for name in names {
        // The hidden-file rule, verbatim from `TclpMatchInDirectory`.
        if name.starts_with('.') != component.starts_with('.') {
            continue;
        }
        if !crate::list::glob_match(component, &name) {
            continue;
        }
        let candidate = joined(&name);
        if last && !matches_types(&candidate, &opts.types) {
            continue;
        }
        out.push(candidate);
    }
}

/// Undo the backslash escapes in a literal pattern component.
fn unescape(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    let mut chars = component.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// `-types`, whose letters are an "or" among the file kinds and an "and"
/// among the permission letters, as `glob(n)` describes.
fn matches_types(path: &str, types: &[String]) -> bool {
    if types.is_empty() {
        return true;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    use std::os::unix::fs::FileTypeExt;
    let kind = meta.file_type();
    let mut wanted_kind = false;
    let mut kind_matched = false;
    for letter in types {
        let matched = match letter.as_str() {
            "f" => std::fs::metadata(path).is_ok_and(|m| m.is_file()),
            "d" => std::fs::metadata(path).is_ok_and(|m| m.is_dir()),
            "l" => kind.is_symlink(),
            "b" => kind.is_block_device(),
            "c" => kind.is_char_device(),
            "p" => kind.is_fifo(),
            "s" => kind.is_socket(),
            _ => {
                // A permission letter, which every candidate has to satisfy.
                let mode = match letter.as_str() {
                    "r" => libc::R_OK,
                    "w" => libc::W_OK,
                    "x" => libc::X_OK,
                    "hidden" => {
                        if !tail(path).starts_with('.') {
                            return false;
                        }
                        continue;
                    }
                    _ => {
                        if accessible(path, libc::W_OK) {
                            return false;
                        }
                        continue;
                    }
                };
                if !accessible(path, mode) {
                    return false;
                }
                continue;
            }
        };
        wanted_kind = true;
        kind_matched |= matched;
    }
    !wanted_kind || kind_matched
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cases a fresh derivation gets wrong, each measured against tclsh
    /// 9.0.4 and each pinned again by the differential suite.
    #[test]
    fn the_awkward_paths_split_as_tclsh_splits_them() {
        assert_eq!(split_path("//a//b//"), vec!["//a", "b"]);
        assert_eq!(split_path("/a/b/c"), vec!["/", "a", "b", "c"]);
        assert_eq!(split_path("a/"), vec!["a"]);
        assert_eq!(split_path("/"), vec!["/"]);
        assert!(split_path("").is_empty());
        assert_eq!(dirname("a/"), ".");
        assert_eq!(dirname("//a//b//"), "//a");
        assert_eq!(tail("/"), "");
        assert_eq!(join_paths(&["a".into(), "/b".into()]), "/b");
        assert_eq!(join_paths(&["a".into(), "".into(), "b".into()]), "a/b");
    }

    /// `rootname` cuts at the last `.` at or after the last `/`, which is why
    /// `/a/.hidden` keeps its trailing separator.
    #[test]
    fn the_extension_is_the_last_dot() {
        let root = |p: &str| match extension_at(p) {
            Some(at) => p[..at].to_string(),
            None => p.to_string(),
        };
        assert_eq!(root("/a/.hidden"), "/a/");
        assert_eq!(root("a.tar.gz"), "a.tar");
        assert_eq!(root(".."), ".");
        assert_eq!(root("/a/b/c"), "/a/b/c");
    }

    #[test]
    fn braces_expand_before_the_walk() {
        assert_eq!(expand_braces("*.{txt,dat}"), vec!["*.txt", "*.dat"]);
        assert_eq!(expand_braces("a"), vec!["a"]);
        assert_eq!(expand_braces("{a,b}{c,d}"), vec!["ac", "ad", "bc", "bd"]);
    }
}
