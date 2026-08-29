//! The `encoding` ensemble.
//!
//! What is here and what is refused, stated plainly, because a *wrong*
//! transcoding is worse than one that says it cannot answer — a script that
//! gets back plausible bytes has no way to find out they are not the right
//! bytes.
//!
//! Supported:
//!
//! * `convertfrom` and `convertto`, with `-profile` (`tcl8`, `strict`,
//!   `replace` — `strict` is the default, as in tclsh 9.0.4) and `-failindex`.
//! * `names`, `profiles`, `system`, `user`, `dirs`.
//! * Every *table* encoding the reference release ships: the 80 files of its
//!   `library/encoding/` directory, vendored verbatim into `src/encodings/` by
//!   `scripts/gen_encoding_tables.py`. That is the whole `iso8859-*` family,
//!   `cp125*` and the rest of the code pages, the `mac*` set, `koi8-*`,
//!   `ascii`, `ebcdic`, `symbol`/`dingbats`, and the double- and multi-byte CJK
//!   encodings — `big5`, `cp932`, `cp936`, `cp949`, `cp950`, `euc-cn`,
//!   `euc-jp`, `euc-kr`, `gb2312`, `gb12345`, `jis0208`, `jis0212`,
//!   `ksc5601`, `shiftjis`, `cns11643`, `macJapan`. They are not approximated:
//!   `load_table` is a port of `LoadTableEncoding`
//!   (`generic/tclEncoding.c:1954`) reading the same files, and
//!   `table_to_utf`/`table_from_utf` are ports of `TableToUtfProc` and
//!   `TableFromUtfProc`, so a double-byte encoding decodes through the same
//!   prefix-byte machinery the reference interpreter uses.
//! * `utf-8`, `cesu-8`, `utf-16`/`utf-16le`/`utf-16be`/`unicode`,
//!   `ucs-2`/`ucs-2le`/`ucs-2be` and `utf-32`/`utf-32le`/`utf-32be`, as ports
//!   of `UtfToUtfProc`, `Utf16ToUtfProc`, `UtfToUtf16Proc`, `UtfToUcs2Proc`,
//!   `Utf32ToUtfProc` and `UtfToUtf32Proc`.
//!
//! Refused, each by name:
//!
//! * `iso2022`, `iso2022-jp` and `iso2022-kr`. These are not tables but escape
//!   state machines with a file format of their own (`LoadEscapeEncoding`), and
//!   an approximation of a stateful encoding is exactly the kind of wrong
//!   answer that survives a test suite. They are absent from
//!   [`names`] as well, so `encoding names` lists what actually works.
//! * `identity` and `binary`. Measured: tclsh 9.0.4 answers `unknown encoding
//!   "identity"` for both — they were Tcl 8 spellings and are gone — so this
//!   frontend refuses them with the same message rather than reviving them.
//! * Any decode whose result would be a lone surrogate. Only the `tcl8`
//!   profile can produce one (`encoding convertfrom -profile tcl8 utf-8
//!   \xED\xA0\x80` is U+D800 in tclsh), and a `String` in this frontend cannot
//!   hold one, so the conversion stops with a message naming the code point
//!   instead of substituting something else.
//!
//! A byte string is a string whose every character is below U+0100, which is
//! how the reference interpreter's byte arrays behave when read as strings:
//! measured, `encoding convertto utf-8 é` is the two characters U+00C3 U+00A9.
//! So `convertto` yields one of those and `convertfrom` demands one, with the
//! same refusal tclsh's `Tcl_GetBytesFromObj` raises for a character above
//! U+00FF.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{place_at, to_tcl_string, var_cell};

/// Extension opcode ids owned by this module.
pub mod ext {
    pub use crate::compiler::ext::ENCODING_BASE as BASE;
    /// `[profile, place, encoding, data]` → the converted string. The inline
    /// operand carries the direction and which options were written; see the
    /// `ARG_*` constants in the parent module.
    pub const CONVERT: u16 = BASE;
    /// `[]` → the encodings this module can actually convert with.
    pub const NAMES: u16 = BASE + 1;
    /// `[name?]` → the system encoding, setting it first when one was given.
    pub const SYSTEM: u16 = BASE + 2;
    /// `[dirList?]` → the encoding search path.
    pub const DIRS: u16 = BASE + 3;
    /// `[]` → the profile names.
    pub const PROFILES: u16 = BASE + 4;
    /// `[]` → the user's preferred encoding.
    pub const USER: u16 = BASE + 5;
}

/// The command names this module claims, for the REPL's completion and the
/// reference page.
pub const COMMANDS: &[&str] = &["encoding"];

/// Every subcommand, in the order the interpreter lists them when it rejects
/// one.
pub const SUBCOMMANDS: &[&str] = &[
    "convertfrom",
    "convertto",
    "dirs",
    "names",
    "profiles",
    "system",
    "user",
];

/// The `-profile` values, in the order the interpreter lists them.
const PROFILES: &[&str] = &["replace", "strict", "tcl8"];

// ── the inline operand of `CONVERT` ──────────────────────────────────────

/// `convertto` rather than `convertfrom`.
const ARG_TO: u8 = 1;
/// A `-profile` word is on the stack; without this its slot holds an empty
/// string and the default profile applies.
const ARG_PROFILE: u8 = 2;
/// A `-failindex` variable place is on the stack.
const ARG_FAILINDEX: u8 = 4;
/// That place is a frame slot rather than a global.
const ARG_SLOT: u8 = 8;
/// The one-argument form: no encoding was named, so the system encoding is
/// used and the encoding slot holds an empty string.
const ARG_SYSTEM: u8 = 16;

// ── compiling ────────────────────────────────────────────────────────────

