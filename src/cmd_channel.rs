//! Channels: the generic layer, and the commands a script reaches it through.
//!
//! Tcl's I/O is two layers, and the split is load-bearing rather than
//! decorative. `generic/tclIO.c` owns everything a channel does that has
//! nothing to do with where the bytes come from — the name, the reference
//! count, the encoding, the end-of-line translation, the buffering mode, the
//! end-of-file rule — and a *driver* owns only `read`, `write`, `seek` and
//! `close`. A driver is a `Tcl_ChannelType`, a table of function pointers
//! (`generic/tcl.h:1445-1494`), and the whole of `tclUnixChan.c`,
//! `tclUnixPipe.c` and Tk's own console channel are drivers in that sense.
//!
//! This module is the generic layer. [`Device`] is the driver interface, with
//! one implementation here per device this crate can open by itself — a file,
//! and the three the process was started with. The other implementation is
//! `tk::channel::Driver`, behind the `tk` feature, which forwards to a
//! `Tcl_ChannelType` Tk supplied — so a channel Tk creates lives in the same
//! table, answers to the same `fconfigure` and is written to by the same `puts`
//! as one a script opened.
//!
//! # Where the table lives
//!
//! One table per thread, exactly as Tcl's is: `tclIO.c` reaches its channel
//! list, and all three standard channels, through
//! `ThreadSpecificData *tsdPtr = TCL_TSD_INIT(&dataKey)`
//! (`generic/tclIO.c:1607`, `:724`, `:768`). A test that opens a file and a
//! test that redirects stdout therefore cannot see each other's channels even
//! though `cargo test` runs them in one process, which is the same isolation
//! two threads of a Tcl program get.
//!
//! # What is not here
//!
//! * **Stacked channels.** `Tcl_StackChannel` (`generic/tclIO.c:1796`) puts one
//!   driver on top of another and is how `zlib push` and `tls` work; every
//!   `topChanPtr` / `bottomChanPtr` hop in `tclIO.c` exists for it. There is
//!   one driver per channel here, and the slot is refused rather than faked.
//! * **Non-blocking I/O and background flushing.** `-blocking` is recorded and
//!   reported, and refused when set to `0`: a channel that answers "non-blocking"
//!   and then blocks anyway would be worse than one that says it cannot.
//! * **Encodings other than UTF-8 and ISO 8859-1.** Those two are what
//!   `-translation binary` and the default need; `Tcl_GetEncoding`'s table of
//!   the rest is not ported, and naming another one is refused by name.
//! * **`-eofchar` and `-profile`**, which are reported at their defaults and
//!   refused when set.
//!
//! # Re-entrancy
//!
//! A driver proc may call back into this layer, and one that Tk supplies does:
//! its console `outputProc` builds a `tk::ConsoleOutput` command and evaluates
//! it (`tk9.0.4/generic/tkConsole.c:520-536`), and that script can write to a
//! channel again. Every path that calls a driver for *writing*, *closing* or
//! *watching* therefore goes through [`with_device_detached`], which lends the
//! device out and puts it back rather than holding the table's borrow across
//! the call.
//!
//! The *read* path does not, and is the one place a re-entrant driver would
//! still be a panic rather than an answer. It is deliberate: reading needs the
//! channel's translation and encoding state as well as its device, so lending
//! only the device out would not be enough, and no driver here or in Tk reads
//! re-entrantly — Tk's console `inputProc` returns end of file and nothing else
//! (`tk9.0.4/generic/tkConsole.c:559-568`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{place_at, to_tcl_string, var_cell, Output};

// ── the driver interface ─────────────────────────────────────────────────

/// `TCL_READABLE` (`generic/tcl.h:1349`).
pub const TCL_READABLE: i32 = 1 << 1;
/// `TCL_WRITABLE` (`generic/tcl.h:1350`).
pub const TCL_WRITABLE: i32 = 1 << 2;
/// `TCL_EXCEPTION` (`generic/tcl.h:1351`).
pub const TCL_EXCEPTION: i32 = 1 << 3;

/// `TCL_STDIN` (`generic/tcl.h:1359`).
pub const TCL_STDIN: i32 = 1 << 1;
/// `TCL_STDOUT` (`generic/tcl.h:1360`).
pub const TCL_STDOUT: i32 = 1 << 2;
/// `TCL_STDERR` (`generic/tcl.h:1361`).
pub const TCL_STDERR: i32 = 1 << 3;

/// `CHANNELBUFFER_DEFAULT_SIZE` (`generic/tclIO.h:67`), which is what
/// `Tcl_CreateChannel` puts in `statePtr->bufSize`
/// (`generic/tclIO.c:1698`) and what `fconfigure -buffersize` reports.
pub const DEFAULT_BUFFER_SIZE: i64 = 4096;

/// Where a seek starts from — `SEEK_SET`, `SEEK_CUR`, `SEEK_END`, which is what
/// `Tcl_Seek`'s `mode` argument is (`generic/tclIO.c:7161-7166`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Whence {
    Start,
    Current,
    End,
}

/// One device behind a channel: the four operations `Tcl_ChannelType` calls a
/// driver for, and the two questions the generic layer asks about it.
///
/// The signatures are the driver procs', reshaped to Rust's error convention:
/// `Tcl_DriverInputProc` and `Tcl_DriverOutputProc` return a byte count and
/// write an errno through `errorCodePtr` (`generic/tcl.h:1402-1407`), and every
/// caller here wants the message rather than the number.
pub trait Device {
    /// `typeName` (`generic/tcl.h:1446`) — `file`, `console`, and so on.
    fn type_name(&self) -> &str;

    /// `inputProc`. Zero bytes means end of file, which is the only way a
    /// driver can report one (`generic/tclIO.c`'s `ChanRead`).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;

    /// `outputProc`.
    fn write(&mut self, buf: &[u8]) -> Result<usize, String>;

    /// `wideSeekProc`. `None` from [`Device::seekable`] means the driver has
    /// no seek proc at all, which is what makes `seek stdin 0` an error.
    fn seek(&mut self, _offset: i64, _whence: Whence) -> Result<i64, String> {
        Err("illegal seek".to_string())
    }

    /// Whether the driver has a `wideSeekProc`. A channel whose driver has none
    /// reports `error during seek on "%s": illegal seek`
    /// (`generic/tclIO.c:7183-7189`).
    fn seekable(&self) -> bool {
        false
    }

    /// `close2Proc`, called once when the last reference goes.
    fn close(&mut self) -> Result<(), String>;

    /// `getHandleProc` — the operating-system handle, when there is one.
    /// `Tcl_GetChannelHandle` answers `TCL_ERROR` when there is not
    /// (`generic/tclIO.c:2397-2420`).
    fn handle(&self, _direction: i32) -> Option<isize> {
        None
    }

    /// `watchProc`, told which events the generic layer is interested in.
    /// Called by [`create_channel_handler`] and [`delete_channel_handler`],
    /// which is the only path in `tclIO.c` that reaches it too
    /// (`generic/tclIO.c:8874-8949`).
    fn watch(&mut self, _mask: i32) {}

    /// `setOptionProc`, when the driver has one. `None` — the default — is a
    /// driver with a NULL `setOptionProc`, and is what turns an unrecognised
    /// option into `Tcl_BadChannelOption`'s error
    /// (`generic/tclIO.c:8463-8468`).
    fn set_option(&mut self, _name: &str, _value: &str) -> Option<Result<(), String>> {
        None
    }

    /// `getOptionProc`, when the driver has one (`generic/tclIO.c:8100-8110`).
    fn get_option(&mut self, _name: &str) -> Option<Result<String, String>> {
        None
    }

    /// The address of the `Tcl_ChannelType` this device forwards to, when it
    /// forwards to one. `Tcl_GetChannelType` has to return the very pointer the
    /// driver registered, because Tk finds its console channel by comparing
    /// addresses (`tk9.0.4/generic/tkConsole.c:361-366`).
    ///
    /// An address rather than a typed pointer, so that the generic layer stays
    /// free of the C ABI: the one caller that needs the type casts it back.
    fn driver_table(&self) -> Option<usize> {
        None
    }

    /// `chanPtr->instanceData` (`generic/tclIO.c:1637`), for
    /// `Tcl_GetChannelInstanceData`.
    fn instance_data(&self) -> Option<usize> {
        None
    }
}

// ── end-of-line translation and buffering ────────────────────────────────

