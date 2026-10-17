//! The `file` subcommands that reach the file system for more than a path or a
//! stat: `attributes`, `link`, `tempdir`, `tempfile`, `volumes`, `system`,
//! `channels`, and the setting forms of `atime` and `mtime`.
//!
//! Ports of `TclFileAttrsCmd` (`generic/tclFCmd.c`) over the Unix attribute
//! procedures of `unix/tclUnixFCmd.c`, `TclFileLinkCmd` and `TclpObjLink`,
//! `TclFileTempDirCmd` and `TclFileTemporaryCmd` over `TclpCreateTemporaryDirectory`
//! and `TclUnixOpenTemporaryFile`, and `FileAttrAccessTimeCmd` /
//! `FileAttrModifyTimeCmd` (`generic/tclCmdAH.c`).
//!
//! Where tclsh's attributes depend on the platform, this provides the Unix set
//! — `-group`, `-owner`, `-permissions` — everywhere. On macOS tclsh adds
//! `-readonly`, `-creator`, `-type`, `-hidden` and `-rsrclength`, which come
//! from `getattrlist` and the Finder's flags; those are not provided.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use crate::cmd_file::{dirname, extension_at, posix, stat_buf, tail};

/// The attributes `file attributes` offers, in tclsh's order for Unix.
const ATTRIBUTES: &[&str] = &["-group", "-owner", "-permissions"];

fn c_path(path: &str) -> Result<CString, String> {
    CString::new(path).map_err(|_| format!("could not read \"{path}\": no such file or directory"))
}

/// `file attributes name ?-option value ...?`.
pub(crate) fn attributes(words: &[String]) -> Result<String, String> {
    let [_, path, rest @ ..] = words else {
        return Err("wrong # args: should be \"file attributes name ?-option value ...?\"".into());
    };
    match rest {
        [] => {
            let mut listing = Vec::new();
            let mut last_error = None;
            for name in ATTRIBUTES {
                match get(path, name) {
                    Ok(value) => {
                        listing.push((*name).to_string());
                        listing.push(value);
                    }
                    Err(message) => last_error = Some(message),
                }
            }
            match (listing.is_empty(), last_error) {
                (true, Some(message)) => Err(message),
                _ => Ok(crate::list::join(&listing)),
            }
        }
        [option] => get(path, ATTRIBUTES[index_of(option)?]),
        pairs => {
            for pair in pairs.chunks(2) {
                let at = index_of(&pair[0])?;
                let Some(value) = pair.get(1) else {
                    return Err(format!("value for \"{}\" missing", pair[0]));
                };
                set(path, ATTRIBUTES[at], value)?;
            }
            Ok(String::new())
        }
    }
}

fn index_of(option: &str) -> Result<usize, String> {
    crate::cmd_prefix::index_from(option, ATTRIBUTES, "option")
}

fn get(path: &str, attribute: &str) -> Result<String, String> {
    let meta = stat_buf(path, false)?;
    use std::os::unix::fs::MetadataExt;
    match attribute {
        "-group" => {
            let gid = meta.gid();
            // SAFETY: `getgrgid` returns null or a pointer to a static record.
            let group = unsafe { libc::getgrgid(gid) };
            Ok(if group.is_null() {
                gid.to_string()
            } else {
                // SAFETY: a non-null record's name is a NUL-terminated string.
                unsafe { std::ffi::CStr::from_ptr((*group).gr_name) }
                    .to_string_lossy()
                    .into_owned()
            })
        }
        "-owner" => {
            let uid = meta.uid();
            // SAFETY: `getpwuid` returns null or a pointer to a static record.
            let user = unsafe { libc::getpwuid(uid) };
            Ok(if user.is_null() {
                uid.to_string()
            } else {
                // SAFETY: a non-null record's name is a NUL-terminated string.
                unsafe { std::ffi::CStr::from_ptr((*user).pw_name) }
                    .to_string_lossy()
                    .into_owned()
            })
        }
        _ => Ok(format!("{:#05o}", meta.mode() & 0x7FFF)),
    }
}

fn set(path: &str, attribute: &str, value: &str) -> Result<(), String> {
    match attribute {
        "-group" => set_ownership(path, value, true),
        "-owner" => set_ownership(path, value, false),
        _ => set_permissions(path, value),
    }
}