/// Lower `encoding …`.
pub(crate) fn compile(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let Some(first) = args.first() else {
        return c.error("wrong # args: should be \"encoding subcommand ?arg ...?\"");
    };
    let given = c.literal_of(first, "subcommand")?.to_string();
    let Some(sub) = resolve(&given, SUBCOMMANDS) else {
        return c.error(format!(
            "unknown or ambiguous subcommand \"{given}\": must be {}",
            listing(SUBCOMMANDS)
        ));
    };
    let rest = &args[1..];
    match sub {
        "convertfrom" | "convertto" => compile_convert(c, sub, rest),
        "names" | "profiles" => {
            if !rest.is_empty() {
                return c.error(format!("wrong # args: should be \"encoding {sub}\""));
            }
            let id = if sub == "names" {
                ext::NAMES
            } else {
                ext::PROFILES
            };
            c.emit(Op::Extended(id, 0), 1);
            Ok(())
        }
        // The trailing space is the interpreter's, not a typo: `encoding user`
        // is declared with an empty argument spec and `Tcl_WrongNumArgs`
        // appends it anyway. Measured on tclsh 9.0.4.
        "user" => {
            if !rest.is_empty() {
                return c.error("wrong # args: should be \"encoding user \"");
            }
            // The empty slot `system` and `dirs` use for an argument that was
            // not written, so all three share one handler and one stack shape.
            c.push_str("");
            c.emit(Op::Extended(ext::USER, 0), 0);
            Ok(())
        }
        // `system` and `dirs` each take one optional argument, which always
        // rides on the stack — empty when absent — so the handler has one
        // shape rather than two. The inline operand says whether it was given.
        other => {
            if rest.len() > 1 {
                let usage = if other == "system" {
                    "?encoding?"
                } else {
                    "?dirList?"
                };
                return c.error(format!(
                    "wrong # args: should be \"encoding {other} {usage}\""
                ));
            }
            let given = match rest.first() {
                Some(w) => {
                    c.word(w)?;
                    1
                }
                None => {
                    c.push_str("");
                    0
                }
            };
            let id = if other == "system" {
                ext::SYSTEM
            } else {
                ext::DIRS
            };
            c.emit(Op::Extended(id, given), 0);
            Ok(())
        }
    }
}

/// Lower `encoding convertfrom …` or `encoding convertto …`.
///
/// A port of `EncodingConvertParseOptions` (`generic/tclCmdAH.c:429`). Which
/// argument is an option name, which is its value, and which two are the
/// encoding and the data is decided by the argument *count* alone, so all of
/// that is settled here; only the values travel.
fn compile_convert(c: &mut Compiler, sub: &str, args: &[Word]) -> Result<(), CompileError> {
    let n = args.len();
    if n == 0 {
        return c.error(wrong_args(sub, n));
    }
    let mut flags = if sub == "convertto" { ARG_TO } else { 0 };
    // Where the `-profile` value and the `-failindex` variable were written,
    // if they were.
    let mut profile: Option<&Word> = None;
    let mut failindex: Option<&Word> = None;
    if n == 1 {
        flags |= ARG_SYSTEM;
    } else {
        let mut k = 0;
        while k + 2 < n {
            let name = c.literal_of(&args[k], "option")?.to_string();
            let Some(option) = resolve(&name, &["-profile", "-failindex"]) else {
                // An unknown option is the interpreter's own run-time refusal,
                // so it stays catchable rather than failing the compile.
                return runtime_error(c, bad_option(&name));
            };
            k += 1;
            // The option's value would be the encoding: the option was written
            // without one. `EncodingConvertParseOptions`'s own `goto
            // numArgsError`.
            if k == n - 2 {
                return c.error(wrong_args(sub, n));
            }
            match option {
                "-profile" => {
                    profile = Some(&args[k]);
                    flags |= ARG_PROFILE;
                }
                _ => {
                    failindex = Some(&args[k]);
                    flags |= ARG_FAILINDEX;
                }
            }
            k += 1;
        }
    }

    // The four stack slots, always all four, so the handler pops a fixed
    // shape: the profile name, where a `-failindex` variable lives, the
    // encoding's name and the data.
    match profile {
        Some(w) => c.word(w)?,
        None => c.push_str(""),
    }
    match failindex {
        Some(w) => {
            let name = c.var_name_of(w)?;
            let encoded = c.place_operand(&name);
            // `place_operand` packs the frame-slot bit at the bottom; the op
            // needs it in its own operand, because the place travels as a
            // plain integer.
            if encoded & 1 == 1 {
                flags |= ARG_SLOT;
            }
            c.emit(Op::LoadInt(encoded >> 1), 1);
        }
        None => {
            c.emit(Op::LoadInt(-1), 1);
        }
    }
    if flags & ARG_SYSTEM != 0 {
        c.push_str("");
        c.word(&args[0])?;
    } else {
        c.word(&args[n - 2])?;
        c.word(&args[n - 1])?;
    }
    c.emit(Op::Extended(ext::CONVERT, flags), -3);
    Ok(())
}

/// The two-alternative `wrong # args` message, as the interpreter renders it.
///
/// Which command name it names depends on the argument count, and that is not
/// this crate inventing a rule: `convertfrom` and `convertto` carry
/// `TclCompileBasic1To3ArgCmd`, so tclsh compiles the call to a direct
/// invocation of the ensemble's implementation when it was written with one to
/// three arguments and the message names *that*, while any other count goes
/// through the ensemble at run time and the message names the ensemble.
/// Measured on tclsh 9.0.4: three arguments give
/// `::tcl::encoding::convertfrom …`, none and five give `encoding convertfrom
/// …`.
fn wrong_args(sub: &str, argc: usize) -> String {
    let name = if (1..=3).contains(&argc) {
        format!("::tcl::encoding::{sub}")
    } else {
        format!("encoding {sub}")
    };
    format!(
        "wrong # args: should be \"{name} ?-profile profile? ?-failindex var? encoding data\" \
         or \"{name} data\""
    )
}

fn bad_option(name: &str) -> String {
    let ambiguous = !name.is_empty()
        && ["-profile", "-failindex"]
            .iter()
            .filter(|o| o.starts_with(name))
            .count()
            > 1;
    let what = if ambiguous { "ambiguous" } else { "bad" };
    format!("{what} option \"{name}\": must be -profile or -failindex")
}

/// Lower a message as a run-time error, the way `regexp` lowers an unknown
/// switch: the script still compiles and the refusal is catchable.
fn runtime_error(c: &mut Compiler, message: String) -> Result<(), CompileError> {
    c.push_str(&message);
    c.emit(Op::Extended(crate::compiler::ext::ERROR, 0), -1);
    c.push_empty();
    Ok(())
}

/// `Tcl_GetIndexFromObj`'s rule: an exact match wins, otherwise a prefix that
/// fits exactly one entry.
fn resolve<'t>(name: &str, table: &[&'t str]) -> Option<&'t str> {
    if let Some(exact) = table.iter().find(|c| **c == name) {
        return Some(exact);
    }
    if name.is_empty() {
        return None;
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

/// The interpreter's rendering of a table in an error message.
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

// ── running ──────────────────────────────────────────────────────────────

/// Whether an id belongs to this module's block.
pub(crate) fn is_op(id: u16) -> bool {
    (ext::BASE..ext::BASE + crate::compiler::ext::BLOCK).contains(&id)
}

/// The system encoding, and the encoding search path.
///
/// tclsh derives the first from the locale and the second from where its own
/// library was installed. This frontend carries its tables inside the binary,
/// so there is no directory to search and the default path is empty — see the
/// `dirs` arm below. The system encoding starts as `utf-8`, which is both what
/// tclsh reports on the machine this was measured against and what this
/// frontend actually writes.
fn state() -> &'static Mutex<(String, String)> {
    static STATE: OnceLock<Mutex<(String, String)>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(("utf-8".to_string(), String::new())))
}

