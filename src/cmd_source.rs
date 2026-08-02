//! `source` and `tcl_findLibrary` — evaluating a file, and finding one.
//!
//! # `source`
//!
//! `source` is `eval` over a file's contents: the text is read, compiled through
//! the same chunk cache every other script goes through, and run against the
//! same interpreter. The running chunk's variables are written back before the
//! file runs and re-read afterwards, so the file and its caller see one set of
//! variables in both directions — the trade [`crate::runtime`]'s `eval` op
//! already makes, for the same reason.
//!
//! The failure wording is Tcl's, and it is the *system's* reason rather than a
//! sentence written here: `couldn't read file "x": no such file or directory` is
//! `Tcl_PosixError`'s message (`generic/tclIOUtil.c`'s `Tcl_FSEvalFileEx`, which
//! reports `couldn't read file \"%s\": %s` with `Tcl_PosixError`'s text), and
//! [`posix_reason`] maps the error kinds that reach it to the same lowercase
//! `strerror` phrases.
//!
//! # `tcl_findLibrary`
//!
//! `tcl_findLibrary` is not a C command: it is a Tcl procedure in Tcl's own
//! library, `library/auto.tcl` lines 55–218 of the 9.0.4 release. [`find_library`]
//! is a port of that procedure, and the ordering of the directories it builds is
//! the ordering of the original's `lappend dirs` calls.
//!
//! Two blocks of the original contribute nothing here, and they contribute
//! nothing for a reason that is part of the original's design rather than a
//! shortcut taken here: both are wrapped in `catch`, and both begin with a
//! command this interpreter does not have.
//!
//! * the zipfs block (`auto.tcl:77-136`) opens with `set root [zipfs root]`.
//!   Without a `zipfs` command the `catch` traps the failure on that line and
//!   `dirs` gains nothing — including the three `lappend dirs` that follow it,
//!   which are inside the same `catch`;
//! * the package-configuration lookup (`auto.tcl:141-143`) is
//!   `catch {lappend dirs [::${basename}::pkgconfig get scriptdir,runtime]}`,
//!   and there is no `pkgconfig` command either.
//!
//! What remains is the environment variable, the `auto_path` walk with its
//! Darwin `Resources/Scripts` case, and the three directories relative to the
//! executable — which is what actually finds an installed Tk.
//!
//! [`seed_library_environment`] sets the variables the procedure reads
//! (`tcl_library`, `tcl_libPath`, `auto_path`), because Tcl's own `init.tcl`
//! sets them from C state this crate has no equivalent of.

use std::path::{Path, PathBuf};

use fusevm::{Op, Value};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, TclError};

/// Extension opcode ids owned by this module. 36 and 37 are free in
/// `compiler::ext` — see the note on [`crate::cmd_namespace::ext`].
pub mod ext {
    /// `[path]` → the value of the file's last command.
    pub const SOURCE: u16 = 36;
    /// `[basename, version, patch, initScript, enVarName, varName]` → `""`,
    /// having sourced the script it found.
    pub const FIND_LIBRARY: u16 = 37;
}

/// Whether `id` is one of this module's runtime ops.
pub fn is_op(id: u16) -> bool {
    id == ext::SOURCE || id == ext::FIND_LIBRARY
}

impl Compiler {
    /// `source ?-encoding encoding? fileName`.
    ///
    /// Only the encoding Tcl 9 reads a script in by default is accepted. An
    /// explicit other encoding is refused rather than ignored, because ignoring
    /// it would read the file as something it is not.
    pub(crate) fn cmd_source(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let path = match args {
            [path] => path,
            [flag, encoding, path] if flag.as_literal() == Some("-encoding") => {
                match encoding.as_literal() {
                    Some("utf-8") | Some("utf8") => path,
                    _ => {
                        return self.error(
                            "\"source -encoding\" is only supported for utf-8: this frontend \
                             reads a script as UTF-8",
                        )
                    }
                }
            }
            [flag, ..] if flag.as_literal().is_some_and(|t| t != "-encoding") => {
                // Deferred: tclsh checks a command's options when the command
                // runs, so `catch {source a b c}` traps this rather than
                // failing the script that contains it.
                let msg = format!(
                    "bad option \"{}\": must be -encoding",
                    flag.as_literal().unwrap_or_default()
                );
                return Err(self.deferrable_err(msg));
            }
            _ => {
                return self
                    .error("wrong # args: should be \"source ?-encoding encoding? fileName\"")
            }
        };
        self.word(path)?;
        self.emit(Op::Extended(ext::SOURCE, 1), 0);
        Ok(())
    }