/// `TclEolTranslation` (`generic/tclIO.h:31-36`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Translation {
    /// `TCL_TRANSLATE_AUTO`: on input, any of `\n`, `\r` and `\r\n` ends a
    /// line. Not a valid *output* mode — `Tcl_SetChannelOption` maps `auto` on
    /// the write side to the platform's own (`generic/tclIO.c:8427-8438`).
    Auto,
    /// `TCL_TRANSLATE_LF`, and `TCL_PLATFORM_TRANSLATION` on anything but
    /// Windows (`generic/tcl.h`).
    Lf,
    /// `TCL_TRANSLATE_CR`.
    Cr,
    /// `TCL_TRANSLATE_CRLF`.
    Crlf,
}

impl Translation {
    /// The word `fconfigure -translation` answers with.
    fn name(self) -> &'static str {
        match self {
            Translation::Auto => "auto",
            Translation::Lf => "lf",
            Translation::Cr => "cr",
            Translation::Crlf => "crlf",
        }
    }
}

/// The three states `CHANNEL_LINEBUFFERED` and `CHANNEL_UNBUFFERED` encode
/// between them (`generic/tclIO.h`, and `Tcl_SetChannelOption`'s `-buffering`
/// arm at `generic/tclIO.c:8237-8255`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Buffering {
    Full,
    Line,
    None,
}

impl Buffering {
    fn name(self) -> &'static str {
        match self {
            Buffering::Full => "full",
            Buffering::Line => "line",
            Buffering::None => "none",
        }
    }
}

/// The encodings this layer converts between.
///
/// The two the channel machinery itself needs have their own arms because they
/// are what every channel starts as and what `-translation binary` switches to,
/// and because both are a straight walk over the bytes with no table to consult.
/// Everything else goes through [`crate::cmd_encoding`], which owns the tables
/// and the ported conversion procs, so `fconfigure -encoding` accepts exactly
/// the set `encoding names` lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Encoding {
    /// The system encoding on every platform this runs on, and what
    /// `Tcl_CreateChannel` installs (`generic/tclIO.c:1667-1668`).
    Utf8,
    /// What `-translation binary` switches to
    /// (`generic/tclIO.c:8392-8393`, `:8441-8442`): one byte, one character.
    Iso8859_1,
    /// Any other encoding `encoding names` offers, by the name that list holds
    /// — which is why this is a `&'static str` and not a `String`.
    Named(&'static str),
}

impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Iso8859_1 => "iso8859-1",
            Encoding::Named(name) => name,
        }
    }
}

// ── the channel ──────────────────────────────────────────────────────────

/// A channel: `Channel` and `ChannelState` of `generic/tclIO.h` with the
/// stacking fields dropped, since there is one driver per channel here.
struct Channel {
    /// `statePtr->channelName` (`generic/tclIO.c:1659`).
    name: String,
    device: Box<dyn Device>,
    /// `statePtr->flags & (TCL_READABLE|TCL_WRITABLE)`, which is the `mask`
    /// `Tcl_CreateChannel` was given (`generic/tclIO.c:1660`).
    mode: i32,
    /// `statePtr->refCount` (`generic/tclIO.c:1687`): how many interpreters
    /// have the channel registered, plus one artificial reference for a
    /// standard channel (`generic/tclIO.c:782-793`).
    ref_count: isize,
    input_translation: Translation,
    output_translation: Translation,
    encoding: Encoding,
    buffering: Buffering,
    buffer_size: i64,
    blocking: bool,

    /// Bytes read from the device and not yet decoded. Non-empty only when a
    /// multi-byte character straddled the end of a read.
    raw: Vec<u8>,
    /// Characters decoded and translated, not yet handed to a script.
    pending: String,
    /// `CHANNEL_EOF` (`generic/tclIO.h`): the device answered a read with zero
    /// bytes. Cleared by a seek, as `Tcl_Seek` does
    /// (`generic/tclIO.c:7245-7250`).
    device_eof: bool,
    /// `INPUT_SAW_CR` (`generic/tclIO.h`): the last translated character was a
    /// carriage return, so an `\n` opening the next buffer belongs to it.
    saw_cr: bool,
    /// Bytes written and not yet handed to the device.
    out: Vec<u8>,

    /// Handlers registered by [`create_channel_handler`], as a mask and the two
    /// pointers `Tcl_CreateChannelHandler` was given
    /// (`generic/tclIO.c:8874-8888`).
    handlers: Vec<ChannelHandler>,
}

/// One entry of `statePtr->chPtr`'s list (`generic/tclIO.h`'s `ChannelHandler`).
struct ChannelHandler {
    mask: i32,
    proc: usize,
    client_data: usize,
}

impl Channel {
    fn readable(&self) -> bool {
        self.mode & TCL_READABLE != 0
    }

    fn writable(&self) -> bool {
        self.mode & TCL_WRITABLE != 0
    }
}

/// `ThreadSpecificData` of `generic/tclIO.c`: every channel this thread has
/// open, and which of them the three standard ones are.
#[derive(Default)]
struct Table {
    channels: HashMap<usize, Channel>,
    /// Name to id. `Tcl_GetChannel` looks a channel up by name
    /// (`generic/tclIO.c:1462-1469`), and two live channels may not share one.
    names: HashMap<String, usize>,
    next_id: usize,
    /// `stdinChannel`, `stdoutChannel`, `stderrChannel`
    /// (`generic/tclIO.c:724-744`), in that order.
    std: [Option<usize>; 3],
    /// `stdinInitialized` and friends: whether the slot has been consulted, so
    /// that a channel closed explicitly is not silently recreated
    /// (`generic/tclIO.c:777-778`).
    std_initialized: [bool; 3],
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

/// Run `f` against this thread's channel table.
fn with_table<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    TABLE.with(|t| f(&mut t.borrow_mut()))
}

/// Which of `std` a standard-channel constant selects.
fn std_index(kind: i32) -> Option<usize> {
    match kind {
        TCL_STDIN => Some(0),
        TCL_STDOUT => Some(1),
        TCL_STDERR => Some(2),
        _ => None,
    }
}

// ── the devices this crate opens itself ──────────────────────────────────

/// A file, which is `tclUnixChan.c`'s `fileChannelType` — the driver behind
/// every channel `open` returns for a path.
struct FileDevice {
    file: File,
}

impl Device for FileDevice {
    fn type_name(&self) -> &str {
        "file"
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.file.read(buf).map_err(|e| errno_message(&e))
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, String> {
        self.file.write(buf).map_err(|e| errno_message(&e))
    }

    fn seek(&mut self, offset: i64, whence: Whence) -> Result<i64, String> {
        let to = match whence {
            Whence::Start => SeekFrom::Start(offset.max(0) as u64),
            Whence::Current => SeekFrom::Current(offset),
            Whence::End => SeekFrom::End(offset),
        };
        self.file
            .seek(to)
            .map(|p| p as i64)
            .map_err(|e| errno_message(&e))
    }

    fn seekable(&self) -> bool {
        true
    }

    fn close(&mut self) -> Result<(), String> {
        self.file.flush().map_err(|e| errno_message(&e))
    }

    fn handle(&self, _direction: i32) -> Option<isize> {
        use std::os::fd::AsRawFd;
        Some(self.file.as_raw_fd() as isize)
    }
}

/// Which of the three the process was started with a [`StdDevice`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StdKind {
    In,
    Out,
    Err,
}

/// One of the process's own three descriptors, which is what
/// `TclpGetDefaultStdChannel` opens (`unix/tclUnixChan.c`).
///
/// The write side does *not* go to `std::io::stdout` here. A script's output
/// belongs to the interpreter that is running it — an [`Output::Capture`]
/// collects it, and `puts` without a channel already writes there — so the
/// standard output channel is handed the sink at the call and writes through
/// it. Writing to `std::io::stdout()` directly would make `puts stdout x`
/// invisible to every caller of [`crate::eval`] while `puts x` was captured.
struct StdDevice {
    kind: StdKind,
}