/// Dispatch this module's ops.
pub(crate) fn extension(vm: &mut VM, id: u16, arg: u8) -> Result<(), String> {
    match id {
        ext::CONVERT => convert(vm, arg),
        ext::NAMES => {
            let list = crate::list::join(&names());
            vm.push(Value::Str(Arc::new(list)));
            Ok(())
        }
        ext::PROFILES => {
            vm.push(Value::Str(Arc::new(crate::list::join(PROFILES))));
            Ok(())
        }
        ext::USER | ext::SYSTEM => {
            let given = to_tcl_string(&vm.pop());
            if id == ext::SYSTEM && arg == 1 {
                // tclsh accepts the empty name — measured, `encoding system ""`
                // answers `{}` — and refuses anything else it has no encoding
                // for.
                if !given.is_empty() {
                    lookup(&given)?;
                }
                state().lock().expect("encoding state").0 = given.clone();
                vm.push(Value::Str(Arc::new(given)));
                return Ok(());
            }
            let current = state().lock().expect("encoding state").0.clone();
            vm.push(Value::Str(Arc::new(current)));
            Ok(())
        }
        ext::DIRS => {
            let given = to_tcl_string(&vm.pop());
            if arg == 1 {
                if crate::list::split(&given).is_err() {
                    return Err(format!("expected directory list but got \"{given}\""));
                }
                state().lock().expect("encoding state").1 = given.clone();
                vm.push(Value::Str(Arc::new(given)));
                return Ok(());
            }
            let current = state().lock().expect("encoding state").1.clone();
            vm.push(Value::Str(Arc::new(current)));
            Ok(())
        }
        other => Err(format!("unknown encoding op {other}")),
    }
}

/// `encoding convertfrom …` and `encoding convertto …`.
fn convert(vm: &mut VM, flags: u8) -> Result<(), String> {
    let data = vm.pop();
    let name = to_tcl_string(&vm.pop());
    let place = vm.pop();
    let profile_word = to_tcl_string(&vm.pop());

    let profile = if flags & ARG_PROFILE != 0 {
        // Not prefix-matched, unlike the option names: measured, `-profile s`
        // is refused where `-prof tcl8` is accepted.
        match PROFILES.iter().position(|p| *p == profile_word) {
            Some(0) => Profile::Replace,
            Some(1) => Profile::Strict,
            Some(2) => Profile::Tcl8,
            _ => {
                return Err(format!(
                    "bad profile name \"{profile_word}\": must be {}",
                    listing(PROFILES)
                ))
            }
        }
    } else {
        // tclsh 9.0.4's default for this command, both in its source
        // (`int profile = TCL_ENCODING_PROFILE_STRICT`, tclCmdAH.c:452) and
        // measured.
        Profile::Strict
    };

    let name = if flags & ARG_SYSTEM != 0 {
        state().lock().expect("encoding state").0.clone()
    } else {
        name
    };
    let encoding = lookup(&name)?;

    let (text, stop) = if flags & ARG_TO != 0 {
        let source = to_tcl_string(&data);
        let (bytes, stop) = from_utf(&encoding, &source, profile);
        let text: String = bytes.iter().map(|b| char::from(*b)).collect();
        let stop = stop.map(|at| FailedAt {
            // `-failindex` gets the byte offset into the source's UTF-8 form;
            // the error message gets the *character* index of the same place.
            // Measured on tclsh 9.0.4: `encoding convertto -failindex v
            // iso8859-1 éé€` sets v to 4 while the message for the same input
            // says index 2.
            index: at,
            message: {
                let chars = source[..at].chars().count();
                let cp = source[at..].chars().next().map_or(0, u32::from);
                format!("unexpected character at index {chars}: 'U+{cp:06X}'")
            },
        });
        (text, stop)
    } else {
        let bytes = as_bytes(&to_tcl_string(&data))?;
        let (text, stop) = to_utf(&encoding, &bytes, profile)?;
        let stop = stop.map(|at| FailedAt {
            index: at,
            message: format!(
                "unexpected byte sequence starting at index {at}: '\\x{:02X}'",
                bytes.get(at).copied().unwrap_or(0)
            ),
        });
        (text, stop)
    };

    if flags & ARG_FAILINDEX != 0 {
        let index = stop.as_ref().map_or(-1, |f| f.index as i64);
        let place = place_at(&place, flags & ARG_SLOT != 0)?;
        if let Some(cell) = var_cell(vm, place) {
            *cell = Value::Int(index);
        }
    } else if let Some(failed) = stop {
        return Err(failed.message);
    }
    vm.push(Value::Str(Arc::new(text)));
    Ok(())
}

/// Where a conversion stopped, and what to say about it.
struct FailedAt {
    index: usize,
    message: String,
}

/// The bytes a string stands for, refusing a character that is not one.
///
/// `encoding convertfrom` and `binary scan` both reach `Tcl_GetBytesFromObj`,
/// so they refuse the same characters with the same message; this used to be a
/// second copy of that loop, which is how the two came to disagree with each
/// other about the wording. One loop, in `crate::cmd_binary`:
///
/// ```text
/// % encoding convertfrom ascii ééŁ
/// expected code point values below 0xff but value at byte offset 2 was 0x141
/// ```
fn as_bytes(text: &str) -> Result<Vec<u8>, String> {
    crate::cmd_binary::as_bytes(text)
}

// ── the encodings ────────────────────────────────────────────────────────

/// How a conversion error is dealt with: `TCL_ENCODING_PROFILE_*`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Tcl8,
    Strict,
    Replace,
}

/// U+FFFD, `UNICODE_REPLACE_CHAR`.
const REPLACE_CHAR: u32 = 0xFFFD;