    /// `tcl_findLibrary basename version patch initScript enVarName varName`.
    pub(crate) fn cmd_find_library(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.len() != 6 {
            return self.error(
                "wrong # args: should be \"tcl_findLibrary basename version patch initScript \
                 enVarName varName\"",
            );
        }
        for w in args {
            self.word(w)?;
        }
        self.emit(Op::Extended(ext::FIND_LIBRARY, 6), 1 - 6);
        Ok(())
    }
}

// ── running ──────────────────────────────────────────────────────────────

pub(crate) fn extension(
    interp: &crate::runtime::Shared,
    vm: &mut fusevm::VM,
    id: u16,
    argc: u8,
) -> Result<(), TclError> {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    let args: Vec<String> = values.iter().map(to_tcl_string).collect();
    // As `eval` does: the chunk's variables are the interpreter's for the
    // duration of the nested script, and back again afterwards.
    let result = crate::runtime::with_written_back(interp, vm, |interp| match id {
        ext::SOURCE => source(interp, &args[0]),
        _ => find_library(interp, &args).map(|_| String::new()),
    })?;
    vm.push(Value::Str(std::sync::Arc::new(result)));
    Ok(())
}

/// The lowercase `strerror` phrase Tcl reports for the failures a `source` can
/// hit (`Tcl_PosixError`, which is `Tcl_ErrnoMsg` over `errno`).
fn posix_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "no such file or directory".to_string(),
        PermissionDenied => "permission denied".to_string(),
        // Measured: tclsh 9.0.4 answers `couldn't read file "lib": is a
        // directory` where Rust's own text for EISDIR is "Is a directory".
        IsADirectory => "is a directory".to_string(),
        // Anything else keeps the system's own words, lowercased to match the
        // rest and stripped of Rust's `(os error N)` suffix, which Tcl's message
        // does not carry.
        _ => {
            let text = e.to_string();
            let text = text.split(" (os error").next().unwrap_or(&text).to_string();
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => first.to_lowercase().chain(chars).collect(),
                None => text,
            }
        }
    }
}

/// Read `path` and evaluate it, answering the value of its last command.
pub(crate) fn source(interp: &crate::runtime::Shared, path: &str) -> Result<String, TclError> {
    let bytes = std::fs::read(path).map_err(|e| {
        TclError::plain(format!(
            "couldn't read file \"{path}\": {}",
            posix_reason(&e)
        ))
    })?;
    let text = String::from_utf8(bytes).map_err(|_| {
        TclError::plain(format!(
            "couldn't read file \"{path}\": the file is not valid UTF-8"
        ))
    })?;
    crate::runtime::run_source(interp, &text).map(|v| to_tcl_string(&v))
}

// ── tcl_findLibrary ──────────────────────────────────────────────────────

/// `file join` for the pieces this port builds: an absolute component discards
/// everything before it, which is `Tcl_FSJoinPath`'s rule and the reason
/// `file join /a /b` is `/b`.
fn join(parts: &[&str]) -> String {
    let mut out = PathBuf::new();
    for p in parts {
        if p.is_empty() {
            continue;
        }
        if p.starts_with('/') {
            out = PathBuf::from(p);
        } else {
            out.push(p);
        }
    }
    out.to_string_lossy().into_owned()
}

/// `file dirname`.
fn dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