/// `SetGroupAttribute` and `SetOwnerAttribute`: a number is an id, anything
/// else a name looked up in the group or password database.
fn set_ownership(path: &str, value: &str, group: bool) -> Result<(), String> {
    let what = if group { "group" } else { "owner" };
    let id: u32 = match crate::list::parse_int(value) {
        Some(n) => n as u32,
        None => {
            let name =
                CString::new(value).map_err(|_| format!("{what} \"{value}\" does not exist"))?;
            // SAFETY: both look-ups are handed a NUL-terminated name and
            // return null or a pointer to a static record.
            let found = unsafe {
                if group {
                    let record = libc::getgrnam(name.as_ptr());
                    (!record.is_null()).then(|| (*record).gr_gid)
                } else {
                    let record = libc::getpwnam(name.as_ptr());
                    (!record.is_null()).then(|| (*record).pw_uid)
                }
            };
            match found {
                Some(id) => id,
                None => {
                    let noun = if group { "group" } else { "user" };
                    return Err(format!(
                        "could not set {what} for file \"{path}\": {noun} \"{value}\" does not exist"
                    ));
                }
            }
        }
    };
    let native = c_path(path)?;
    let (uid, gid) = if group {
        (u32::MAX, id)
    } else {
        (id, u32::MAX)
    };
    // SAFETY: `chown` is handed a NUL-terminated path; -1 leaves an id alone.
    if unsafe { libc::chown(native.as_ptr(), uid, gid) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(format!(
            "could not set {what} for file \"{path}\": {}",
            posix(&error)
        ));
    }
    Ok(())
}

/// `SetPermissionsAttribute`: an octal number with a leading zero, any other
/// integer, `rwxr-x---`, or `chmod`-style `ug+x,o=r` clauses applied to the
/// file's current mode.
fn set_permissions(path: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let octal = trimmed
        .strip_prefix('0')
        .filter(|rest| rest.starts_with(|c: char| ('0'..='7').contains(&c)))
        .and_then(|rest| crate::list::parse_int(&format!("0o{rest}")));
    let mode = match octal.or_else(|| crate::list::parse_int(value)) {
        Some(n) => (n & 0x7FFF) as u32,
        None => {
            let meta = stat_buf(path, false)?;
            use std::os::unix::fs::MetadataExt;
            let current = meta.mode() & 0x7FFF;
            mode_from_string(value, current)
                .ok_or_else(|| format!("unknown permission string format \"{value}\""))?
        }
    };
    let native = c_path(path)?;
    // SAFETY: `chmod` is handed a NUL-terminated path and a mode.
    if unsafe { libc::chmod(native.as_ptr(), mode as libc::mode_t) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(format!(
            "could not set permissions for file \"{path}\": {}",
            posix(&error)
        ));
    }
    Ok(())
}