/// One encoding, as the reference interpreter's registration of it.
enum Encoding {
    /// A table encoding read from a `.enc` file.
    Table(&'static Table),
    /// `utf-8`: `UtfToUtfProc` with `ENCODING_UTF`.
    Utf8,
    /// `cesu-8`: `UtfToUtfProc` without it, so an astral character is a
    /// surrogate pair of three-byte sequences.
    Cesu8,
    /// `utf-16`, `utf-16le`, `utf-16be` and `unicode`.
    Utf16 { le: bool },
    /// `ucs-2`, `ucs-2le`, `ucs-2be`: decoded as UTF-16, encoded without
    /// surrogate pairs.
    Ucs2 { le: bool },
    /// `utf-32`, `utf-32le`, `utf-32be`.
    Utf32 { le: bool },
}

/// The byte order of this machine, which is what the unsuffixed `utf-16`,
/// `ucs-2` and `utf-32` names mean — `TclInitEncodingSubsystem`'s `leFlags`.
const NATIVE_LE: bool = cfg!(target_endian = "little");

/// The built-in encodings, in the order [`names`] lists them.
const BUILTIN: &[&str] = &[
    "cesu-8", "ucs-2", "ucs-2be", "ucs-2le", "unicode", "utf-16", "utf-16be", "utf-16le", "utf-32",
    "utf-32be", "utf-32le", "utf-8",
];

/// The escape-type encodings the reference release ships and this module does
/// not implement. Named here so the refusal can say which one it was.
const ESCAPE: &[&str] = &["iso2022", "iso2022-jp", "iso2022-kr"];

/// Every encoding this module can actually convert with, sorted.
///
/// `encoding names` answers exactly this, so a script can discover the truth
/// rather than being told about an encoding that would then refuse. tclsh's own
/// answer is in the order of its hash table and includes the three escape
/// encodings; this one is sorted and does not.
pub fn names() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = BUILTIN.to_vec();
    all.extend(crate::encoding_tables::TABLES.iter().map(|(name, _)| *name));
    all.sort_unstable();
    all
}

/// Resolve an encoding by name, or refuse it.
fn lookup(name: &str) -> Result<Encoding, String> {
    match name {
        "utf-8" => return Ok(Encoding::Utf8),
        "cesu-8" => return Ok(Encoding::Cesu8),
        "utf-16" | "unicode" => return Ok(Encoding::Utf16 { le: NATIVE_LE }),
        "utf-16le" => return Ok(Encoding::Utf16 { le: true }),
        "utf-16be" => return Ok(Encoding::Utf16 { le: false }),
        "ucs-2" => return Ok(Encoding::Ucs2 { le: NATIVE_LE }),
        "ucs-2le" => return Ok(Encoding::Ucs2 { le: true }),
        "ucs-2be" => return Ok(Encoding::Ucs2 { le: false }),
        "utf-32" => return Ok(Encoding::Utf32 { le: NATIVE_LE }),
        "utf-32le" => return Ok(Encoding::Utf32 { le: true }),
        "utf-32be" => return Ok(Encoding::Utf32 { le: false }),
        _ => {}
    }
    if let Some(table) = table(name) {
        return Ok(Encoding::Table(table));
    }
    if ESCAPE.contains(&name) {
        return Err(format!(
            "encoding: the escape-sequence encoding \"{name}\" is not supported yet; \
             it is a state machine rather than a table and is absent from \"encoding names\""
        ));
    }
    Err(format!("unknown encoding \"{name}\""))
}

// ── what the channel layer needs ─────────────────────────────────────────

/// The `'static` spelling of a name this module supports, or `None`.
///
/// A channel keeps its encoding's name for `fconfigure -encoding`, and keeping
/// the one from [`names`] rather than the caller's copy is what makes that a
/// `&'static str`.
pub(crate) fn static_name(name: &str) -> Option<&'static str> {
    names().into_iter().find(|candidate| *candidate == name)
}

/// What a channel does with a sequence its encoding cannot represent.
///
/// `strict`, because that is a channel's profile in tclsh 9.0.4 — measured,
/// `fconfigure $f -profile` on a freshly opened channel answers `strict` — and
/// because it is what this frontend's channel layer already *reports* for the
/// option it refuses to set. A strict channel raises rather than substituting,
/// which is why both functions below can fail.
///
/// Note that the `utf-8` and `iso8859-1` arms of [`crate::cmd_channel`] predate
/// this and still substitute; only the encodings that reach this module are
/// strict. Making the other two strict would change what a channel does with a
/// bad byte, which belongs with the channel layer rather than here.
const CHANNEL_PROFILE: Profile = Profile::Strict;

/// The message `Tcl_ReadChars` produces from `EILSEQ`, without the `error
/// reading "channel": ` the channel layer puts in front of it.
pub(crate) const ILLEGAL_SEQUENCE: &str = "invalid or incomplete multibyte or wide character";

/// Decode as much of a channel's pending bytes as is complete.
///
/// The answer is the text and how many bytes it consumed; the rest is an
/// incomplete sequence the channel holds until more arrives, which is what
/// `TCL_CONVERT_MULTIBYTE` means when `TCL_ENCODING_END` is not set.
pub(crate) fn stream_decode(name: &str, src: &[u8]) -> Result<(String, usize), String> {
    let encoding = lookup(name)?;
    let whole = src.len() - incomplete_tail(&encoding, src);
    let (text, stop) = to_utf(&encoding, &src[..whole], CHANNEL_PROFILE)?;
    if stop.is_some() {
        return Err(ILLEGAL_SEQUENCE.to_string());
    }
    Ok((text, whole))
}

/// Encode a channel's text.
pub(crate) fn stream_encode(name: &str, text: &str) -> Result<Vec<u8>, String> {
    let encoding = lookup(name)?;
    let (bytes, stop) = from_utf(&encoding, text, CHANNEL_PROFILE);
    if stop.is_some() {
        return Err(ILLEGAL_SEQUENCE.to_string());
    }
    Ok(bytes)
}