impl Device for StdDevice {
    fn type_name(&self) -> &str {
        "file"
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self.kind {
            StdKind::In => std::io::stdin().read(buf).map_err(|e| errno_message(&e)),
            _ => Err("bad file descriptor".to_string()),
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, String> {
        match self.kind {
            // Answered by [`write_through`] before the device is reached; a
            // write that arrives here is one whose sink was not available.
            StdKind::Out => Ok(buf.len()),
            StdKind::Err => {
                let mut err = std::io::stderr();
                err.write_all(buf).map_err(|e| errno_message(&e))?;
                let _ = err.flush();
                Ok(buf.len())
            }
            StdKind::In => Err("bad file descriptor".to_string()),
        }
    }

    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn handle(&self, _direction: i32) -> Option<isize> {
        Some(match self.kind {
            StdKind::In => 0,
            StdKind::Out => 1,
            StdKind::Err => 2,
        })
    }
}

/// An `io::Error` in the wording Tcl reports it, which is `Tcl_PosixError`'s —
/// the lowercased `strerror` text.
fn errno_message(e: &std::io::Error) -> String {
    let text = match e.raw_os_error() {
        Some(code) => unsafe {
            let p = libc::strerror(code);
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        },
        None => e.to_string(),
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => text,
    }
}

// ── creating, finding and closing ────────────────────────────────────────

/// `Tcl_CreateChannel` (`generic/tclIO.c:1594-1767`), less the parts that only
/// a stacked channel or a background flush needs.
///
/// Every initial value is that function's: `TCL_TRANSLATE_AUTO` on input and
/// the platform's on output (`:1682-1683`), the system encoding (`:1667-1668`),
/// a zero reference count (`:1687`) and `CHANNELBUFFER_DEFAULT_SIZE` (`:1698`).
pub fn create(name: &str, device: Box<dyn Device>, mode: i32) -> usize {
    with_table(|t| {
        let id = t.next_id;
        t.next_id += 1;
        t.names.insert(name.to_string(), id);
        t.channels.insert(
            id,
            Channel {
                name: name.to_string(),
                device,
                mode,
                ref_count: 0,
                input_translation: Translation::Auto,
                output_translation: Translation::Lf,
                encoding: Encoding::Utf8,
                buffering: Buffering::Full,
                buffer_size: DEFAULT_BUFFER_SIZE,
                blocking: true,
                raw: Vec::new(),
                pending: String::new(),
                device_eof: false,
                saw_cr: false,
                out: Vec::new(),
                handlers: Vec::new(),
            },
        );
        id
    })
}

/// `Tcl_RegisterChannel` (`generic/tclIO.c:1161-1198`) with a NULL interpreter:
/// the reference count goes up and nothing else happens.
pub fn register(id: usize) {
    with_table(|t| {
        if let Some(c) = t.channels.get_mut(&id) {
            c.ref_count += 1;
        }
    });
}

/// `Tcl_UnregisterChannel` (`generic/tclIO.c:1226-1283`): drop a reference and
/// close the channel once none is left.
pub fn unregister(id: usize) -> Result<(), String> {
    let should_close = with_table(|t| match t.channels.get_mut(&id) {
        Some(c) => {
            c.ref_count -= 1;
            c.ref_count <= 0
        }
        None => false,
    });
    if should_close {
        close_id(id)?;
    }
    Ok(())
}

/// The name a channel answers to, or `None` when the id names none.
pub fn name_of(id: usize) -> Option<String> {
    with_table(|t| t.channels.get(&id).map(|c| c.name.clone()))
}

/// `Tcl_GetChannelMode` (`generic/tclIO.c:2342-2360`).
pub fn mode_of(id: usize) -> i32 {
    with_table(|t| t.channels.get(&id).map_or(0, |c| c.mode))
}

/// `Tcl_GetChannelHandle` (`generic/tclIO.c:2397-2420`).
pub fn handle_of(id: usize, direction: i32) -> Option<isize> {
    with_table(|t| t.channels.get(&id).and_then(|c| c.device.handle(direction)))
}

/// `Tcl_SetStdChannel` (`generic/tclIO.c:719-745`). `None` clears the slot,
/// which is what a NULL channel does there.
pub fn set_std(kind: i32, id: Option<usize>) {
    let Some(i) = std_index(kind) else { return };
    with_table(|t| {
        t.std[i] = id;
        t.std_initialized[i] = true;
    });
}

/// `Tcl_GetStdChannel` (`generic/tclIO.c:763-830`), including its lazy
/// creation: the slot is filled from the process's own descriptor the first
/// time it is asked for, and the channel gets the artificial reference that
/// keeps it open until exit (`:782-793`).
pub fn std_channel(kind: i32) -> Option<usize> {
    let i = std_index(kind)?;
    if with_table(|t| t.std_initialized[i]) {
        return with_table(|t| t.std[i]);
    }
    let (name, device, mode, translation, buffering) = match kind {
        TCL_STDIN => (
            "stdin",
            StdKind::In,
            TCL_READABLE,
            Translation::Auto,
            Buffering::Full,
        ),
        TCL_STDOUT => (
            "stdout",
            StdKind::Out,
            TCL_WRITABLE,
            Translation::Lf,
            Buffering::Line,
        ),
        _ => (
            "stderr",
            StdKind::Err,
            TCL_WRITABLE,
            Translation::Lf,
            Buffering::None,
        ),
    };
    let id = create(name, Box::new(StdDevice { kind: device }), mode);
    with_table(|t| {
        if let Some(c) = t.channels.get_mut(&id) {
            // The measured defaults: `fconfigure stdin -translation` is `auto`,
            // stdout's is `lf`, stdout buffers by line and stderr not at all.
            if mode & TCL_READABLE != 0 {
                c.input_translation = translation;
            } else {
                c.output_translation = translation;
            }
            c.buffering = buffering;
        }
        t.std[i] = Some(id);
        t.std_initialized[i] = true;
    });
    register(id);
    Some(id)
}

/// A device that answers nothing, standing in for one that is out on loan.
///
/// See [`with_device_detached`]. It is never reachable from a script: the
/// channel it stands in for is put back before the borrow the caller could ask
/// through is available again.
struct Detached {
    id: usize,
}

impl Device for Detached {
    fn type_name(&self) -> &str {
        "detached"
    }

    /// A driver that reads re-entrantly would land here. None does — Tk's
    /// console `inputProc` returns end of file and nothing else
    /// (`tk9.0.4/generic/tkConsole.c:559-568`) — so this is a named failure
    /// rather than a guess at what the device would have said.
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
        Err("the channel's device is servicing an earlier call".to_string())
    }

    /// A re-entrant write goes back into the channel's own output buffer and
    /// leaves on the next flush. That is the one thing it can do that is both
    /// correct and terminating: writing through would need the device, which is
    /// the call already in progress.
    fn write(&mut self, buf: &[u8]) -> Result<usize, String> {
        let id = self.id;
        with_table(|t| {
            if let Some(c) = t.channels.get_mut(&id) {
                c.out.extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Run `f` with the channel's device taken *out* of the table.
///
/// A driver proc may call back into the channel layer. Tk's console
/// `outputProc` does exactly that: it builds a `tk::ConsoleOutput` command and
/// evaluates it (`tk9.0.4/generic/tkConsole.c:520-536`), and that script can
/// `puts` again. Holding the table's borrow across the call would turn that
/// into a panic, so the device is lent out and put back.
fn with_device_detached<T>(id: usize, f: impl FnOnce(&mut dyn Device) -> T) -> Option<T> {
    let mut device = with_table(|t| {
        t.channels
            .get_mut(&id)
            .map(|c| std::mem::replace(&mut c.device, Box::new(Detached { id })))
    })?;
    let out = f(device.as_mut());
    with_table(|t| {
        if let Some(c) = t.channels.get_mut(&id) {
            c.device = device;
        }
    });
    Some(out)
}

/// `Tcl_GetChannel` (`generic/tclIO.c:1424-1484`): a name to a channel, with
/// `stdin`, `stdout` and `stderr` resolved through the standard-channel slots
/// first (`:1447-1460`).
pub fn lookup(name: &str) -> Option<usize> {
    if let Some(kind) = match name {
        "stdin" => Some(TCL_STDIN),
        "stdout" => Some(TCL_STDOUT),
        "stderr" => Some(TCL_STDERR),
        _ => None,
    } {
        if let Some(id) = std_channel(kind) {
            return Some(id);
        }
    }
    with_table(|t| t.names.get(name).copied())
}

/// The error `Tcl_GetChannel` leaves when the name is not a channel
/// (`generic/tclIO.c:1465-1466`).
fn resolve(name: &str) -> Result<usize, String> {
    lookup(name).ok_or_else(|| format!("can not find channel named \"{name}\""))
}

/// Flush what is buffered, close the device and forget the channel — the tail
/// of `Tcl_Close` (`generic/tclIO.c`'s `CloseChannel`).
fn close_id(id: usize) -> Result<(), String> {
    let flushed = flush_id(id, None);
    // Taken out of the table *before* the driver is called, for the reason
    // [`with_device_detached`] gives: a `close2Proc` may reach back in.
    let device = with_table(|t| {
        t.channels.remove(&id).map(|c| {
            t.names.remove(&c.name);
            for slot in t.std.iter_mut() {
                if *slot == Some(id) {
                    *slot = None;
                }
            }
            c.device
        })
    });
    let outcome = match device {
        Some(mut d) => d.close(),
        None => Ok(()),
    };
    flushed.and(outcome)
}

// ── reading ──────────────────────────────────────────────────────────────

/// One pass of the read pipeline: bytes from the device, decoded through the
/// channel's encoding and translated through its input rule.
///
/// Returns false once the device has answered with zero bytes, which is the
/// only end-of-file signal a driver has (`generic/tclIO.c`'s `ChanRead`).
fn fill(c: &mut Channel) -> Result<bool, String> {
    if c.device_eof {
        return Ok(false);
    }
    let mut buf = vec![0u8; c.buffer_size.clamp(1, 1 << 20) as usize];
    let n = c.device.read(&mut buf)?;
    if n == 0 {
        c.device_eof = true;
        // A trailing carriage return under `auto` is a line ending of its own
        // once nothing can follow it.
        c.saw_cr = false;
        // Undecodable bytes at the very end are not a straddling character
        // after all; they are what they are.
        if !c.raw.is_empty() {
            let tail = std::mem::take(&mut c.raw);
            let text = String::from_utf8_lossy(&tail).into_owned();
            translate_in(c, &text);
        }
        return Ok(false);
    }
    c.raw.extend_from_slice(&buf[..n]);
    decode(c)?;
    Ok(true)
}

/// Move every complete character out of `raw` and through the translation.
fn decode(c: &mut Channel) -> Result<(), String> {
    let text = match c.encoding {
        // Anything with a table behind it, decoded by the module that owns the
        // tables. What is left in `raw` is the start of a character whose rest
        // has not arrived.
        Encoding::Named(name) => {
            let (text, used) = crate::cmd_encoding::stream_decode(name, &c.raw)
                .map_err(|e| format!("error reading \"{}\": {e}", c.name))?;
            c.raw.drain(..used);
            text
        }
        Encoding::Iso8859_1 => {
            let s: String = c.raw.iter().map(|b| *b as char).collect();
            c.raw.clear();
            s
        }
        Encoding::Utf8 => {
            let taken = std::mem::take(&mut c.raw);
            match String::from_utf8(taken) {
                Ok(s) => s,
                Err(e) => {
                    let valid = e.utf8_error().valid_up_to();
                    let bytes = e.into_bytes();
                    // A sequence cut by the end of the buffer waits for the
                    // rest; anything else is decoded as replacement, which is
                    // what Tcl's `tcl8` profile does with a bad byte.
                    let (good, rest) = bytes.split_at(valid);
                    let s = String::from_utf8_lossy(good).into_owned();
                    c.raw = rest.to_vec();
                    s
                }
            }
        }
    };
    translate_in(c, &text);
    Ok(())
}

/// Apply the channel's input translation to freshly decoded text.
///
/// `auto` accepts `\n`, `\r` and `\r\n` and answers `\n` for each
/// (`generic/tclIO.c:1675-1677`); the `saw_cr` flag is `INPUT_SAW_CR`, and it
/// exists because a `\r\n` may straddle two reads.
fn translate_in(c: &mut Channel, text: &str) {
    match c.input_translation {
        Translation::Lf => c.pending.push_str(text),
        Translation::Cr => {
            for ch in text.chars() {
                c.pending.push(if ch == '\r' { '\n' } else { ch });
            }
        }
        Translation::Crlf => {
            for ch in text.chars() {
                if c.saw_cr {
                    c.saw_cr = false;
                    if ch == '\n' {
                        c.pending.push('\n');
                        continue;
                    }
                    c.pending.push('\r');
                }
                if ch == '\r' {
                    c.saw_cr = true;
                } else {
                    c.pending.push(ch);
                }
            }
        }
        Translation::Auto => {
            for ch in text.chars() {
                if c.saw_cr {
                    c.saw_cr = false;
                    // The `\n` of a `\r\n` was already answered by the `\r`.
                    if ch == '\n' {
                        continue;
                    }
                }
                if ch == '\r' {
                    c.saw_cr = true;
                    c.pending.push('\n');
                } else {
                    c.pending.push(ch);
                }
            }
        }
    }
}

/// `gets` (`generic/tclIO.c:4601`'s `Tcl_GetsObj`): the next line without its
/// terminator, or `None` at end of file.
///
/// Every translation has already turned its line ending into `\n` by the time
/// the text reaches [`Channel::pending`], so the scan here is for that one
/// character — which is what `Tcl_GetsObj` does too once `TranslateInputEOL`
/// has run.
fn gets_id(id: usize) -> Result<Option<String>, String> {
    with_channel(id, |c| loop {
        if let Some(at) = c.pending.find('\n') {
            let line = c.pending[..at].to_string();
            c.pending.drain(..=at);
            return Ok(Some(line));
        }
        if !fill(c)? {
            if c.pending.is_empty() {
                return Ok(None);
            }
            return Ok(Some(std::mem::take(&mut c.pending)));
        }
    })
}

/// `read` (`generic/tclIO.c:5877`'s `Tcl_ReadChars`): `count` characters, or
/// everything up to end of file when `count` is `None`.
fn read_id(id: usize, count: Option<i64>) -> Result<String, String> {
    with_channel(id, |c| match count {
        None => {
            while fill(c)? {}
            Ok(std::mem::take(&mut c.pending))
        }
        Some(n) if n <= 0 => Ok(String::new()),
        Some(n) => {
            let n = n as usize;
            while c.pending.chars().count() < n {
                if !fill(c)? {
                    break;
                }
            }
            let end = c
                .pending
                .char_indices()
                .nth(n)
                .map_or(c.pending.len(), |(i, _)| i);
            let taken = c.pending[..end].to_string();
            c.pending.drain(..end);
            Ok(taken)
        }
    })
}

/// `Tcl_Eof` (`generic/tclIO.c:7604`): whether the channel has reached end of
/// file, which needs both a device that said so and nothing left buffered.
fn eof_id(id: usize) -> Result<bool, String> {
    with_channel(id, |c| Ok(c.device_eof && c.pending.is_empty()))
}

// ── writing ──────────────────────────────────────────────────────────────

/// Apply the channel's output translation, which is the inverse of
/// [`translate_in`]: every `\n` becomes whatever the mode spells a line ending
/// with (`generic/tclIO.c`'s `TranslateOutputEOL`).
fn translate_out(t: Translation, text: &str) -> String {
    match t {
        // `auto` is never an output mode: `Tcl_SetChannelOption` maps it to the
        // platform's, which is `lf` here (`generic/tclIO.c:8427-8438`).
        Translation::Lf | Translation::Auto => text.to_string(),
        Translation::Cr => text.replace('\n', "\r"),
        Translation::Crlf => text.replace('\n', "\r\n"),
    }
}

/// Encode translated text for the device.
fn encode(e: Encoding, text: &str) -> Result<Vec<u8>, String> {
    Ok(match e {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        // Encoded by the module that owns the tables, under a channel's own
        // profile — `strict`, which is what tclsh reports for a fresh channel
        // and what this layer reports for the option it refuses to set. So a
        // character the encoding cannot hold is an error rather than a
        // substitution, as it is in tclsh.
        Encoding::Named(name) => crate::cmd_encoding::stream_encode(name, text)?,
        // One byte per character, with anything outside Latin-1 replaced —
        // Tcl's `tcl8` profile answers `?` for an unrepresentable character.
        Encoding::Iso8859_1 => text
            .chars()
            .map(|ch| if (ch as u32) < 0x100 { ch as u8 } else { b'?' })
            .collect(),
    })
}

/// `Tcl_WriteChars` (`generic/tclIO.c:4171-4218`), plus the buffering decision
/// `WriteChars` makes: `none` reaches the device at once, `line` when the text
/// completes a line, `full` when the buffer passes `-buffersize`.
///
/// `sink` is the running interpreter's output, and is used for the standard
/// output channel only — see [`StdDevice`].
fn write_id(id: usize, text: &str, sink: Option<&Output>) -> Result<(), String> {
    let to_sink = with_table(|t| {
        t.channels
            .get(&id)
            .is_some_and(|c| c.name == "stdout" && c.device.type_name() == "file")
    });
    if to_sink {
        if let Some(out) = sink {
            let translated = with_channel(id, |c| Ok(translate_out(c.output_translation, text)))?;
            out.write(&translated);
            return Ok(());
        }
    }
    let ready = with_channel(id, |c| {
        let translated = translate_out(c.output_translation, text);
        let name = c.name.clone();
        c.out.extend_from_slice(
            &encode(c.encoding, &translated)
                .map_err(|e| format!("error writing \"{name}\": {e}"))?,
        );
        Ok(match c.buffering {
            Buffering::None => true,
            Buffering::Line => translated.contains('\n'),
            Buffering::Full => c.out.len() as i64 >= c.buffer_size,
        })
    })?;
    if ready {
        flush_id(id, sink)?;
    }
    Ok(())
}

/// `Tcl_Flush` (`generic/tclIO.c:6920`): hand everything buffered to the device.
fn flush_id(id: usize, sink: Option<&Output>) -> Result<(), String> {
    let pending = with_table(|t| {
        t.channels
            .get_mut(&id)
            .map(|c| std::mem::take(&mut c.out))
            .unwrap_or_default()
    });
    if pending.is_empty() {
        // The standard output channel writes straight through to the sink, so
        // a flush of it is a flush of that sink.
        if let (Some(out), true) = (sink, name_of(id).as_deref() == Some("stdout")) {
            out.flush();
        }
        return Ok(());
    }
    with_device_detached(id, |device| {
        let mut at = 0;
        while at < pending.len() {
            match device.write(&pending[at..]) {
                Ok(0) => return Err("channel is not writable".to_string()),
                Ok(n) => at += n,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })
    .unwrap_or(Ok(()))
}

// ── options ──────────────────────────────────────────────────────────────

/// Run `f` against a channel by id, refusing an id that names none.
fn with_channel<T>(
    id: usize,
    f: impl FnOnce(&mut Channel) -> Result<T, String>,
) -> Result<T, String> {
    with_table(|t| match t.channels.get_mut(&id) {
        Some(c) => f(c),
        None => Err("channel is not open".to_string()),
    })
}

/// The options `fconfigure` with no option name reports, in the order
/// `Tcl_GetChannelOption` builds them (`generic/tclIO.c:7966-8100`) — which is
/// the order tclsh prints, `-blocking` first and `-translation` last.
const GENERIC_OPTIONS: &[&str] = &[
    "-blocking",
    "-buffering",
    "-buffersize",
    "-encoding",
    "-eofchar",
    "-profile",
    "-translation",
];

/// `Tcl_GetChannelOption` (`generic/tclIO.c:7966`) for the generic options.
pub fn get_option(id: usize, option: &str) -> Result<String, String> {
    with_channel(id, |c| match option {
        "-blocking" => Ok(if c.blocking { "1" } else { "0" }.to_string()),
        "-buffering" => Ok(c.buffering.name().to_string()),
        "-buffersize" => Ok(c.buffer_size.to_string()),
        "-encoding" => Ok(c.encoding.name().to_string()),
        // Reported at the value `Tcl_CreateChannel` sets and never changed,
        // because setting it is refused below (`generic/tclIO.c:1684`).
        "-eofchar" => Ok(String::new()),
        "-profile" => Ok("strict".to_string()),
        "-translation" => Ok(match (c.readable(), c.writable()) {
            // A read-write channel reports both halves as a two-element list
            // (`generic/tclIO.c`'s `-translation` arm).
            (true, true) => format!(
                "{} {}",
                c.input_translation.name(),
                c.output_translation.name()
            ),
            (true, false) => c.input_translation.name().to_string(),
            _ => c.output_translation.name().to_string(),
        }),
        other => Err(bad_option(other)),
    })
}

/// The wording `Tcl_BadChannelOption` produces for a name no option matches
/// (`generic/tclIO.c:7876-7912`), with the driver's own options appended —
/// which for a file is `-stat`.
fn bad_option(name: &str) -> String {
    format!(
        "bad option \"{name}\": should be one of -blocking, -buffering, \
         -buffersize, -encoding, -eofchar, -profile, -translation, or -stat"
    )
}

/// `Tcl_SetChannelOption` (`generic/tclIO.c:8179-8471`) for the generic
/// options, refusing by name the ones whose machinery is not here.
pub fn set_option(id: usize, option: &str, value: &str) -> Result<(), String> {
    match option {
        "-translation" => set_translation(id, value),
        "-encoding" => {
            let encoding = match value {
                "utf-8" | "utf8" => Encoding::Utf8,
                "iso8859-1" | "iso-8859-1" | "latin1" => Encoding::Iso8859_1,
                "" | "binary" => {
                    return Err(format!(
                        "unknown encoding \"{value}\": No longer supported.\n\
                         \tplease use either \"-translation binary\" or \
                         \"-encoding iso8859-1\""
                    ))
                }
                // Every other name `encoding names` offers, which is the
                // set `crate::cmd_encoding` has tables or a ported conversion
                // proc for. A name it does not offer is refused with the same
                // message `encoding convertfrom` would give it.
                other => match crate::cmd_encoding::static_name(other) {
                    Some(name) => Encoding::Named(name),
                    None => return Err(format!("unknown encoding \"{other}\"")),
                },
            };
            with_channel(id, |c| {
                c.encoding = encoding;
                Ok(())
            })
        }
        "-buffering" => {
            let buffering = match value {
                "full" => Buffering::Full,
                "line" => Buffering::Line,
                "none" => Buffering::None,
                _ => {
                    return Err(
                        "bad value for -buffering: must be one of full, line, or none".to_string(),
                    )
                }
            };
            with_channel(id, |c| {
                c.buffering = buffering;
                Ok(())
            })
        }
        "-buffersize" => {
            let size: i64 = value
                .parse()
                .map_err(|_| format!("expected integer but got \"{value}\""))?;
            with_channel(id, |c| {
                c.buffer_size = size.clamp(1, 1 << 20);
                Ok(())
            })
        }
        "-blocking" => match value {
            "1" | "yes" | "true" | "on" => with_channel(id, |c| {
                c.blocking = true;
                Ok(())
            }),
            "0" | "no" | "false" | "off" => Err(
                "non-blocking channels are not implemented in this frontend; \
                 -blocking 0 is refused rather than accepted and ignored"
                    .to_string(),
            ),
            other => Err(format!("expected boolean value but got \"{other}\"")),
        },
        "-eofchar" | "-profile" => Err(format!(
            "{option} is not implemented in this frontend; it is reported at its \
             default and refused when set"
        )),
        other => Err(bad_option(other)),
    }
}

/// The `-translation` arm of `Tcl_SetChannelOption`
/// (`generic/tclIO.c:8359-8462`), including `binary`, which is not a
/// translation at all but a translation *and* an encoding
/// (`:8389-8393`, `:8439-8442`).
fn set_translation(id: usize, value: &str) -> Result<(), String> {
    let parts = crate::list::split(value)
        .map_err(|_| "bad value for -translation: must be a one or two element list".to_string())?;
    let (read_mode, write_mode) = match parts.len() {
        1 => (parts[0].as_str(), parts[0].as_str()),
        2 => (parts[0].as_str(), parts[1].as_str()),
        _ => {
            return Err("bad value for -translation: must be a one or two element list".to_string())
        }
    };
    let readable = with_channel(id, |c| Ok(c.readable()))?;
    let writable = with_channel(id, |c| Ok(c.writable()))?;

    if readable {
        let (translation, binary) = parse_translation(read_mode, Translation::Auto)?;
        with_channel(id, |c| {
            c.input_translation = translation;
            if binary {
                c.encoding = Encoding::Iso8859_1;
            }
            Ok(())
        })?;
    }
    if writable {
        let (translation, binary) = parse_translation(write_mode, Translation::Lf)?;
        with_channel(id, |c| {
            c.output_translation = translation;
            if binary {
                c.encoding = Encoding::Iso8859_1;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// One half of a `-translation` value. The second element of the pair is
/// whether the word was `binary`, which also changes the encoding.
fn parse_translation(word: &str, auto: Translation) -> Result<(Translation, bool), String> {
    Ok(match word {
        "auto" => (auto, false),
        "binary" => (Translation::Lf, true),
        "lf" => (Translation::Lf, false),
        "cr" => (Translation::Cr, false),
        "crlf" => (Translation::Crlf, false),
        "platform" => (Translation::Lf, false),
        _ => {
            return Err(
                "bad value for -translation: must be one of auto, binary, cr, lf, \
                 crlf, or platform"
                    .to_string(),
            )
        }
    })
}

// ── what the C side reaches this layer through ───────────────────────────
//
// Everything below is the same generic layer the commands above use, addressed
// by channel id instead of by name — which is the shape `generic/tclIO.c`'s own
// public functions have, since a `Tcl_Channel` is a handle and not a name.

/// `Tcl_CreateChannel`'s tail (`generic/tclIO.c:1751-1765`): a channel created
/// while a standard slot is empty *and* has been consulted takes that slot,
/// under that slot's name.
pub fn adopt_empty_std_slot(id: usize) {
    let taken = with_table(|t| {
        for (i, name) in ["stdin", "stdout", "stderr"].iter().enumerate() {
            if t.std[i].is_none() && t.std_initialized[i] {
                if let Some(c) = t.channels.get_mut(&id) {
                    t.names.remove(&c.name);
                    c.name = (*name).to_string();
                    t.names.insert((*name).to_string(), id);
                }
                t.std[i] = Some(id);
                return true;
            }
        }
        false
    });
    if taken {
        register(id);
    }
}

/// `Tcl_WriteChars` / `Tcl_Write` / `Tcl_WriteObj` for a caller that has bytes
/// rather than a Tcl string, and no interpreter sink to write a standard
/// channel through.
pub fn write_bytes(id: usize, bytes: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    write_id(id, &text, None)
}

/// `Tcl_ReadChars` (`generic/tclIO.c:5877`).
pub fn read_chars(id: usize, count: Option<i64>) -> Result<String, String> {
    read_id(id, count)
}

/// `Tcl_GetsObj` (`generic/tclIO.c:4601`).
pub fn gets(id: usize) -> Result<Option<String>, String> {
    gets_id(id)
}

/// `Tcl_Flush` (`generic/tclIO.c:6920`).
pub fn flush(id: usize) -> Result<(), String> {
    flush_id(id, None)
}

/// `Tcl_Close` (`generic/tclIO.c`) by id.
pub fn close(id: usize) -> Result<(), String> {
    close_id(id)
}

/// `Tcl_CloseEx` with `TCL_CLOSE_READ` or `TCL_CLOSE_WRITE`
/// (`generic/tcl.h:1369-1370`): drop one side and keep the channel, or close it
/// outright once no side is left.
pub fn half_close(id: usize, flags: i32) -> Result<(), String> {
    // The two close flags have the same bit values as the two mode flags,
    // which is what lets one mask serve both (`generic/tcl.h:1349-1350`,
    // `:1369-1370`).
    let dropping = flags & (TCL_READABLE | TCL_WRITABLE);
    let left = with_channel(id, |c| {
        c.mode &= !dropping;
        Ok(c.mode & (TCL_READABLE | TCL_WRITABLE))
    })?;
    if left == 0 {
        return close_id(id);
    }
    Ok(())
}

/// `Tcl_Seek` (`generic/tclIO.c:7161`): the new position.
pub fn seek(id: usize, offset: i64, whence: Whence) -> Result<i64, String> {
    seek_at(id, offset, whence)
}

/// `Tcl_Tell` (`generic/tclIO.c:7330`).
pub fn tell(id: usize) -> Result<i64, String> {
    with_channel(id, |c| {
        if !c.device.seekable() {
            return Ok(-1);
        }
        let at = c.device.seek(0, Whence::Current)?;
        Ok(at - c.pending.chars().count() as i64 - c.raw.len() as i64)
    })
}

/// `Tcl_Eof` (`generic/tclIO.c:7604`).
pub fn at_eof(id: usize) -> bool {
    eof_id(id).unwrap_or(false)
}

/// `Tcl_InputBuffered` (`generic/tclIO.c`).
pub fn input_buffered(id: usize) -> usize {
    with_channel(id, |c| Ok(c.pending.len() + c.raw.len())).unwrap_or(0)
}

/// `Tcl_ChannelBuffered` (`generic/tclIO.c`): the output side of the same
/// question.
pub fn output_buffered(id: usize) -> usize {
    with_channel(id, |c| Ok(c.out.len())).unwrap_or(0)
}

/// `statePtr->refCount` (`generic/tclIO.c:1687`).
pub fn ref_count(id: usize) -> isize {
    with_table(|t| t.channels.get(&id).map_or(0, |c| c.ref_count))
}

/// `Tcl_GetChannelBufferSize` (`generic/tclIO.c`).
pub fn buffer_size(id: usize) -> i64 {
    with_channel(id, |c| Ok(c.buffer_size)).unwrap_or(DEFAULT_BUFFER_SIZE)
}

/// `Tcl_SetChannelBufferSize` (`generic/tclIO.c`).
pub fn set_buffer_size(id: usize, size: i64) {
    let _ = with_channel(id, |c| {
        c.buffer_size = size.clamp(1, 1 << 20);
        Ok(())
    });
}

/// `Tcl_SetChannelOption`'s generic arms (`generic/tclIO.c:8225-8462`). An
/// option no arm matches comes back as an error, and the caller then offers it
/// to the driver as the C does (`:8463-8465`).
pub fn set_channel_option(id: usize, option: &str, value: &str) -> Result<(), String> {
    match set_option(id, option, value) {
        Err(e) if e.starts_with("bad option ") => {
            match with_channel(id, |c| Ok(c.device.set_option(option, value)))? {
                Some(answer) => answer,
                None => Err(e),
            }
        }
        other => other,
    }
}

/// `Tcl_GetChannelOption` (`generic/tclIO.c:7966`), with the same fall-through
/// to the driver.
pub fn get_channel_option(id: usize, option: &str) -> Result<String, String> {
    match get_option(id, option) {
        Err(e) if e.starts_with("bad option ") => {
            match with_channel(id, |c| Ok(c.device.get_option(option)))? {
                Some(answer) => answer,
                None => Err(e),
            }
        }
        other => other,
    }
}

/// Every generic option as a name/value list, which is what
/// `Tcl_GetChannelOption` answers for a NULL option name
/// (`generic/tclIO.c:7990-7995`).
pub fn all_options(id: usize) -> Result<String, String> {
    let mut parts = Vec::with_capacity(GENERIC_OPTIONS.len() * 2);
    for option in GENERIC_OPTIONS {
        parts.push((*option).to_string());
        parts.push(get_option(id, option)?);
    }
    Ok(crate::list::join(&parts))
}

/// `Tcl_GetChannelType` (`generic/tclIO.c:2315`): the driver table's address.
pub fn driver_table(id: usize) -> Option<usize> {
    with_channel(id, |c| Ok(c.device.driver_table())).unwrap_or(None)
}

/// `Tcl_GetChannelInstanceData` (`generic/tclIO.c:2262`).
pub fn instance_data(id: usize) -> Option<usize> {
    with_channel(id, |c| Ok(c.device.instance_data())).unwrap_or(None)
}

/// `Tcl_OpenFileChannel` (`generic/tclIOUtil.c:345`): the channel's name.
pub fn open_file(path: &str, access: &str) -> Result<String, String> {
    open(path, Some(access))
}

// ── the channel-handler slots ────────────────────────────────────────────

/// `Tcl_CreateChannelHandler` (`generic/tclIO.c:8874-8949`): remember the
/// handler, then tell the driver the union of every handler's interest through
/// its `watchProc`.
pub fn create_channel_handler(id: usize, mask: i32, proc: usize, client_data: usize) {
    let interest = with_table(|t| {
        let Some(c) = t.channels.get_mut(&id) else {
            return 0;
        };
        c.handlers
            .retain(|h| !(h.proc == proc && h.client_data == client_data));
        if mask != 0 {
            c.handlers.push(ChannelHandler {
                mask,
                proc,
                client_data,
            });
        }
        c.handlers.iter().fold(0, |acc, h| acc | h.mask)
    });
    with_device_detached(id, |device| device.watch(interest));
}

/// `Tcl_DeleteChannelHandler` (`generic/tclIO.c:8951-9020`).
pub fn delete_channel_handler(id: usize, proc: usize, client_data: usize) {
    create_channel_handler(id, 0, proc, client_data);
}

/// Every handler registered for `id` whose mask overlaps `mask` — what
/// `Tcl_NotifyChannel` walks (`generic/tclIO.c`).
pub fn handlers_for(id: usize, mask: i32) -> Vec<(usize, usize, i32)> {
    with_table(|t| {
        t.channels
            .get(&id)
            .map(|c| {
                c.handlers
                    .iter()
                    .filter(|h| h.mask & mask != 0)
                    .map(|h| (h.proc, h.client_data, h.mask & mask))
                    .collect()
            })
            .unwrap_or_default()
    })
}

// ── compiling ────────────────────────────────────────────────────────────

/// The names `compile` accepts. `puts` is not here: the compiler owns it, and
/// only its channel forms reach this module.
pub const COMMANDS: &[&str] = &[
    "open",
    "close",
    "gets",
    "read",
    "flush",
    "eof",
    "seek",
    "tell",
    "fconfigure",
];

/// Lower one of the channel commands. Every argument is an ordinary word, so
/// nothing about a channel is resolved while compiling.
pub(crate) fn compile(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    let (id, usage, min, max) = match name {
        "open" => (ext::OPEN, "open fileName ?access? ?permissions?", 1, 3),
        "close" => (ext::CLOSE, "close channel ?direction?", 1, 2),
        "read" => (
            ext::READ,
            "read channel ?numChars?\" or \"read ?-nonewline? channel",
            1,
            2,
        ),
        "flush" => (ext::FLUSH, "flush channel", 1, 1),
        "eof" => (ext::EOF, "eof channel", 1, 1),
        "seek" => (ext::SEEK, "seek channel offset ?origin?", 2, 3),
        "tell" => (ext::TELL, "tell channel", 1, 1),
        "fconfigure" => (
            ext::FCONFIGURE,
            "fconfigure channel ?-option value ...?",
            1,
            usize::MAX,
        ),
        "gets" => return compile_gets(c, args),
        other => return c.error(format!("invalid command name \"{other}\"")),
    };
    if args.len() < min || args.len() > max {
        return c.error(format!("wrong # args: should be \"{usage}\""));
    }
    for arg in args {
        c.word(arg)?;
    }
    let argc = u8::try_from(args.len()).map_err(|_| c.err("too many arguments for one command"))?;
    c.emit(Op::Extended(id, argc), 1 - args.len() as i32);
    Ok(())
}

/// `gets channel ?varName?`. The variable travels as where it lives, the way
/// `regexp`'s match variables do, because the op assigns to it and yields the
/// character count instead of the line.
fn compile_gets(c: &mut Compiler, args: &[Word]) -> Result<(), CompileError> {
    let (channel, var) = match args {
        [channel] => (channel, None),
        [channel, var] => (channel, Some(var)),
        _ => return c.error("wrong # args: should be \"gets channel ?varName?\""),
    };
    c.word(channel)?;
    let operands = match var {
        None => 1,
        Some(word) => {
            let name = c.var_name_of(word)?;
            let encoded = c.place_operand(&name);
            c.emit(Op::LoadInt(encoded), 1);
            2
        }
    };
    c.emit(Op::Extended(ext::GETS, operands), 1 - operands as i32);
    Ok(())
}

/// `puts ?-nonewline? channel string`, lowered by the compiler's own `puts`
/// once it has seen a channel argument.
pub(crate) fn compile_puts(
    c: &mut Compiler,
    channel: &Word,
    value: &Word,
    newline: bool,
) -> Result<(), CompileError> {
    c.word(channel)?;
    c.word(value)?;
    c.emit(Op::Extended(ext::CH_PUTS, u8::from(newline)), -1);
    Ok(())
}

// ── running ──────────────────────────────────────────────────────────────

/// The channel ops. `sink` is the running interpreter's output, which the
/// standard output channel writes through.
pub(crate) fn run(vm: &mut VM, id: u16, arg: u8, sink: &Output) -> Result<(), String> {
    // Every op but one takes its stack depth from the inline operand.
    // [`ext::CH_PUTS`] spends that operand on `-nonewline` instead, and always
    // has the channel and the string below it.
    let count = if id == ext::CH_PUTS { 2 } else { arg as usize };
    let mut operands = Vec::with_capacity(count);
    for _ in 0..count {
        operands.push(vm.pop());
    }
    operands.reverse();
    let text = |i: usize| to_tcl_string(operands.get(i).unwrap_or(&Value::Undef));

    let result = match id {
        ext::OPEN => Value::Str(Arc::new(open(
            &text(0),
            operands.get(1).map(|_| text(1)).as_deref(),
        )?)),
        ext::CLOSE => {
            close_command(&text(0), operands.get(1).map(|_| text(1)).as_deref())?;
            empty()
        }
        ext::GETS => {
            let channel = resolve_readable(&text(0))?;
            let line = gets_id(channel)?;
            match operands.get(1) {
                None => Value::Str(Arc::new(line.unwrap_or_default())),
                Some(place) => {
                    let value = line.clone().unwrap_or_default();
                    assign(vm, place, &value)?;
                    Value::Int(match line {
                        Some(l) => l.chars().count() as i64,
                        None => -1,
                    })
                }
            }
        }
        ext::READ => read_command(operands.len(), &text)?,
        ext::CH_PUTS => {
            let channel = resolve_writable(&text(0))?;
            let mut out = text(1);
            if arg == 1 {
                out.push('\n');
            }
            write_id(channel, &out, Some(sink))?;
            empty()
        }
        ext::FLUSH => {
            let channel = resolve_writable(&text(0))?;
            flush_id(channel, Some(sink))?;
            empty()
        }
        ext::EOF => Value::Int(i64::from(eof_id(resolve(&text(0))?)?)),
        ext::SEEK => {
            seek_command(&text(0), &text(1), operands.get(2).map(|_| text(2)))?;
            empty()
        }
        ext::TELL => Value::Int(tell_command(&text(0))?),
        ext::FCONFIGURE => {
            let rest: Vec<String> = (1..operands.len()).map(text).collect();
            Value::Str(Arc::new(fconfigure(&text(0), &rest)?))
        }
        other => return Err(format!("unknown channel op {other}")),
    };
    vm.push(result);
    Ok(())
}

/// `puts`'s own empty result.
fn empty() -> Value {
    Value::Str(Arc::new(String::new()))
}

/// Store a line in the variable an encoded place operand names, as
/// [`crate::regexp`]'s match variables are stored.
fn assign(vm: &mut VM, encoded: &Value, value: &str) -> Result<(), String> {
    let raw = match encoded {
        Value::Int(v) => *v,
        other => return Err(format!("gets: not a variable place: {other:?}")),
    };
    let place = place_at(&Value::Int(raw >> 1), raw & 1 == 1)?;
    if let Some(cell) = var_cell(vm, place) {
        *cell = Value::Str(Arc::new(value.to_string()));
    }
    Ok(())
}

/// A channel that must be readable, in `Tcl_ReadChars`'s wording for one that
/// is not (`generic/tclIO.c`'s `CheckChannelErrors`).
fn resolve_readable(name: &str) -> Result<usize, String> {
    let id = resolve(name)?;
    if !with_channel(id, |c| Ok(c.readable()))? {
        return Err(format!("channel \"{name}\" wasn't opened for reading"));
    }
    Ok(id)
}

/// The same for the write side.
fn resolve_writable(name: &str) -> Result<usize, String> {
    let id = resolve(name)?;
    if !with_channel(id, |c| Ok(c.writable()))? {
        return Err(format!("channel \"{name}\" wasn't opened for writing"));
    }
    Ok(id)
}

/// `open fileName ?access?`, less the pipe form.
///
/// The channel's name is `file` followed by the descriptor number, which is
/// what `TclpOpenFileChannel` builds it from
/// (`unix/tclUnixChan.c:1845`, `:1859`) — so a script that prints a channel
/// name prints the same name tclsh does.
fn open(path: &str, access: Option<&str>) -> Result<String, String> {
    if path.starts_with('|') {
        return Err(
            "opening a command pipeline is not implemented in this frontend; \
             open refuses it rather than opening a file named \"|…\""
                .to_string(),
        );
    }
    let access = access.unwrap_or("r");
    let mut options = std::fs::OpenOptions::new();
    // The POSIX access strings of `Tcl_OpenFileChannel`'s `modeString`
    // (`generic/tclIOUtil.c:345`, and `TclGetOpenMode`).
    match access {
        "r" => options.read(true),
        "r+" => options.read(true).write(true),
        "w" => options.write(true).create(true).truncate(true),
        "w+" => options.read(true).write(true).create(true).truncate(true),
        "a" => options.append(true).create(true),
        // `a+` is `O_RDWR|O_CREAT` with `O_APPEND` *removed*, so that `seek`
        // works on it (`generic/tclIOUtil.c:1494-1501`, "Bug 1773127"). The
        // seek-to-end below is what still puts it at the end of the file.
        "a+" => options.read(true).write(true).create(true),
        other => {
            // The list form — `{WRONLY CREAT TRUNC}` — is a second grammar for
            // the same thing, and is refused by name rather than guessed at.
            if other.contains(char::is_whitespace)
                || other.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            {
                return Err(format!(
                    "the POSIX list form of an access mode ({other}) is not \
                     implemented in this frontend; the r/w/a strings are"
                ));
            }
            return Err(format!("illegal access mode \"{other}\""));
        }
    };
    let mut file = options
        .open(path)
        .map_err(|e| format!("couldn't open \"{path}\": {}", errno_message(&e)))?;
    // Both append modes set `modeFlags & 1`, and the caller seeks to the end
    // once the channel exists (`generic/tclIOUtil.c:2232`). Without it `tell`
    // on a freshly opened `a` channel answers 0 where tclsh answers the file's
    // size.
    if access.starts_with('a') {
        file.seek(SeekFrom::End(0)).map_err(|e| errno_message(&e))?;
    }
    let mode = match access {
        "r" => TCL_READABLE,
        "w" | "a" => TCL_WRITABLE,
        _ => TCL_READABLE | TCL_WRITABLE,
    };
    let name = {
        use std::os::fd::AsRawFd;
        format!("file{}", file.as_raw_fd())
    };
    let id = create(&name, Box::new(FileDevice { file }), mode);
    register(id);
    Ok(name)
}

/// `close channel ?direction?`.
fn close_command(name: &str, direction: Option<&str>) -> Result<(), String> {
    let id = resolve(name)?;
    if let Some(direction) = direction {
        let (want, side) = match direction {
            "read" | "r" => (TCL_READABLE, "read"),
            "write" | "w" => (TCL_WRITABLE, "write"),
            other => return Err(format!("bad direction \"{other}\": must be read or write")),
        };
        let mode = mode_of(id);
        if mode & want == 0 {
            return Err(format!(
                "Half-close of {side}-side not possible, side not opened or already closed"
            ));
        }
        // A channel with only the named side open is closed outright, which is
        // what `Tcl_CloseEx` does once no side is left — `close $f r` on a
        // read-only channel is a plain close in tclsh, measured.
        //
        // A genuine half-close of a read-write channel is refused. It needs a
        // driver whose `close2Proc` honours `TCL_CLOSE_READ` /
        // `TCL_CLOSE_WRITE` (`generic/tcl.h:1369-1370`), and neither device
        // here has one — tclsh's own file driver has not either, and answers
        // `close $f r` on a `w+` channel with an error.
        if mode & !want & (TCL_READABLE | TCL_WRITABLE) != 0 {
            return Err(format!(
                "half-closing the {side} side of a read-write channel is not \
                 implemented in this frontend; no device here has a close2Proc \
                 that honours it"
            ));
        }
    }
    unregister(id)
}

/// `read channel ?numChars?` and `read ?-nonewline? channel`, which are two
/// grammars over the same two words.
fn read_command(argc: usize, text: &impl Fn(usize) -> String) -> Result<Value, String> {
    let (name, count_word, nonewline) = match argc {
        1 => (text(0), None, false),
        2 if text(0) == "-nonewline" => (text(1), None, true),
        2 => (text(0), Some(text(1)), false),
        _ => {
            return Err("wrong # args: should be \"read channel ?numChars?\" or \
                 \"read ?-nonewline? channel\""
                .to_string())
        }
    };
    // The channel is resolved and checked before `numChars` is read, which is
    // the order `Tcl_ReadObjCmd` uses and is observable: `read $writeonly xyz`
    // reports the channel rather than the count.
    let id = resolve_readable(&name)?;
    // A negative count is not "read everything", it is refused — the same
    // message a non-numeric one gets.
    let count = match count_word {
        None => None,
        Some(n) => Some(
            n.parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| format!("expected non-negative integer but got \"{n}\""))?,
        ),
    };
    let mut out = read_id(id, count)?;
    // One newline, not every trailing one: `read -nonewline` on a file ending
    // in two blank lines keeps the first of them. Measured against tclsh.
    if nonewline && out.ends_with('\n') {
        out.pop();
    }
    Ok(Value::Str(Arc::new(out)))
}

/// `seek channel offset ?origin?` (`generic/tclIO.c:7161`).
fn seek_command(name: &str, offset: &str, origin: Option<String>) -> Result<(), String> {
    let id = resolve(name)?;
    let offset: i64 = offset
        .parse()
        .map_err(|_| format!("expected integer but got \"{offset}\""))?;
    let whence = match origin.as_deref().unwrap_or("start") {
        "start" => Whence::Start,
        "current" => Whence::Current,
        "end" => Whence::End,
        other => {
            return Err(format!(
                "bad origin \"{other}\": must be start, current, or end"
            ))
        }
    };
    seek_at(id, offset, whence)
        .map(|_| ())
        .map_err(|e| match e.as_str() {
            "illegal seek" => format!("error during seek on \"{name}\": illegal seek"),
            _ => e,
        })
}

/// The seek itself, by id: `Tcl_Seek` (`generic/tclIO.c:7161`).
///
/// A seek discards what is buffered on both sides and clears the end-of-file
/// condition (`generic/tclIO.c:7230-7250`), which is what makes `seek $f 0`
/// followed by `read $f` answer the whole file again.
fn seek_at(id: usize, offset: i64, whence: Whence) -> Result<i64, String> {
    flush_id(id, None)?;
    with_channel(id, |c| {
        if !c.device.seekable() {
            return Err("illegal seek".to_string());
        }
        // The position the script asked for is a position in the *file*, and
        // what is buffered here was read past it, so it has to go first.
        let buffered = c.pending.chars().count() as i64 + c.raw.len() as i64;
        let offset = if whence == Whence::Current {
            offset - buffered
        } else {
            offset
        };
        let at = c.device.seek(offset, whence)?;
        c.pending.clear();
        c.raw.clear();
        c.device_eof = false;
        c.saw_cr = false;
        Ok(at)
    })
}

/// `tell channel` (`generic/tclIO.c:7330`): the device's position, less
/// whatever was read ahead of the script.
fn tell_command(name: &str) -> Result<i64, String> {
    let id = resolve(name)?;
    with_channel(id, |c| {
        if !c.device.seekable() {
            return Ok(-1);
        }
        let at = c.device.seek(0, Whence::Current)?;
        Ok(at - c.pending.chars().count() as i64 - c.raw.len() as i64)
    })
}

/// `fconfigure channel ?-option value ...?`.
fn fconfigure(name: &str, rest: &[String]) -> Result<String, String> {
    let id = resolve(name)?;
    match rest.len() {
        0 => {
            let mut parts = Vec::with_capacity(GENERIC_OPTIONS.len() * 2);
            for option in GENERIC_OPTIONS {
                parts.push((*option).to_string());
                parts.push(get_option(id, option)?);
            }
            Ok(crate::list::join(&parts))
        }
        1 => get_option(id, &rest[0]),
        n if n % 2 == 0 => {
            for pair in rest.chunks(2) {
                set_option(id, &pair[0], &pair[1])?;
            }
            Ok(String::new())
        }
        // An odd number of option words past the first is a shape error rather
        // than a missing value: tclsh reports the usage.
        _ => Err("wrong # args: should be \"fconfigure channel ?-option value ...?\"".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel's name is the one `open` returned, and it is gone once the
    /// channel is closed — the two halves of the name table.
    #[test]
    fn a_closed_channel_gives_its_name_back() {
        let path = std::env::temp_dir().join(format!("tclrs-chan-name-{}", std::process::id()));
        std::fs::write(&path, b"x").expect("write");
        let name = open(path.to_str().expect("path"), None).expect("open");
        assert!(lookup(&name).is_some());
        close_command(&name, None).expect("close");
        assert!(lookup(&name).is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// `auto` answers `\n` for all three line endings, and a `\r\n` split
    /// across two fills stays one line ending — the reason `saw_cr` exists.
    #[test]
    fn auto_translation_joins_a_split_carriage_return() {
        let mut c = Channel {
            name: "test".to_string(),
            device: Box::new(StdDevice { kind: StdKind::In }),
            mode: TCL_READABLE,
            ref_count: 0,
            input_translation: Translation::Auto,
            output_translation: Translation::Lf,
            encoding: Encoding::Utf8,
            buffering: Buffering::Full,
            buffer_size: DEFAULT_BUFFER_SIZE,
            blocking: true,
            raw: Vec::new(),
            pending: String::new(),
            device_eof: false,
            saw_cr: false,
            out: Vec::new(),
            handlers: Vec::new(),
        };
        translate_in(&mut c, "a\r");
        translate_in(&mut c, "\nb\rc\nd");
        assert_eq!(c.pending, "a\nb\nc\nd");
    }
}