/// `file normalize`, used only to make the candidate list unique the way the
/// original does. A path that cannot be canonicalised — because it does not
/// exist, which most candidates do not — keeps its own text, so two spellings of
/// a directory that is not there are still two candidates, exactly as they are
/// in tclsh when `file normalize` cannot resolve them either.
fn normalize(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// A port of `tcl_findLibrary` (`library/auto.tcl:55-218` of the Tcl 9.0.4
/// release). See the module documentation for the two blocks of the original
/// that contribute nothing in this interpreter, and why.
pub(crate) fn find_library(
    interp: &crate::runtime::Shared,
    args: &[String],
) -> Result<(), TclError> {
    let [basename, version, _patch, init_script, envar, varname] = args else {
        return Err(TclError::plain(
            "wrong # args: should be \"tcl_findLibrary basename version patch initScript \
             enVarName varName\"",
        ));
    };
    let read = |name: &str| -> Option<String> {
        let state = interp.lock().expect("interpreter lock");
        state.globals.get(name).map(to_tcl_string)
    };

    let mut dirs: Vec<String> = Vec::new();
    // `upvar #0 $varName the_library`: a path the host hardwired is honoured and
    // nothing else is searched.
    let hardwired = read(crate::cmd_namespace::store_key(
        &crate::cmd_namespace::resolve("::", varname),
    ));
    match hardwired {
        Some(dir) if !dir.is_empty() => dirs.push(dir),
        _ => {
            // 1. the environment variable, first so it can work around anything.
            if let Ok(dir) = std::env::var(envar) {
                dirs.push(dir);
            }
            // The zipfs and pkgconfig blocks of the original add nothing here;
            // see the module documentation.

            // 3. relative to every auto_path entry, with the macOS bundle layout
            //    as a second candidate under each.
            let auto_path = read("auto_path").unwrap_or_default();
            let entries = crate::list::split(&auto_path).map_err(TclError::plain)?;
            let darwin = cfg!(target_os = "macos");
            for d in &entries {
                dirs.push(join(&[d, &format!("{basename}{version}")]));
                if darwin {
                    dirs.push(join(&[
                        d,
                        &format!("{basename}{version}"),
                        "Resources",
                        "Scripts",
                    ]));
                }
            }
            // 3 (again, as the original numbers it). Relative to the executable.
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parent = dirname(&dirname(&exe));
            let grandparent = dirname(&parent);
            dirs.push(join(&[&parent, "lib", &format!("{basename}{version}")]));
            dirs.push(join(&[
                &grandparent,
                "lib",
                &format!("{basename}{version}"),
            ]));
            dirs.push(join(&[&parent, "library"]));
        }
    }

    // Make `dirs` unique under normalization, preserving order, and try each.
    let mut seen: Vec<String> = Vec::new();
    let mut errors = String::new();
    for dir in &dirs {
        let norm = normalize(dir);
        if seen.contains(&norm) {
            continue;
        }
        seen.push(norm);

        let file = join(&[dir, init_script]);
        if !Path::new(&file).exists() {
            continue;
        }
        // `set the_library $i` happens before the source, so the sourced script
        // can read it — which `tk.tcl` does.
        {
            let mut state = interp.lock().expect("interpreter lock");
            state.globals.insert(
                crate::cmd_namespace::store_key(&crate::cmd_namespace::resolve("::", varname))
                    .to_string(),
                Value::Str(std::sync::Arc::new(dir.clone())),
            );
        }
        // The original appends two lines per failed candidate: `"$file: $msg"`
        // and `[dict get $opts -errorinfo]`. This frontend has no `-errorinfo` —
        // there is no options dictionary and no traceback — so the message
        // stands in for both, which keeps the shape of the report without
        // inventing a stack that was never recorded.
        match source(interp, &file) {
            Ok(_) => return Ok(()),
            Err(e) => errors.push_str(&format!("{file}: {}\n{}\n", e.msg, e.msg)),
        }
    }

    {
        let mut state = interp.lock().expect("interpreter lock");
        state.globals.remove(crate::cmd_namespace::store_key(
            &crate::cmd_namespace::resolve("::", varname),
        ));
    }
    Err(TclError::plain(format!(
        "Can't find a usable {init_script} in the following directories: \n    {}\n\n{errors}\n\n\
         This probably means that {basename} wasn't installed properly.\n",
        crate::list::join(&dirs)
    )))
}

/// Set the variables `tcl_findLibrary` reads, which Tcl's own `init.tcl` sets
/// from C state this crate has no equivalent of.
///
/// * `tcl_library` — where Tcl's script library is, from `TCL_LIBRARY` if the
///   environment names one and from the standard locations otherwise;
/// * `tcl_libPath` and `auto_path` — the directories `tcl_findLibrary` walks.
///
/// A host that knows better should set them itself; this is the default a
/// process gets when it does not, and it is the reason `tcl_findLibrary tk …`
/// can find an installed Tk at all.
pub fn seed_library_environment(interp: &mut crate::runtime::Interp) {
    let mut path: Vec<String> = Vec::new();
    if let Ok(dir) = std::env::var("TCL_LIBRARY") {
        if !dir.is_empty() {
            path.push(dirname(&dir));
            interp.set_global("tcl_library", dir);
        }
    }
    // ../lib and ../../lib relative to the running binary, which is where an
    // installed Tcl and Tk put their script libraries.
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.to_string_lossy().into_owned();
        let parent = dirname(&dirname(&exe));
        let grandparent = dirname(&parent);
        for base in [&parent, &grandparent] {
            let lib = join(&[base, "lib"]);
            if Path::new(&lib).is_dir() && !path.contains(&lib) {
                path.push(lib);
            }
        }
    }
    if interp.global("tcl_library").is_none() {
        for dir in &path {
            let candidate = join(&[dir, "tcl9.0"]);
            if Path::new(&join(&[&candidate, "init.tcl"])).exists() {
                interp.set_global("tcl_library", candidate);
                break;
            }
        }
    }
    let joined = crate::list::join(&path);
    interp.set_global("tcl_libPath", joined.clone());
    if interp.global("auto_path").is_none() {
        interp.set_global("auto_path", joined);
    }
}