/// How many bytes at the end of `src` are the start of a character whose rest
/// has not arrived.
///
/// Answered by walking the buffer the way the decoder walks it, not by looking
/// at the last byte: in a multi-byte table a trailing byte is only half a
/// character when a character actually *starts* there, and a byte that is a
/// prefix byte in the abstract may be the second half of the pair before it.
fn incomplete_tail(encoding: &Encoding, src: &[u8]) -> usize {
    match encoding {
        Encoding::Table(table) => {
            let mut at = 0;
            while at < src.len() {
                let byte = src[at] as usize;
                if !table.prefix[byte] {
                    at += 1;
                    continue;
                }
                if at + 1 >= src.len() {
                    return src.len() - at;
                }
                // A pair the table has no entry for is not incomplete, it is
                // wrong: leave it in and let the conversion stop on it.
                at += 2;
            }
            0
        }
        Encoding::Utf8 | Encoding::Cesu8 => {
            let mut at = 0;
            while at < src.len() {
                let lead = src[at];
                // `\xC0\x80`, the modified-UTF-8 null, and its truncation.
                if lead == 0xC0 || lead == 0xC1 {
                    if at + 1 >= src.len() {
                        return 1;
                    }
                    at += if src[at + 1] == 0x80 { 2 } else { 1 };
                    continue;
                }
                if let Some((_, len)) = decode_utf8(&src[at..]) {
                    at += len;
                    continue;
                }
                let need = match lead {
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    // Not a lead byte at all: one byte, however the profile
                    // renders it.
                    _ => 1,
                };
                let have = src.len() - at;
                // Truncated only if every byte that *did* arrive belongs to the
                // sequence; a bad continuation byte makes it invalid instead,
                // and an invalid sequence consumes one byte.
                let cut = have < need && src[at + 1..].iter().all(|byte| byte & 0xC0 == 0x80);
                if cut {
                    return have;
                }
                at += 1;
            }
            0
        }
        // A code unit is two bytes; a high surrogate is waiting for its low
        // one.
        Encoding::Utf16 { le } | Encoding::Ucs2 { le } => {
            let odd = src.len() % 2;
            let units = src.len() - odd;
            if units >= 2 {
                let last = &src[units - 2..units];
                let unit = if *le {
                    u32::from(last[0]) | u32::from(last[1]) << 8
                } else {
                    u32::from(last[0]) << 8 | u32::from(last[1])
                };
                if (0xD800..0xDC00).contains(&unit) {
                    return odd + 2;
                }
            }
            odd
        }
        Encoding::Utf32 { .. } => src.len() % 4,
    }
}

// ── the table encodings ──────────────────────────────────────────────────

/// A loaded `.enc` file: `TableEncodingData`.
pub struct Table {
    /// Indexed by the leading byte; `None` where the file had no such page.
    to_unicode: Vec<Option<Box<[u16; 256]>>>,
    /// Indexed by the code point's high byte, the same shape inverted.
    from_unicode: Vec<Option<Box<[u16; 256]>>>,
    /// Which bytes begin a two-byte sequence.
    prefix: [bool; 256],
    /// What an unencodable character becomes under the `tcl8` and `replace`
    /// profiles.
    fallback: u16,
}

/// The tables that have been read, keyed by name. A `.enc` file is parsed once
/// per process and only if a script names it, so a script that uses one
/// encoding pays for one table.
fn table(name: &str) -> Option<&'static Table> {
    static LOADED: OnceLock<Mutex<HashMap<&'static str, &'static Table>>> = OnceLock::new();
    let cache = LOADED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("encoding table cache");
    let (name, text) = crate::encoding_tables::TABLES
        .iter()
        .find(|(candidate, _)| *candidate == name)?;
    if let Some(table) = cache.get(name) {
        return Some(table);
    }
    // Leaked deliberately: a table is immutable and lives as long as the
    // process, and a `&'static` reference is what keeps the conversion
    // functions free of a lifetime that would otherwise reach every caller.
    let table: &'static Table = Box::leak(Box::new(load_table(text)?));
    cache.insert(name, table);
    Some(table)
}

/// Parse a `.enc` file.
///
/// A port of `LoadTableEncoding` (`generic/tclEncoding.c:1954`), including the
/// inversion that produces `fromUnicode` from `toUnicode`, the multi-byte
/// backslash repair and the symbol-font page, and the trailing `R` section of
/// [Patch 689341] that four of the Japanese tables carry.
fn load_table(text: &str) -> Option<Table> {
    let mut lines = text.lines();
    lines.next()?; // the comment line
    let kind = lines.next()?.trim();
    let header = lines.next()?;

    // `strtol(line, &line, 16)` then two base-10 reads off the same cursor.
    let mut fields = header.split_whitespace();
    let fallback = u16::from_str_radix(fields.next()?, 16).ok()?;
    let symbol: u32 = fields.next()?.parse().ok()?;
    let pages: usize = fields.next()?.parse().ok()?;
    let pages = pages.min(256);

    let mut to_unicode: Vec<Option<Box<[u16; 256]>>> = (0..256).map(|_| None).collect();
    for _ in 0..pages {
        let hi = hex4(lines.next()?.as_bytes(), 0) >> 8;
        let mut page = Box::new([0u16; 256]);
        for row in 0..16 {
            let bytes = lines.next()?.as_bytes();
            for column in 0..16 {
                page[row * 16 + column] = hex4(bytes, column * 4);
            }
        }
        to_unicode[hi as usize] = Some(page);
    }

    let mut prefix = [false; 256];
    if kind == "D" {
        prefix = [true; 256];
    } else {
        for (hi, page) in to_unicode.iter().enumerate().skip(1) {
            prefix[hi] = page.is_some();
        }
    }

    let mut from_unicode: Vec<Option<Box<[u16; 256]>>> = (0..256).map(|_| None).collect();
    if symbol != 0 {
        from_unicode[0] = Some(Box::new([0u16; 256]));
    }
    for (hi, slot) in to_unicode.iter().enumerate() {
        let Some(page) = slot else { continue };
        for (lo, entry) in page.iter().enumerate() {
            let ch = *entry as usize;
            if ch == 0 {
                continue;
            }
            from_unicode[ch >> 8]
                .get_or_insert_with(|| Box::new([0u16; 256]))
                .as_mut()[ch & 0xFF] = ((hi << 8) | lo) as u16;
        }
    }
    if kind == "M" {
        if let Some(page) = &mut from_unicode[0] {
            if page[usize::from(b'\\')] == 0 {
                page[usize::from(b'\\')] = u16::from(b'\\');
            }
        }
    }
    if symbol != 0 {
        // Every character the font does have on page 0 maps to itself, so a
        // plain ASCII string can be shown in a symbol font.
        let zero = to_unicode[0]
            .clone()
            .unwrap_or_else(|| Box::new([0u16; 256]));
        let page = from_unicode[0].get_or_insert_with(|| Box::new([0u16; 256]));
        for lo in 0..256 {
            if zero[lo] != 0 {
                page[lo] = lo as u16;
            }
        }
    }

    // The trailing reverse section: lines of `TTTT FFFF FFFF …`, each mapping
    // every code point named after the first onto the encoded value first.
    let mut rest = lines.skip_while(|line| line.is_empty());
    if rest.next().is_some_and(|line| line.starts_with('R')) {
        for line in rest {
            let bytes = line.as_bytes();
            if bytes.len() < 5 {
                continue;
            }
            let to = hex4(bytes, 0);
            if to == 0 {
                continue;
            }
            let mut p = 5;
            while p + 4 <= bytes.len() {
                let from = hex4(bytes, p) as usize;
                p += 5;
                if from == 0 {
                    continue;
                }
                from_unicode[from >> 8]
                    .get_or_insert_with(|| Box::new([0u16; 256]))
                    .as_mut()[from & 0xFF] = to;
            }
        }
    }

    Some(Table {
        to_unicode,
        from_unicode,
        prefix,
        fallback,
    })
}