/// `GetModeFromPermString`: `rwxrwxrwx` (with `s`, `S`, `t`, `T`) or
/// `[ugoa]*[+-=][rwxst]*` clauses separated by commas.
fn mode_from_string(text: &str, mut mode: u32) -> Option<u32> {
    let bytes = text.as_bytes();
    if bytes.len() == 9 {
        if let Some(fresh) = symbolic_mode(bytes) {
            return Some(fresh);
        }
    }
    let mut n = 0;
    while n < bytes.len() {
        let old = mode;
        let (mut who, mut op, mut what) = (0u32, 0u8, 0u32);
        let (mut op_found, mut who_found) = (false, false);
        let mut i = 0;
        while n + i < bytes.len() {
            let c = bytes[n + i];
            if !who_found {
                match c {
                    b'u' => {
                        who |= 0x9C0;
                        i += 1;
                        continue;
                    }
                    b'g' => {
                        who |= 0x438;
                        i += 1;
                        continue;
                    }
                    b'o' => {
                        who |= 0x207;
                        i += 1;
                        continue;
                    }
                    b'a' => {
                        who |= 0xFFF;
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            who_found = true;
            if who == 0 {
                who = 0xFFF;
            }
            if !op_found {
                op = match c {
                    b'+' => 1,
                    b'-' => 2,
                    b'=' => 3,
                    _ => return None,
                };
                op_found = true;
                i += 1;
                continue;
            }
            match c {
                b'r' => what |= 0x124,
                b'w' => what |= 0x92,
                b'x' => what |= 0x49,
                b's' => what |= 0xC00,
                b't' => what |= 0x200,
                b',' => {
                    i += 1;
                    break;
                }
                _ => return None,
            }
            i += 1;
        }
        mode = match op {
            1 => old | (who & what),
            2 => old & !(who & what),
            3 => (old & !who) | (who & what),
            _ => mode,
        };
        n += i;
        if i == 0 {
            break;
        }
    }
    Some(mode)
}

/// The nine-character `rwxr-x---` form.
fn symbolic_mode(bytes: &[u8]) -> Option<u32> {
    let mut mode = 0u32;
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'r' if i % 3 == 0 => mode |= 1 << (8 - i),
            b'w' if i % 3 == 1 => mode |= 1 << (8 - i),
            b'x' if i % 3 == 2 => mode |= 1 << (8 - i),
            b's' if i % 3 == 2 && i <= 5 => {
                mode |= 1 << (8 - i);
                mode |= 1 << (11 - i / 3);
            }
            b'S' if i % 3 == 2 && i <= 5 => mode |= 1 << (11 - i / 3),
            b't' if i == 8 => {
                mode |= 1 << (8 - i);
                mode |= 1 << 9;
            }
            b'T' if i == 8 => mode |= 1 << 9,
            b'-' => {}
            _ => return None,
        }
    }
    Some(mode)
}

/// `file link ?-linktype? linkname ?target?`.
pub(crate) fn link(words: &[String]) -> Result<String, String> {
    let args = &words[1..];
    if args.is_empty() || args.len() > 3 {
        return Err("wrong # args: should be \"file link ?-linktype? linkname ?target?\"".into());
    }
    let index = if args.len() == 3 { 1 } else { 0 };
    let name = &args[index];
    if args.len() == 1 {
        let target = std::fs::read_link(name)
            .map_err(|e| format!("could not read link \"{name}\": {}", posix(&e)))?;
        return Ok(target.to_string_lossy().into_owned());
    }
    let target = &args[index + 1];
    let symbolic = if args.len() == 3 {
        crate::cmd_prefix::index_from(&args[0], &["-symbolic", "-hard"], "option")? == 0
    } else {
        true
    };
    create_link(name, target, symbolic).map_err(|failure| match failure.raw_os_error() {
        Some(libc::EEXIST) => {
            format!("could not create new link \"{name}\": that path already exists")
        }
        Some(libc::ENOENT) => {
            let parent = dirname(name);
            if std::fs::metadata(&parent).is_err() {
                format!("could not create new link \"{name}\": no such file or directory")
            } else {
                format!("could not create new link \"{name}\": target \"{target}\" doesn't exist")
            }
        }
        _ => format!(
            "could not create new link \"{name}\" pointing to \"{target}\": {}",
            posix(&failure)
        ),
    })?;
    Ok(target.clone())
}

/// `TclpObjLink` with a target: the target must exist (a relative symbolic
/// target is looked for beside the link), the link must not, and a symbolic
/// link is preferred to a hard one.
fn create_link(name: &str, target: &str, symbolic: bool) -> std::io::Result<()> {
    let exists = |path: &Path| std::fs::symlink_metadata(path).is_ok() || path.exists();
    use std::path::Path;
    let probe = if symbolic && !target.starts_with('/') && !target.starts_with('~') {
        Path::new(&dirname(name)).join(target)
    } else {
        Path::new(target).to_path_buf()
    };
    if !exists(&probe) {
        return Err(std::io::Error::from_raw_os_error(libc::ENOENT));
    }
    if exists(Path::new(name)) {
        return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
    }
    if symbolic {
        std::os::unix::fs::symlink(target, name)
    } else {
        std::fs::hard_link(target, name)
    }
}

/// The directory a temporary file or directory goes in when the template names
/// none: `$TMPDIR` when that is a writable directory, then `/tmp`.
fn default_temp_dir() -> String {
    let usable = |dir: &str| {
        !dir.is_empty()
            && std::fs::metadata(dir).is_ok_and(|m| m.is_dir())
            && c_path(dir).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
    };
    if let Ok(dir) = std::env::var("TMPDIR") {
        if usable(&dir) {
            return dir;
        }
    }
    "/tmp".to_string()
}

/// `file tempdir ?template?`.
pub(crate) fn tempdir(words: &[String]) -> Result<String, String> {
    if words.len() > 2 {
        return Err("wrong # args: should be \"file tempdir ?template?\"".into());
    }
    let (mut dir, mut base) = (None, None);
    if let Some(template) = words.get(1).filter(|t| !t.is_empty()) {
        if !template.contains('/') {
            base = Some(template.clone());
        } else if !template.ends_with('/') {
            dir = Some(dirname(template));
            base = Some(tail(template));
        } else {
            dir = Some(template.clone());
        }
    }
    let dir = dir
        .filter(|d| !d.is_empty())
        .unwrap_or_else(default_temp_dir);
    let mut pattern = dir;
    if !pattern.ends_with('/') {
        pattern.push('/');
    }
    pattern.push_str(base.as_deref().filter(|b| !b.is_empty()).unwrap_or("tcl"));
    pattern.push_str("_XXXXXX");
    let mut bytes = CString::new(pattern.as_bytes())
        .map_err(|_| "can't create temporary directory: invalid argument".to_string())?
        .into_bytes_with_nul();
    // SAFETY: `mkdtemp` rewrites the trailing XXXXXX of a NUL-terminated buffer.
    let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if made.is_null() {
        let error = std::io::Error::last_os_error();
        return Err(format!(
            "can't create temporary directory: {}",
            posix(&error)
        ));
    }
    bytes.pop();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The pieces `file tempfile ?nameVar? ?template?` takes from a template: a
/// directory only if there is a separator in it, a base name unless it ends in
/// one, and the extension of that name.
fn template_pieces(template: &str) -> (Option<String>, Option<String>, Option<String>) {
    if template.is_empty() {
        return (None, None, None);
    }
    let dir = template.contains('/').then(|| dirname(template));
    let (mut base, mut ext) = (None, None);
    if !template.ends_with('/') {
        let name = tail(template);
        match extension_at(&name) {
            Some(at) => {
                base = Some(name[..at].to_string());
                ext = Some(name[at..].to_string());
            }
            None => base = Some(name),
        }
    }
    let keep = |piece: Option<String>| piece.filter(|p| !p.is_empty());
    (keep(dir), keep(base), keep(ext))
}

/// `TclUnixOpenTemporaryFile`: a descriptor open on a new file, and its path.
/// With no `keep`, the file is unlinked at once.
pub(crate) fn open_temporary(
    template: &str,
    keep: bool,
) -> Result<(std::fs::File, String), String> {
    let (dir, base, ext) = template_pieces(template);
    let mut pattern = dir.unwrap_or_else(default_temp_dir);
    pattern.push('/');
    pattern.push_str(base.as_deref().unwrap_or("tcl"));
    pattern.push_str("_XXXXXX");
    let suffix = ext.as_deref().unwrap_or("");
    pattern.push_str(suffix);
    let mut bytes = CString::new(pattern.as_bytes())
        .map_err(|_| "can't create temporary file: invalid argument".to_string())?
        .into_bytes_with_nul();
    // SAFETY: `mkstemps` rewrites the XXXXXX that precedes the last `suffix`
    // bytes of a NUL-terminated buffer and returns a descriptor or -1.
    let fd = unsafe { libc::mkstemps(bytes.as_mut_ptr().cast(), suffix.len() as libc::c_int) };
    if fd == -1 {
        let error = std::io::Error::last_os_error();
        return Err(format!("can't create temporary file: {}", posix(&error)));
    }
    bytes.pop();
    let path = String::from_utf8_lossy(&bytes).into_owned();
    if !keep {
        let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes(path.as_bytes()));
    }
    // SAFETY: `fd` is a descriptor `mkstemps` just returned, owned by no one.
    let file = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
    Ok((file, path))
}

/// `file atime|mtime name ?time?`: read the time, setting it first when given.
pub(crate) fn times(words: &[String]) -> Result<String, String> {
    let sub = words[0].as_str();
    if !(2..=3).contains(&words.len()) {
        return Err(format!(
            "wrong # args: should be \"file {sub} name ?time?\""
        ));
    }
    let path = &words[1];
    let current = stat_buf(path, false)?;
    use std::os::unix::fs::MetadataExt;
    if let Some(text) = words.get(2) {
        let new = crate::list::wide(text)?;
        let (atime, mtime) = if sub == "atime" {
            (new, current.mtime())
        } else {
            (current.atime(), new)
        };
        let native = c_path(path)?;
        let stamps = [
            libc::timeval {
                tv_sec: atime as libc::time_t,
                tv_usec: 0,
            },
            libc::timeval {
                tv_sec: mtime as libc::time_t,
                tv_usec: 0,
            },
        ];
        // SAFETY: `utimes` is handed a NUL-terminated path and two timevals.
        if unsafe { libc::utimes(native.as_ptr(), stamps.as_ptr()) } != 0 {
            let error = std::io::Error::last_os_error();
            let what = if sub == "atime" {
                "access"
            } else {
                "modification"
            };
            return Err(format!(
                "could not set {what} time for file \"{path}\": {}",
                posix(&error)
            ));
        }
        let after = stat_buf(path, false)?;
        return Ok(if sub == "atime" {
            after.atime()
        } else {
            after.mtime()
        }
        .to_string());
    }
    Ok(if sub == "atime" {
        current.atime()
    } else {
        current.mtime()
    }
    .to_string())
}