/// Four hex digits at `at`, with a non-digit reading as zero — which is what
/// `LoadTableEncoding`'s `staticHex` table does.
fn hex4(bytes: &[u8], at: usize) -> u16 {
    let mut value = 0u16;
    for offset in 0..4 {
        let digit = match bytes.get(at + offset) {
            Some(b'0'..=b'9') => bytes[at + offset] - b'0',
            Some(b'a'..=b'f') => bytes[at + offset] - b'a' + 10,
            Some(b'A'..=b'F') => bytes[at + offset] - b'A' + 10,
            _ => 0,
        };
        value = (value << 4) | u16::from(digit);
    }
    value
}

/// A table encoding's page, or the empty one.
fn page(pages: &[Option<Box<[u16; 256]>>], hi: usize) -> &[u16; 256] {
    const EMPTY: [u16; 256] = [0; 256];
    pages[hi].as_deref().unwrap_or(&EMPTY)
}

// ── decoding ─────────────────────────────────────────────────────────────

/// Decode `src` into a string, stopping where the profile says to.
///
/// The second half of the answer is where it stopped, as a count of bytes
/// consumed — the `nBytesProcessed` that `Tcl_ExternalToUtfDStringEx` reports
/// as either an error location or an error message. `Err` is this frontend
/// refusing, which is a different thing from the conversion failing.
fn to_utf(
    encoding: &Encoding,
    src: &[u8],
    profile: Profile,
) -> Result<(String, Option<usize>), String> {
    match encoding {
        Encoding::Table(table) => Ok(table_to_utf(table, src, profile)),
        Encoding::Utf8 => utf8_to_utf(src, profile, true),
        Encoding::Cesu8 => utf8_to_utf(src, profile, false),
        Encoding::Utf16 { le } | Encoding::Ucs2 { le } => utf16_to_utf(src, *le, profile),
        Encoding::Utf32 { le } => utf32_to_utf(src, *le, profile),
    }
}

/// Append a code point, refusing the surrogates a `String` cannot hold.
///
/// Only the `tcl8` profile ever reaches one: `strict` stops and `replace`
/// substitutes U+FFFD, but tcl8 passes an unpaired surrogate through, and
/// tclsh's string can carry it where this one cannot.
fn push_char(out: &mut String, cp: u32) -> Result<(), String> {
    match char::from_u32(cp) {
        Some(ch) => {
            out.push(ch);
            Ok(())
        }
        None => Err(format!(
            "encoding convertfrom: the tcl8 profile decodes this input to the lone surrogate \
             U+{cp:04X}, which a string in this frontend cannot hold"
        )),
    }
}

/// A port of `TableToUtfProc` (`generic/tclEncoding.c:3447`).
fn table_to_utf(table: &Table, src: &[u8], profile: Profile) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut at = 0;
    while at < src.len() {
        let byte = src[at] as usize;
        let mut consumed = 1;
        let mut ch = if table.prefix[byte] {
            if at + 1 >= src.len() {
                // A prefix byte with nothing after it, and no more data is
                // coming. tclsh does *not* fall back to cp1252 here — see the
                // note on [1355b9a874] in `TableToUtfProc`.
                match profile {
                    Profile::Strict => return (out, Some(at)),
                    Profile::Replace => REPLACE_CHAR,
                    Profile::Tcl8 => byte as u32,
                }
            } else {
                consumed = 2;
                u32::from(page(&table.to_unicode, byte)[src[at + 1] as usize])
            }
        } else {
            u32::from(page(&table.to_unicode, 0)[byte])
        };
        if ch == 0 && byte != 0 {
            // The pair was not in the table.
            if profile == Profile::Strict {
                return (out, Some(at + consumed - 1));
            }
            consumed = 1;
            ch = match profile {
                Profile::Replace => REPLACE_CHAR,
                _ => lone_byte(byte as u8),
            };
        }
        // A table never maps to a surrogate — asserted for every vendored file
        // — and `lone_byte` cannot produce one, so this is infallible.
        out.push(char::from_u32(ch).unwrap_or(char::REPLACEMENT_CHARACTER));
        at += consumed;
    }
    (out, None)
}

/// What the `tcl8` profile makes of a byte that is not valid where it stands:
/// `TclUtfToUniChar` on that byte alone.
///
/// tclsh reads a stray byte as the cp1252 character of that number when cp1252
/// defines one and as the code point of that number when it does not.
/// Measured: `encoding convertfrom -profile tcl8 ascii A\x80` is U+0041 U+20AC
/// and `…A\x81` is U+0041 U+0081. `encoding(n)` describes this as happening
/// only when converting *from utf-8*; it happens for the table encodings too.
fn lone_byte(byte: u8) -> u32 {
    // `library/encoding/cp1252.enc`'s page 0, rows 8 and 9 — the only rows
    // where cp1252 differs from the code points of its own byte values.
    const CP1252_HIGH: [u16; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&byte) {
        u32::from(CP1252_HIGH[usize::from(byte) - 0x80])
    } else {
        u32::from(byte)
    }
}

/// A port of `UtfToUtfProc` in its decoding direction (`ENCODING_INPUT`),
/// which is both `utf-8` and `cesu-8`.
fn utf8_to_utf(src: &[u8], profile: Profile, utf: bool) -> Result<(String, Option<usize>), String> {
    let mut out = String::new();
    let mut at = 0;
    // A high surrogate seen but not yet paired, which only cesu-8 produces.
    let mut pending: Option<u32> = None;
    while at < src.len() {
        // `\xC0\x80` is the null byte in Tcl's modified UTF-8. Under tcl8 it
        // decodes to U+0000; the other two profiles refuse it as an
        // overlong form.
        if src[at] == 0xC0 && at + 1 < src.len() && src[at + 1] == 0x80 {
            if let Some(high) = pending.take() {
                match profile {
                    Profile::Strict => return Ok((out, Some(at))),
                    Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                    Profile::Tcl8 => push_char(&mut out, high)?,
                }
            }
            match profile {
                Profile::Tcl8 => {
                    out.push('\0');
                    at += 2;
                }
                Profile::Replace => {
                    push_char(&mut out, REPLACE_CHAR)?;
                    at += 2;
                }
                Profile::Strict => return Ok((out, Some(at))),
            }
            continue;
        }
        let (cp, len) = match decode_utf8(&src[at..]) {
            Some(pair) => pair,
            None => {
                // A truncated or otherwise invalid sequence.
                if let Some(high) = pending.take() {
                    match profile {
                        Profile::Strict => return Ok((out, Some(at))),
                        Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                        Profile::Tcl8 => push_char(&mut out, high)?,
                    }
                    continue;
                }
                match profile {
                    Profile::Strict => return Ok((out, Some(at))),
                    Profile::Replace => {
                        push_char(&mut out, REPLACE_CHAR)?;
                        at += 1;
                    }
                    Profile::Tcl8 => {
                        push_char(&mut out, lone_byte(src[at]))?;
                        at += 1;
                    }
                }
                continue;
            }
        };
        if is_surrogate(cp) {
            if utf {
                // utf-8, where a surrogate is never valid.
                if let Some(high) = pending.take() {
                    push_char(&mut out, high)?;
                }
                match profile {
                    Profile::Strict => return Ok((out, Some(at))),
                    Profile::Replace => {
                        push_char(&mut out, REPLACE_CHAR)?;
                        at += len;
                    }
                    Profile::Tcl8 => {
                        push_char(&mut out, cp)?;
                        at += len;
                    }
                }
                continue;
            }
            // cesu-8, where a pair is how an astral character is written.
            if (0xDC00..0xE000).contains(&cp) {
                match pending.take() {
                    Some(high) => {
                        push_char(&mut out, 0x10000 + ((high - 0xD800) << 10) + (cp - 0xDC00))?;
                        at += len;
                    }
                    None => match profile {
                        Profile::Strict => return Ok((out, Some(at))),
                        Profile::Replace => {
                            push_char(&mut out, REPLACE_CHAR)?;
                            at += len;
                        }
                        Profile::Tcl8 => {
                            push_char(&mut out, cp)?;
                            at += len;
                        }
                    },
                }
                continue;
            }
            // A high surrogate. One already pending means the first was
            // isolated.
            if let Some(high) = pending.take() {
                match profile {
                    Profile::Strict => return Ok((out, Some(at))),
                    Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                    Profile::Tcl8 => push_char(&mut out, high)?,
                }
            }
            pending = Some(cp);
            at += len;
            continue;
        }
        if let Some(high) = pending.take() {
            match profile {
                Profile::Strict => return Ok((out, Some(at))),
                Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                Profile::Tcl8 => push_char(&mut out, high)?,
            }
        }
        // cesu-8 has no four-byte form: an astral character written as one is
        // invalid input.
        if !utf && cp > 0xFFFF {
            match profile {
                Profile::Strict => return Ok((out, Some(at))),
                Profile::Replace => {
                    push_char(&mut out, REPLACE_CHAR)?;
                    at += len;
                }
                Profile::Tcl8 => {
                    push_char(&mut out, cp)?;
                    at += len;
                }
            }
            continue;
        }
        push_char(&mut out, cp)?;
        at += len;
    }
    if let Some(high) = pending {
        match profile {
            Profile::Strict => return Ok((out, Some(src.len()))),
            Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
            Profile::Tcl8 => push_char(&mut out, high)?,
        }
    }
    Ok((out, None))
}

/// One code point at the head of `src`, and how many bytes it took, by the
/// same rules `Tcl_UtfCharComplete`/`TclUtfToUniChar` apply: a surrogate is a
/// legal three-byte sequence, an overlong or out-of-range form is not.
fn decode_utf8(src: &[u8]) -> Option<(u32, usize)> {
    let lead = src[0];
    let (len, mut cp) = match lead {
        0x00..=0x7F => return Some((u32::from(lead), 1)),
        0xC2..=0xDF => (2, u32::from(lead & 0x1F)),
        0xC0..=0xC1 => return None, // overlong two-byte form
        0xE0..=0xEF => (3, u32::from(lead & 0x0F)),
        0xF0..=0xF4 => (4, u32::from(lead & 0x07)),
        _ => return None,
    };
    if src.len() < len {
        return None;
    }
    for byte in &src[1..len] {
        if byte & 0xC0 != 0x80 {
            return None;
        }
        cp = (cp << 6) | u32::from(byte & 0x3F);
    }
    // An overlong three- or four-byte form, or past the last code point.
    if (len == 3 && cp < 0x800) || (len == 4 && cp < 0x10000) || cp > 0x10FFFF {
        return None;
    }
    Some((cp, len))
}

fn is_surrogate(cp: u32) -> bool {
    (0xD800..0xE000).contains(&cp)
}

/// A port of `Utf16ToUtfProc` (`generic/tclEncoding.c:2992`), which decodes
/// both `utf-16` and `ucs-2`.
fn utf16_to_utf(src: &[u8], le: bool, profile: Profile) -> Result<(String, Option<usize>), String> {
    let mut out = String::new();
    let whole = src.len() - src.len() % 2;
    let mut at = 0;
    let mut pending: Option<u32> = None;
    while at < whole {
        let unit = if le {
            u32::from(src[at]) | u32::from(src[at + 1]) << 8
        } else {
            u32::from(src[at]) << 8 | u32::from(src[at + 1])
        };
        if let Some(high) = pending.take() {
            if (0xDC00..0xE000).contains(&unit) {
                push_char(
                    &mut out,
                    0x10000 + ((high - 0xD800) << 10) + (unit - 0xDC00),
                )?;
                at += 2;
                continue;
            }
            // The high surrogate stood alone. tclsh rewinds and re-reads this
            // unit as a character of its own.
            match profile {
                Profile::Strict => return Ok((out, Some(at - 2))),
                Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                Profile::Tcl8 => push_char(&mut out, high)?,
            }
            continue;
        }
        if (0xD800..0xDC00).contains(&unit) {
            pending = Some(unit);
            at += 2;
            continue;
        }
        if is_surrogate(unit) {
            // An isolated low surrogate.
            match profile {
                Profile::Strict => return Ok((out, Some(at))),
                Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
                Profile::Tcl8 => push_char(&mut out, unit)?,
            }
            at += 2;
            continue;
        }
        push_char(&mut out, unit)?;
        at += 2;
    }
    if let Some(high) = pending {
        match profile {
            Profile::Strict => return Ok((out, Some(whole - 2))),
            Profile::Replace => push_char(&mut out, REPLACE_CHAR)?,
            Profile::Tcl8 => push_char(&mut out, high)?,
        }
    }
    if whole != src.len() {
        // A trailing byte with no partner, and no more data coming.
        match profile {
            Profile::Strict => return Ok((out, Some(whole))),
            _ => push_char(&mut out, REPLACE_CHAR)?,
        }
    }
    Ok((out, None))
}

/// A port of `Utf32ToUtfProc` (`generic/tclEncoding.c:2762`).
fn utf32_to_utf(src: &[u8], le: bool, profile: Profile) -> Result<(String, Option<usize>), String> {
    let mut out = String::new();
    let whole = src.len() - src.len() % 4;
    let mut at = 0;
    while at < whole {
        let quad = &src[at..at + 4];
        let cp = if le {
            u32::from(quad[3]) << 24
                | u32::from(quad[2]) << 16
                | u32::from(quad[1]) << 8
                | u32::from(quad[0])
        } else {
            u32::from(quad[0]) << 24
                | u32::from(quad[1]) << 16
                | u32::from(quad[2]) << 8
                | u32::from(quad[3])
        };
        let cp = if cp > 0x10FFFF {
            if profile == Profile::Strict {
                return Ok((out, Some(at)));
            }
            REPLACE_CHAR
        } else if is_surrogate(cp) {
            match profile {
                Profile::Strict => return Ok((out, Some(at))),
                Profile::Replace => REPLACE_CHAR,
                Profile::Tcl8 => cp,
            }
        } else {
            cp
        };
        push_char(&mut out, cp)?;
        at += 4;
    }
    if whole != src.len() {
        match profile {
            Profile::Strict => return Ok((out, Some(whole))),
            _ => push_char(&mut out, REPLACE_CHAR)?,
        }
    }
    Ok((out, None))
}

// ── encoding ─────────────────────────────────────────────────────────────

/// Encode `src`, stopping where the profile says to.
///
/// The second half of the answer is the byte offset into `src` where it
/// stopped, which is what `-failindex` receives.
fn from_utf(encoding: &Encoding, src: &str, profile: Profile) -> (Vec<u8>, Option<usize>) {
    match encoding {
        Encoding::Table(table) => table_from_utf(table, src, profile),
        Encoding::Utf8 => (src.as_bytes().to_vec(), None),
        Encoding::Cesu8 => cesu8_from_utf(src),
        Encoding::Utf16 { le } => utf16_from_utf(src, *le),
        Encoding::Ucs2 { le } => ucs2_from_utf(src, *le, profile),
        Encoding::Utf32 { le } => utf32_from_utf(src, *le),
    }
}

/// A port of `TableFromUtfProc` (`generic/tclEncoding.c:3596`).
fn table_from_utf(table: &Table, src: &str, profile: Profile) -> (Vec<u8>, Option<usize>) {
    let mut out = Vec::new();
    for (at, ch) in src.char_indices() {
        let cp = u32::from(ch);
        // Nothing above U+FFFF is in any table.
        let mut word = if cp > 0xFFFF {
            0
        } else {
            page(&table.from_unicode, (cp >> 8) as usize)[(cp & 0xFF) as usize]
        };
        if word == 0 && cp != 0 {
            if profile == Profile::Strict {
                return (out, Some(at));
            }
            word = table.fallback;
        }
        if table.prefix[usize::from(word >> 8)] {
            out.push((word >> 8) as u8);
            out.push(word as u8);
        } else {
            out.push(word as u8);
        }
    }
    (out, None)
}

/// `cesu-8`: `UtfToUtfProc` in its encoding direction, where an astral
/// character becomes the two three-byte sequences of its surrogate pair.
///
/// Never fails: this frontend's strings hold no surrogates, so the isolated
/// ones the reference proc has to deal with cannot occur.
fn cesu8_from_utf(src: &str) -> (Vec<u8>, Option<usize>) {
    let mut out = Vec::new();
    for ch in src.chars() {
        let cp = u32::from(ch);
        if cp > 0xFFFF {
            let cp = cp - 0x10000;
            push_three(&mut out, 0xD800 + (cp >> 10));
            push_three(&mut out, 0xDC00 + (cp & 0x3FF));
        } else if cp > 0x7FF {
            push_three(&mut out, cp);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    (out, None)
}

/// One three-byte UTF-8 sequence, surrogate or not, which is why this is not
/// `char::encode_utf8`.
fn push_three(out: &mut Vec<u8>, cp: u32) {
    out.push(0xE0 | (cp >> 12) as u8);
    out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
    out.push(0x80 | (cp & 0x3F) as u8);
}

/// A port of `UtfToUtf16Proc` (`generic/tclEncoding.c:3245`).
fn utf16_from_utf(src: &str, le: bool) -> (Vec<u8>, Option<usize>) {
    let mut out = Vec::new();
    for ch in src.chars() {
        let cp = u32::from(ch);
        if cp > 0xFFFF {
            let cp = cp - 0x10000;
            push_unit(&mut out, 0xD800 + (cp >> 10), le);
            push_unit(&mut out, 0xDC00 + (cp & 0x3FF), le);
        } else {
            push_unit(&mut out, cp, le);
        }
    }
    (out, None)
}

/// A port of `UtfToUcs2Proc` (`generic/tclEncoding.c:3305`), which has no
/// surrogate pairs: a character above U+FFFF stops a strict conversion and
/// becomes U+FFFD under the other two profiles. Measured on tclsh 9.0.4:
/// `encoding convertto -profile tcl8 ucs-2 𐐷` is the two bytes FD FF.
fn ucs2_from_utf(src: &str, le: bool, profile: Profile) -> (Vec<u8>, Option<usize>) {
    let mut out = Vec::new();
    for (at, ch) in src.char_indices() {
        let cp = u32::from(ch);
        let cp = if cp > 0xFFFF {
            if profile == Profile::Strict {
                return (out, Some(at));
            }
            REPLACE_CHAR
        } else {
            cp
        };
        push_unit(&mut out, cp, le);
    }
    (out, None)
}

fn push_unit(out: &mut Vec<u8>, unit: u32, le: bool) {
    if le {
        out.push(unit as u8);
        out.push((unit >> 8) as u8);
    } else {
        out.push((unit >> 8) as u8);
        out.push(unit as u8);
    }
}

/// A port of `UtfToUtf32Proc` (`generic/tclEncoding.c:2878`).
fn utf32_from_utf(src: &str, le: bool) -> (Vec<u8>, Option<usize>) {
    let mut out = Vec::new();
    for ch in src.chars() {
        let cp = u32::from(ch);
        let quad = [
            (cp >> 24) as u8,
            (cp >> 16) as u8,
            (cp >> 8) as u8,
            cp as u8,
        ];
        if le {
            out.extend(quad.iter().rev());
        } else {
            out.extend_from_slice(&quad);
        }
    }
    (out, None)
}
