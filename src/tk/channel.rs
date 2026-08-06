//! The channel slots, and the driver that calls back into Tk.
//!
//! This is the third caller-supplied struct after [`super::hash`]'s
//! `Tcl_HashTable` and [`super::objtype`]'s `Tcl_ObjType`, and it is the one
//! that reverses the direction of the boundary. Everywhere else Tk asks the
//! host a question; here Tk hands over a `Tcl_ChannelType` — a table of its own
//! function pointers (`generic/tcl.h:1445-1494`,
//! [`super::abi::TclChannelType`]) — and the host calls *into* it every time
//! the channel reads, writes or closes. Tk's console is defined that way
//! (`tk9.0.4/generic/tkConsole.c:66-84`) and there is no other way to serve it.
//!
//! What is here is the C side only. The generic layer — the name table, the
//! reference count, the translation, the encoding, the buffering, the standard
//! channels — is [`crate::cmd_channel`], which is where a channel a *script*
//! opens lives too, so the two kinds are one table and Tk's console channel can
//! be written to by `puts` and reconfigured by `fconfigure`.
//!
//! # The version check is not optional
//!
//! `Tcl_CreateChannel` panics on five distinct conditions before it allocates
//! anything (`generic/tclIO.c:1609-1626`): no type name, a `version` that is
//! not `TCL_CHANNEL_VERSION_5`, no `close2Proc`, no `inputProc` on a readable
//! channel, no `outputProc` on a writable one, and no `watchProc` at all. All
//! six are reproduced. They are the only thing standing between a driver table
//! this side misread and a jump through a null or misaligned pointer, and
//! every one of them is a caller bug that is otherwise silent.
//!
//! # What is refused
//!
//! * `Tcl_StackChannel`, `Tcl_UnstackChannel`, `Tcl_GetStackedChannel` and
//!   `Tcl_GetTopChannel` — the generic layer has one driver per channel; see
//!   [`crate::cmd_channel`].
//! * `Tcl_OpenCommandChannel` and `Tcl_MakeTcpClientChannel` — there is no
//!   pipe or socket driver.
//! * The `Tcl_Channel*Proc` accessors (`Tcl_ChannelInputProc` and the twelve
//!   beside it), which exist so that a *stacked* driver can call the one below
//!   it. Nothing here stacks, and Tk names none of them.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

use super::abi::{RawStub, TclChannelType, TclDString, TclObj, TclStubs, TCL_CHANNEL_VERSION_5};
use super::generated::TCL_NAMES;
use super::trace::{record, Table};
use crate::cmd_channel::{self, Device, Whence, TCL_READABLE, TCL_WRITABLE};

macro_rules! entered {
    ($name:literal) => {
        record(
            Table::Tcl,
            TCL_NAMES
                .iter()
                .position(|n| *n == $name)
                .expect("no such slot"),
        )
    };
}

/// `TCL_OK` / `TCL_ERROR` (`generic/tcl.h`).
const TCL_OK: c_int = 0;
const TCL_ERROR: c_int = 1;

/// `TCL_INDEX_NONE` (`generic/tcl.h:2292`), which is what the read and write
/// slots return on failure (`generic/tclIO.c:4185`).
const TCL_INDEX_NONE: isize = -1;

// ── the token Tk holds ───────────────────────────────────────────────────

/// What a `Tcl_Channel` points at.
///
/// `Tcl_Channel` is opaque to Tk — `typedef struct Tcl_Channel_ *Tcl_Channel`
/// (`generic/tcl.h`), and no macro in the header reaches inside one — so the
/// host is free to choose what it addresses, as long as the address is stable
/// and unique. This is that address: one leaked box per channel, holding the
/// id of the entry in [`crate::cmd_channel`]'s table.
///
/// Leaked rather than owned because Tk stores the pointer (in its `ConsoleInfo`
/// and in the three standard-channel slots) and there is no moment at which
/// this side can prove the last copy is gone.
struct Token {
    id: usize,
}

/// The `Tcl_Channel` for a channel id, allocated once and reused, so that Tk
/// comparing two `Tcl_Channel`s for identity — which `Tk_CreateConsoleWindow`
/// does not, but `CheckForStdChannelsBeingClosed` does
/// (`generic/tclIO.c:650-652`) — gets the right answer.
fn token_for(id: usize) -> *mut c_void {
    TOKENS.with(|t| {
        let mut map = t.borrow_mut();
        *map.entry(id)
            .or_insert_with(|| Box::into_raw(Box::new(Token { id })) as usize)
            as *mut c_void
    })
}

/// The channel id a `Tcl_Channel` names.
///
/// # Safety
/// `chan` is null or a pointer [`token_for`] returned.
unsafe fn id_of(chan: *mut c_void) -> Option<usize> {
    if chan.is_null() {
        return None;
    }
    Some((*(chan as *const Token)).id)
}

thread_local! {
    /// One token per channel id, so the mapping is a bijection in both
    /// directions.
    static TOKENS: std::cell::RefCell<std::collections::HashMap<usize, usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

// ── the driver ───────────────────────────────────────────────────────────

/// A [`Device`] whose four operations are Tk's own function pointers.
///
/// The pointers are `*const c_void` because their real types are the
/// `Tcl_Driver*Proc` typedefs (`generic/tcl.h:1399-1432`) and each is
/// transmuted back at its one call site, next to the typedef it came from —
/// the same discipline [`super::host`] uses for the stub table itself.
pub struct Driver {
    /// The `Tcl_ChannelType` Tk passed to `Tcl_CreateChannel`. Tk's is
    /// `static const` (`tk9.0.4/generic/tkConsole.c:66`), so the pointer stays
    /// valid for the life of the process and is what `Tcl_GetChannelType` has
    /// to hand back — Tk compares it against `&consoleChannelType` by address
    /// (`tk9.0.4/generic/tkConsole.c:361-366`).
    type_ptr: *const TclChannelType,
    /// `chanPtr->instanceData` (`generic/tclIO.c:1637`), passed to every driver
    /// proc and handed back by `Tcl_GetChannelInstanceData`.
    instance_data: *mut c_void,
    /// The type name, copied out at creation: `Tcl_ChannelType::typeName` is
    /// owned by the driver (`generic/tcl.h:1446-1448`) and [`Device::type_name`]
    /// must answer without an unsafe read at every call.
    name: String,
    /// Whether `close2Proc` has already run, so that a double close does not
    /// call into Tk twice.
    closed: bool,
}

impl Driver {
    /// The driver table, as a reference.
    ///
    /// # Safety
    /// `type_ptr` came from `Tcl_CreateChannel` and Tk keeps it alive.
    unsafe fn ty(&self) -> &TclChannelType {
        &*self.type_ptr
    }
}

impl Device for Driver {
    fn type_name(&self) -> &str {
        &self.name
    }

    /// `inputProc`: `int (*)(void *instanceData, char *buf, int toRead,
    /// int *errorCodePtr)` (`generic/tcl.h:1403-1404`).
    ///
    /// A negative return is a failure and `*errorCodePtr` is the errno
    /// (`generic/tclIO.c`'s `ChanRead`); zero is end of file.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        unsafe {
            let proc = self.ty().input_proc;
            if proc.is_null() {
                return Err("channel is not readable".to_string());
            }
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_int) -> c_int,
            >(proc);
            let mut errno: c_int = 0;
            let n = f(
                self.instance_data,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
                &mut errno,
            );
            if n < 0 {
                return Err(posix_message(errno));
            }
            Ok(n as usize)
        }
    }

    /// `outputProc`: `int (*)(void *instanceData, const char *buf, int toWrite,
    /// int *errorCodePtr)` (`generic/tcl.h:1405-1406`).
    fn write(&mut self, buf: &[u8]) -> Result<usize, String> {
        unsafe {
            let proc = self.ty().output_proc;
            if proc.is_null() {
                return Err("channel is not writable".to_string());
            }
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int,
            >(proc);
            let mut errno: c_int = 0;
            let n = f(
                self.instance_data,
                buf.as_ptr() as *const c_char,
                buf.len() as c_int,
                &mut errno,
            );
            if n < 0 {
                return Err(posix_message(errno));
            }
            Ok(n as usize)
        }
    }

    /// `wideSeekProc`: `long long (*)(void *instanceData, long long offset,
    /// int mode, int *errorCodePtr)` (`generic/tcl.h:1420-1421`).
    fn seek(&mut self, offset: i64, whence: Whence) -> Result<i64, String> {
        unsafe {
            let proc = self.ty().wide_seek_proc;
            if proc.is_null() {
                return Err("illegal seek".to_string());
            }
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, i64, c_int, *mut c_int) -> i64,
            >(proc);
            let mut errno: c_int = 0;
            let at = f(self.instance_data, offset, seek_mode(whence), &mut errno);
            if at < 0 {
                return Err(posix_message(errno));
            }
            Ok(at)
        }
    }

    fn seekable(&self) -> bool {
        unsafe { !self.ty().wide_seek_proc.is_null() }
    }

    /// `close2Proc`: `int (*)(void *instanceData, Tcl_Interp *interp, int flags)`
    /// (`generic/tcl.h:1401-1402`). `flags` of 0 means close both sides
    /// (`generic/tcl.h:1367-1370`).
    fn close(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        unsafe {
            let proc = self.ty().close2_proc;
            if proc.is_null() {
                return Ok(());
            }
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
            >(proc);
            let rc = f(
                self.instance_data,
                super::interp::current() as *mut c_void,
                0,
            );
            if rc != 0 {
                return Err(posix_message(rc));
            }
            Ok(())
        }
    }

    /// `getHandleProc`: `int (*)(void *instanceData, int direction,
    /// void **handlePtr)` (`generic/tcl.h:1414-1415`).
    fn handle(&self, direction: i32) -> Option<isize> {
        unsafe {
            let proc = self.ty().get_handle_proc;
            if proc.is_null() {
                return None;
            }
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_void) -> c_int,
            >(proc);
            let mut handle: *mut c_void = ptr::null_mut();
            if f(self.instance_data, direction, &mut handle) != TCL_OK {
                return None;
            }
            Some(handle as isize)
        }
    }

    /// `watchProc`: `void (*)(void *instanceData, int mask)`
    /// (`generic/tcl.h:1413`). Not optional — `Tcl_CreateChannel` refuses a
    /// type without one (`generic/tclIO.c:1624-1626`) — so it is called
    /// unconditionally.
    fn watch(&mut self, mask: i32) {
        unsafe {
            let proc = self.ty().watch_proc;
            if proc.is_null() {
                return;
            }
            let f = std::mem::transmute::<*const c_void, unsafe extern "C" fn(*mut c_void, c_int)>(
                proc,
            );
            f(self.instance_data, mask);
        }
    }

    /// `setOptionProc`: `int (*)(void *, Tcl_Interp *, const char *optionName,
    /// const char *value)` (`generic/tcl.h:1408-1410`), reached when the
    /// generic layer does not recognise the option (`generic/tclIO.c:8463-8465`).
    fn set_option(&mut self, name: &str, value: &str) -> Option<Result<(), String>> {
        unsafe {
            let proc = self.ty().set_option_proc;
            if proc.is_null() {
                return None;
            }
            let (name, value) = (c_string(name), c_string(value));
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *const c_char,
                    *const c_char,
                ) -> c_int,
            >(proc);
            let rc = f(
                self.instance_data,
                super::interp::current() as *mut c_void,
                name.as_ptr(),
                value.as_ptr(),
            );
            Some(if rc == TCL_OK {
                Ok(())
            } else {
                Err(format!("the channel driver refused {:?}", name.to_bytes()))
            })
        }
    }

    /// `getOptionProc`: `int (*)(void *, Tcl_Interp *, const char *optionName,
    /// Tcl_DString *)` (`generic/tcl.h:1411-1412`).
    ///
    /// The `Tcl_DString` is this side's, initialised and freed around the call,
    /// because the driver appends to it and the answer is wanted as a Rust
    /// string.
    fn get_option(&mut self, name: &str) -> Option<Result<String, String>> {
        unsafe {
            let proc = self.ty().get_option_proc;
            if proc.is_null() {
                return None;
            }
            let name = c_string(name);
            let f = std::mem::transmute::<
                *const c_void,
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *const c_char,
                    *mut TclDString,
                ) -> c_int,
            >(proc);
            let mut ds = std::mem::zeroed::<TclDString>();
            super::dstring::init(&mut ds);
            let rc = f(
                self.instance_data,
                super::interp::current() as *mut c_void,
                name.as_ptr(),
                &mut ds,
            );
            let text = if ds.string.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    ds.string as *const u8,
                    ds.length.max(0) as usize,
                ))
                .into_owned()
            };
            super::dstring::free(&mut ds);
            Some(if rc == TCL_OK { Ok(text) } else { Err(text) })
        }
    }

    fn driver_table(&self) -> Option<usize> {
        Some(self.type_ptr as usize)
    }

    fn instance_data(&self) -> Option<usize> {
        Some(self.instance_data as usize)
    }
}

/// A NUL-terminated copy of `s`, for a driver proc that takes `const char *`.
/// An interior NUL cannot reach here from an option name and is truncated at
/// rather than refused, since the driver would stop there anyway.
fn c_string(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_else(|e| {
        let bytes = e.into_vec();
        let at = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        std::ffi::CString::new(&bytes[..at]).unwrap_or_default()
    })
}

/// `SEEK_SET` / `SEEK_CUR` / `SEEK_END`, which is what a driver's seek proc
/// takes as its `mode`.
fn seek_mode(whence: Whence) -> c_int {
    match whence {
        Whence::Start => libc::SEEK_SET,
        Whence::Current => libc::SEEK_CUR,
        Whence::End => libc::SEEK_END,
    }
}

/// An errno in the wording `Tcl_PosixError` reports it.
fn posix_message(errno: c_int) -> String {
    let text = unsafe {
        CStr::from_ptr(libc::strerror(errno))
            .to_string_lossy()
            .into_owned()
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => text,
    }
}

// ── the slots ────────────────────────────────────────────────────────────

/// Slot 88. `Tcl_CreateChannel` (`generic/tclIO.c:1594-1767`).
///
/// The six panics at `:1609-1626` come first and are reproduced exactly,
/// because every one of them catches a driver table this side would otherwise
/// call through blindly.
unsafe extern "C" fn create_channel(
    type_ptr: *const TclChannelType,
    chan_name: *const c_char,
    instance_data: *mut c_void,
    mask: c_int,
) -> *mut c_void {
    entered!("tcl_CreateChannel");
    assert!(
        !type_ptr.is_null() && !(*type_ptr).type_name.is_null(),
        "channel does not have a type name (generic/tclIO.c:1609-1611)"
    );
    let ty = &*type_ptr;
    let name = CStr::from_ptr(ty.type_name).to_string_lossy().into_owned();
    assert!(
        ty.version == TCL_CHANNEL_VERSION_5,
        "channel type {name} must be version TCL_CHANNEL_VERSION_5 \
         (generic/tclIO.c:1612-1614); got {}",
        ty.version
    );
    assert!(
        !ty.close2_proc.is_null(),
        "channel type {name} must define close2Proc (generic/tclIO.c:1615-1617)"
    );
    assert!(
        mask & TCL_READABLE == 0 || !ty.input_proc.is_null(),
        "channel type {name} must define inputProc when used for reader channel \
         (generic/tclIO.c:1618-1620)"
    );
    assert!(
        mask & TCL_WRITABLE == 0 || !ty.output_proc.is_null(),
        "channel type {name} must define outputProc when used for writer channel \
         (generic/tclIO.c:1621-1623)"
    );
    assert!(
        !ty.watch_proc.is_null(),
        "channel type {name} must define watchProc (generic/tclIO.c:1624-1626)"
    );

    // A NULL name is legal and means the empty string (`:1655-1658`).
    let chan_name = if chan_name.is_null() {
        String::new()
    } else {
        CStr::from_ptr(chan_name).to_string_lossy().into_owned()
    };
    let id = cmd_channel::create(
        &chan_name,
        Box::new(Driver {
            type_ptr,
            instance_data,
            name,
            closed: false,
        }),
        mask,
    );
    // `:1751-1765`: a channel created while a standard slot is empty *and* has
    // been consulted takes that slot and is renamed to it. Tk's console relies
    // on the opposite path — it calls `Tcl_SetStdChannel` itself — but the rule
    // is what makes a reopened stdout work at all.
    cmd_channel::adopt_empty_std_slot(id);
    token_for(id)
}

/// Slot 210. `Tcl_RegisterChannel` (`generic/tclIO.c:1161-1198`).
///
/// The interpreter argument is ignored, which is what the C does when it is
/// NULL (`:1185`) — the only form Tk's console uses
/// (`tk9.0.4/generic/tkConsole.c:276`) — and what this frontend can honour,
/// since it has one channel table per thread rather than one per interpreter.
unsafe extern "C" fn register_channel(_interp: *mut c_void, chan: *mut c_void) {
    entered!("tcl_RegisterChannel");
    if let Some(id) = id_of(chan) {
        cmd_channel::register(id);
    }
}

/// Slot 252. `Tcl_UnregisterChannel` (`generic/tclIO.c:1226-1283`).
unsafe extern "C" fn unregister_channel(_interp: *mut c_void, chan: *mut c_void) -> c_int {
    entered!("tcl_UnregisterChannel");
    match id_of(chan) {
        Some(id) => match cmd_channel::unregister(id) {
            Ok(()) => TCL_OK,
            Err(_) => TCL_ERROR,
        },
        None => TCL_OK,
    }
}

/// Slot 151. `Tcl_GetChannel` (`generic/tclIO.c:1424-1484`).
unsafe extern "C" fn get_channel(
    _interp: *mut c_void,
    chan_name: *const c_char,
    mode_ptr: *mut c_int,
) -> *mut c_void {
    entered!("tcl_GetChannel");
    let name = CStr::from_ptr(chan_name).to_string_lossy().into_owned();
    match cmd_channel::lookup(&name) {
        Some(id) => {
            if !mode_ptr.is_null() {
                *mode_ptr = cmd_channel::mode_of(id);
            }
            token_for(id)
        }
        None => ptr::null_mut(),
    }
}

/// Slot 173. `Tcl_GetStdChannel` (`generic/tclIO.c:763-830`).
unsafe extern "C" fn get_std_channel(kind: c_int) -> *mut c_void {
    entered!("tcl_GetStdChannel");
    match cmd_channel::std_channel(kind) {
        Some(id) => token_for(id),
        None => ptr::null_mut(),
    }
}

/// Slot 236. `Tcl_SetStdChannel` (`generic/tclIO.c:719-745`).
unsafe extern "C" fn set_std_channel(chan: *mut c_void, kind: c_int) {
    entered!("tcl_SetStdChannel");
    cmd_channel::set_std(kind, id_of(chan));
}

/// Slot 338. `Tcl_WriteChars` (`generic/tclIO.c:4171-4218`): a byte count, or
/// `TCL_INDEX_NONE` on failure.
unsafe extern "C" fn write_chars(chan: *mut c_void, src: *const c_char, len: isize) -> isize {
    entered!("tcl_WriteChars");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    let bytes = super::host::c_bytes_of(src, len);
    match cmd_channel::write_bytes(id, bytes) {
        Ok(()) => bytes.len() as isize,
        Err(_) => TCL_INDEX_NONE,
    }
}

/// Slot 339. `Tcl_WriteObj` (`generic/tclIO.c:4245-4280`).
unsafe extern "C" fn write_obj(chan: *mut c_void, obj: *mut TclObj) -> isize {
    entered!("tcl_WriteObj");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    let bytes = super::host::obj_bytes_of(obj);
    match cmd_channel::write_bytes(id, bytes) {
        Ok(()) => bytes.len() as isize,
        Err(_) => TCL_INDEX_NONE,
    }
}

/// Slot 263. `Tcl_Write` (`generic/tclIO.c:4061`), which is `Tcl_WriteChars`
/// without the encoding step — the bytes go out as they are.
unsafe extern "C" fn write_bytes_slot(chan: *mut c_void, src: *const c_char, len: isize) -> isize {
    entered!("tcl_Write");
    write_chars(chan, src, len)
}

/// Slot 313. `Tcl_ReadChars` (`generic/tclIO.c:5877`): up to `chars_to_read`
/// characters into `obj`, appending when `append_flag` is set. Returns the
/// count, or `TCL_INDEX_NONE`.
unsafe extern "C" fn read_chars(
    chan: *mut c_void,
    obj: *mut TclObj,
    chars_to_read: isize,
    append_flag: c_int,
) -> isize {
    entered!("tcl_ReadChars");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    let want = if chars_to_read < 0 {
        None
    } else {
        Some(chars_to_read as i64)
    };
    match cmd_channel::read_chars(id, want) {
        Ok(text) => {
            if append_flag != 0 {
                super::obj::append_bytes(obj, text.as_bytes());
            } else {
                super::obj::set_string(obj, text.as_bytes());
            }
            text.chars().count() as isize
        }
        Err(_) => TCL_INDEX_NONE,
    }
}

/// Slot 206. `Tcl_Read` (`generic/tclIO.c:5714`): raw bytes into the caller's
/// buffer.
unsafe extern "C" fn read_bytes_slot(chan: *mut c_void, buf: *mut c_char, to_read: isize) -> isize {
    entered!("tcl_Read");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    match cmd_channel::read_chars(id, Some(to_read as i64)) {
        Ok(text) => {
            let bytes = text.as_bytes();
            let n = bytes.len().min(to_read.max(0) as usize);
            ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
            n as isize
        }
        Err(_) => TCL_INDEX_NONE,
    }
}

/// Slot 170. `Tcl_GetsObj` (`generic/tclIO.c:4601`): the next line into `obj`,
/// without its terminator, or `TCL_INDEX_NONE` at end of file.
unsafe extern "C" fn gets_obj(chan: *mut c_void, obj: *mut TclObj) -> isize {
    entered!("tcl_GetsObj");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    match cmd_channel::gets(id) {
        Ok(Some(line)) => {
            super::obj::set_string(obj, line.as_bytes());
            line.chars().count() as isize
        }
        _ => TCL_INDEX_NONE,
    }
}

/// Slot 169. `Tcl_Gets` (`generic/tclIO.c:4558`), the `Tcl_DString` form.
unsafe extern "C" fn gets_dstring(chan: *mut c_void, ds: *mut TclDString) -> isize {
    entered!("tcl_Gets");
    let Some(id) = id_of(chan) else {
        return TCL_INDEX_NONE;
    };
    match cmd_channel::gets(id) {
        Ok(Some(line)) => {
            super::dstring::append(ds, line.as_ptr() as *const c_char, line.len() as isize);
            line.chars().count() as isize
        }
        _ => TCL_INDEX_NONE,
    }
}

/// Slot 146. `Tcl_Flush` (`generic/tclIO.c:6920`).
unsafe extern "C" fn flush_channel(chan: *mut c_void) -> c_int {
    entered!("tcl_Flush");
    match id_of(chan) {
        Some(id) if cmd_channel::flush(id).is_ok() => TCL_OK,
        Some(_) => TCL_ERROR,
        None => TCL_ERROR,
    }
}

/// Slot 81. `Tcl_Close` (`generic/tclIO.c`), which is `Tcl_CloseEx` with no
/// flags: close both sides and destroy the channel.
unsafe extern "C" fn close_channel(_interp: *mut c_void, chan: *mut c_void) -> c_int {
    entered!("tcl_Close");
    match id_of(chan) {
        Some(id) => {
            TOKENS.with(|t| t.borrow_mut().remove(&id));
            match cmd_channel::close(id) {
                Ok(()) => TCL_OK,
                Err(_) => TCL_ERROR,
            }
        }
        None => TCL_ERROR,
    }
}

/// Slot 624. `Tcl_CloseEx` (`generic/tclIO.c`). A non-zero `flags` is a
/// half-close (`TCL_CLOSE_READ` / `TCL_CLOSE_WRITE`, `generic/tcl.h:1369-1370`).
unsafe extern "C" fn close_ex(interp: *mut c_void, chan: *mut c_void, flags: c_int) -> c_int {
    entered!("tcl_CloseEx");
    if flags == 0 {
        return close_channel(interp, chan);
    }
    match id_of(chan) {
        Some(id) => match cmd_channel::half_close(id, flags) {
            Ok(()) => TCL_OK,
            Err(_) => TCL_ERROR,
        },
        None => TCL_ERROR,
    }
}

/// Slot 156. `Tcl_GetChannelName` (`generic/tclIO.c:2370-2385`).
///
/// The C returns `statePtr->channelName`, memory the channel owns for as long
/// as it lives. This side keeps the name as a Rust `String`, so a leaked C copy
/// is made once per channel and reused — a fresh allocation per call would be a
/// leak on every call, and a pointer into the `String` would dangle the moment
/// the table resized.
unsafe extern "C" fn get_channel_name(chan: *mut c_void) -> *const c_char {
    entered!("tcl_GetChannelName");
    match id_of(chan).and_then(cmd_channel::name_of) {
        Some(name) => NAMES.with(|n| {
            let mut map = n.borrow_mut();
            let id = id_of(chan).unwrap_or(0);
            *map.entry(id).or_insert_with(|| {
                let c = std::ffi::CString::new(name).unwrap_or_default();
                c.into_raw() as usize
            }) as *const c_char
        }),
        None => ptr::null(),
    }
}

thread_local! {
    /// The C copy of each channel's name, made once — see [`get_channel_name`].
    static NAMES: std::cell::RefCell<std::collections::HashMap<usize, usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Slot 158. `Tcl_GetChannelType` (`generic/tclIO.c:2315-2330`).
///
/// The identity of this pointer is the whole point: `Tk_CreateConsoleWindow`
/// finds its console channel by comparing what this returns against
/// `&consoleChannelType` (`tk9.0.4/generic/tkConsole.c:361-366`), so returning
/// a copy of the struct rather than the address Tk gave would make the console
/// invisible to Tk.
unsafe extern "C" fn get_channel_type(chan: *mut c_void) -> *const TclChannelType {
    entered!("tcl_GetChannelType");
    id_of(chan)
        .and_then(cmd_channel::driver_table)
        .map_or(ptr::null(), |a| a as *const TclChannelType)
}

/// Slot 154. `Tcl_GetChannelInstanceData` (`generic/tclIO.c:2262-2277`).
unsafe extern "C" fn get_channel_instance_data(chan: *mut c_void) -> *mut c_void {
    entered!("tcl_GetChannelInstanceData");
    id_of(chan)
        .and_then(cmd_channel::instance_data)
        .map_or(ptr::null_mut(), |a| a as *mut c_void)
}

/// Slot 155. `Tcl_GetChannelMode` (`generic/tclIO.c:2342-2360`).
unsafe extern "C" fn get_channel_mode(chan: *mut c_void) -> c_int {
    entered!("tcl_GetChannelMode");
    id_of(chan).map_or(0, cmd_channel::mode_of)
}

/// Slot 153. `Tcl_GetChannelHandle` (`generic/tclIO.c:2397-2420`).
unsafe extern "C" fn get_channel_handle(
    chan: *mut c_void,
    direction: c_int,
    handle_ptr: *mut *mut c_void,
) -> c_int {
    entered!("tcl_GetChannelHandle");
    match id_of(chan).and_then(|id| cmd_channel::handle_of(id, direction)) {
        Some(handle) => {
            if !handle_ptr.is_null() {
                *handle_ptr = handle as *mut c_void;
            }
            TCL_OK
        }
        None => TCL_ERROR,
    }
}

/// Slot 225. `Tcl_SetChannelOption` (`generic/tclIO.c:8179-8471`): the generic
/// options first, then the driver's own `setOptionProc` (`:8463-8465`), then
/// `Tcl_BadChannelOption` (`:8467`).
unsafe extern "C" fn set_channel_option(
    _interp: *mut c_void,
    chan: *mut c_void,
    option_name: *const c_char,
    new_value: *const c_char,
) -> c_int {
    entered!("tcl_SetChannelOption");
    let Some(id) = id_of(chan) else {
        return TCL_ERROR;
    };
    let name = CStr::from_ptr(option_name).to_string_lossy().into_owned();
    let value = CStr::from_ptr(new_value).to_string_lossy().into_owned();
    match cmd_channel::set_channel_option(id, &name, &value) {
        Ok(()) => TCL_OK,
        Err(_) => TCL_ERROR,
    }
}

/// Slot 157. `Tcl_GetChannelOption` (`generic/tclIO.c:7966-8110`).
unsafe extern "C" fn get_channel_option(
    _interp: *mut c_void,
    chan: *mut c_void,
    option_name: *const c_char,
    ds: *mut TclDString,
) -> c_int {
    entered!("tcl_GetChannelOption");
    let Some(id) = id_of(chan) else {
        return TCL_ERROR;
    };
    // A NULL option name asks for every option as a name/value list
    // (`generic/tclIO.c:7990-7995`).
    let answer = if option_name.is_null() {
        cmd_channel::all_options(id)
    } else {
        cmd_channel::get_channel_option(id, &CStr::from_ptr(option_name).to_string_lossy())
    };
    match answer {
        Ok(text) => {
            super::dstring::append(ds, text.as_ptr() as *const c_char, text.len() as isize);
            TCL_OK
        }
        Err(_) => TCL_ERROR,
    }
}

/// Slot 152. `Tcl_GetChannelBufferSize` (`generic/tclIO.c`).
unsafe extern "C" fn get_channel_buffer_size(chan: *mut c_void) -> isize {
    entered!("tcl_GetChannelBufferSize");
    id_of(chan).map_or(0, |id| cmd_channel::buffer_size(id) as isize)
}

/// Slot 224. `Tcl_SetChannelBufferSize` (`generic/tclIO.c`).
unsafe extern "C" fn set_channel_buffer_size(chan: *mut c_void, size: isize) {
    entered!("tcl_SetChannelBufferSize");
    if let Some(id) = id_of(chan) {
        cmd_channel::set_buffer_size(id, size as i64);
    }
}

/// Slot 491. `Tcl_Seek` (`generic/tclIO.c:7161`).
unsafe extern "C" fn seek_channel(chan: *mut c_void, offset: i64, mode: c_int) -> i64 {
    entered!("tcl_Seek");
    let whence = match mode {
        m if m == libc::SEEK_CUR => Whence::Current,
        m if m == libc::SEEK_END => Whence::End,
        _ => Whence::Start,
    };
    match id_of(chan) {
        Some(id) => cmd_channel::seek(id, offset, whence).map_or(-1, |at| at),
        None => -1,
    }
}

/// Slot 492. `Tcl_Tell` (`generic/tclIO.c:7330`).
unsafe extern "C" fn tell_channel(chan: *mut c_void) -> i64 {
    entered!("tcl_Tell");
    id_of(chan).map_or(-1, |id| cmd_channel::tell(id).unwrap_or(-1))
}

/// Slot 126. `Tcl_Eof` (`generic/tclIO.c:7604`).
unsafe extern "C" fn eof_channel(chan: *mut c_void) -> c_int {
    entered!("tcl_Eof");
    id_of(chan).map_or(0, |id| c_int::from(cmd_channel::at_eof(id)))
}

/// Slot 183. `Tcl_InputBuffered` (`generic/tclIO.c`): how much has been read
/// from the device and not yet handed to the caller.
unsafe extern "C" fn input_buffered(chan: *mut c_void) -> c_int {
    entered!("tcl_InputBuffered");
    id_of(chan).map_or(0, |id| cmd_channel::input_buffered(id) as c_int)
}

/// Slot 397. `Tcl_ChannelBuffered` (`generic/tclIO.c`): the same question for
/// the output side.
unsafe extern "C" fn channel_buffered(chan: *mut c_void) -> c_int {
    entered!("tcl_ChannelBuffered");
    id_of(chan).map_or(0, |id| cmd_channel::output_buffered(id) as c_int)
}

/// Slot 89. `Tcl_CreateChannelHandler` (`generic/tclIO.c:8874-8949`).
unsafe extern "C" fn create_channel_handler(
    chan: *mut c_void,
    mask: c_int,
    proc: *mut c_void,
    client_data: *mut c_void,
) {
    entered!("tcl_CreateChannelHandler");
    if let Some(id) = id_of(chan) {
        cmd_channel::create_channel_handler(id, mask, proc as usize, client_data as usize);
    }
}

/// Slot 101. `Tcl_DeleteChannelHandler` (`generic/tclIO.c:8951-9020`).
unsafe extern "C" fn delete_channel_handler(
    chan: *mut c_void,
    proc: *mut c_void,
    client_data: *mut c_void,
) {
    entered!("tcl_DeleteChannelHandler");
    if let Some(id) = id_of(chan) {
        cmd_channel::delete_channel_handler(id, proc as usize, client_data as usize);
    }
}

/// Slot 194. `Tcl_NotifyChannel` (`generic/tclIO.c`): run every handler whose
/// mask overlaps `mask`.
///
/// `Tcl_ChannelProc` is `void (*)(void *clientData, int mask)`
/// (`generic/tcl.h`).
unsafe extern "C" fn notify_channel(chan: *mut c_void, mask: c_int) {
    entered!("tcl_NotifyChannel");
    let Some(id) = id_of(chan) else { return };
    for (proc, client_data, hit) in cmd_channel::handlers_for(id, mask) {
        let f = std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void, c_int)>(proc);
        f(client_data as *mut c_void, hit);
    }
}

/// Slot 78. `Tcl_BadChannelOption` (`generic/tclIO.c:7876-7912`): the error a
/// driver reports for an option it does not know.
unsafe extern "C" fn bad_channel_option(
    interp: *mut c_void,
    option_name: *const c_char,
    _option_list: *const c_char,
) -> c_int {
    entered!("tcl_BadChannelOption");
    let name = CStr::from_ptr(option_name).to_string_lossy();
    let message = format!("bad option \"{name}\": should be one of -blocking, -buffering, -buffersize, -encoding, -eofchar, -profile, -translation");
    super::host::set_result_bytes(interp, message.as_bytes());
    TCL_ERROR
}

/// Slot 413. `Tcl_IsChannelShared` (`generic/tclIO.c`): whether more than one
/// interpreter has the channel registered.
unsafe extern "C" fn is_channel_shared(chan: *mut c_void) -> c_int {
    entered!("tcl_IsChannelShared");
    id_of(chan).map_or(0, |id| c_int::from(cmd_channel::ref_count(id) > 1))
}

/// Slot 414. `Tcl_IsChannelRegistered` (`generic/tclIO.c`). With one channel
/// table per thread rather than per interpreter, "registered anywhere" is the
/// only question this side can answer, and it is the one Tk asks.
unsafe extern "C" fn is_channel_registered(_interp: *mut c_void, chan: *mut c_void) -> c_int {
    entered!("tcl_IsChannelRegistered");
    id_of(chan).map_or(0, |id| c_int::from(cmd_channel::ref_count(id) > 0))
}

/// Slot 198. `Tcl_OpenFileChannel` (`generic/tclIOUtil.c:345`).
unsafe extern "C" fn open_file_channel(
    _interp: *mut c_void,
    file_name: *const c_char,
    mode_string: *const c_char,
    _permissions: c_int,
) -> *mut c_void {
    entered!("tcl_OpenFileChannel");
    let path = CStr::from_ptr(file_name).to_string_lossy().into_owned();
    let mode = if mode_string.is_null() {
        "r".to_string()
    } else {
        CStr::from_ptr(mode_string).to_string_lossy().into_owned()
    };
    match cmd_channel::open_file(&path, &mode) {
        Ok(name) => match cmd_channel::lookup(&name) {
            Some(id) => token_for(id),
            None => ptr::null_mut(),
        },
        Err(_) => ptr::null_mut(),
    }
}

/// Patch this module's slots into `t`, returning their indices.
///
/// # Safety
/// Each erased signature is the one `tclDecls.h` gives the slot, quoted on the
/// line above it.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        // int (*tcl_BadChannelOption)(Tcl_Interp *, const char *, const char *) /* 78 */
        install(t, "tcl_BadChannelOption", bad_channel_option as *const ()),
        // int (*tcl_Close)(Tcl_Interp *, Tcl_Channel chan) /* 81 */
        install(t, "tcl_Close", close_channel as *const ()),
        // Tcl_Channel (*tcl_CreateChannel)(const Tcl_ChannelType *, const char *,
        //     void *instanceData, int mask) /* 88 */
        install(t, "tcl_CreateChannel", create_channel as *const ()),
        // void (*tcl_CreateChannelHandler)(Tcl_Channel, int mask,
        //     Tcl_ChannelProc *, void *) /* 89 */
        install(
            t,
            "tcl_CreateChannelHandler",
            create_channel_handler as *const (),
        ),
        // void (*tcl_DeleteChannelHandler)(Tcl_Channel, Tcl_ChannelProc *, void *) /* 101 */
        install(
            t,
            "tcl_DeleteChannelHandler",
            delete_channel_handler as *const (),
        ),
        // int (*tcl_Eof)(Tcl_Channel chan) /* 126 */
        install(t, "tcl_Eof", eof_channel as *const ()),
        // int (*tcl_Flush)(Tcl_Channel chan) /* 146 */
        install(t, "tcl_Flush", flush_channel as *const ()),
        // Tcl_Channel (*tcl_GetChannel)(Tcl_Interp *, const char *, int *) /* 151 */
        install(t, "tcl_GetChannel", get_channel as *const ()),
        // Tcl_Size (*tcl_GetChannelBufferSize)(Tcl_Channel chan) /* 152 */
        install(
            t,
            "tcl_GetChannelBufferSize",
            get_channel_buffer_size as *const (),
        ),
        // int (*tcl_GetChannelHandle)(Tcl_Channel, int direction, void **) /* 153 */
        install(t, "tcl_GetChannelHandle", get_channel_handle as *const ()),
        // void *(*tcl_GetChannelInstanceData)(Tcl_Channel chan) /* 154 */
        install(
            t,
            "tcl_GetChannelInstanceData",
            get_channel_instance_data as *const (),
        ),
        // int (*tcl_GetChannelMode)(Tcl_Channel chan) /* 155 */
        install(t, "tcl_GetChannelMode", get_channel_mode as *const ()),
        // const char *(*tcl_GetChannelName)(Tcl_Channel chan) /* 156 */
        install(t, "tcl_GetChannelName", get_channel_name as *const ()),
        // int (*tcl_GetChannelOption)(Tcl_Interp *, Tcl_Channel, const char *,
        //     Tcl_DString *) /* 157 */
        install(t, "tcl_GetChannelOption", get_channel_option as *const ()),
        // const Tcl_ChannelType *(*tcl_GetChannelType)(Tcl_Channel chan) /* 158 */
        install(t, "tcl_GetChannelType", get_channel_type as *const ()),
        // Tcl_Size (*tcl_Gets)(Tcl_Channel chan, Tcl_DString *dsPtr) /* 169 */
        install(t, "tcl_Gets", gets_dstring as *const ()),
        // Tcl_Size (*tcl_GetsObj)(Tcl_Channel chan, Tcl_Obj *objPtr) /* 170 */
        install(t, "tcl_GetsObj", gets_obj as *const ()),
        // Tcl_Channel (*tcl_GetStdChannel)(int type) /* 173 */
        install(t, "tcl_GetStdChannel", get_std_channel as *const ()),
        // int (*tcl_InputBuffered)(Tcl_Channel chan) /* 183 */
        install(t, "tcl_InputBuffered", input_buffered as *const ()),
        // void (*tcl_NotifyChannel)(Tcl_Channel channel, int mask) /* 194 */
        install(t, "tcl_NotifyChannel", notify_channel as *const ()),
        // Tcl_Channel (*tcl_OpenFileChannel)(Tcl_Interp *, const char *,
        //     const char *, int) /* 198 */
        install(t, "tcl_OpenFileChannel", open_file_channel as *const ()),
        // Tcl_Size (*tcl_Read)(Tcl_Channel chan, char *bufPtr, Tcl_Size toRead) /* 206 */
        install(t, "tcl_Read", read_bytes_slot as *const ()),
        // void (*tcl_RegisterChannel)(Tcl_Interp *, Tcl_Channel chan) /* 210 */
        install(t, "tcl_RegisterChannel", register_channel as *const ()),
        // void (*tcl_SetChannelBufferSize)(Tcl_Channel chan, Tcl_Size sz) /* 224 */
        install(
            t,
            "tcl_SetChannelBufferSize",
            set_channel_buffer_size as *const (),
        ),
        // int (*tcl_SetChannelOption)(Tcl_Interp *, Tcl_Channel, const char *,
        //     const char *) /* 225 */
        install(t, "tcl_SetChannelOption", set_channel_option as *const ()),
        // void (*tcl_SetStdChannel)(Tcl_Channel channel, int type) /* 236 */
        install(t, "tcl_SetStdChannel", set_std_channel as *const ()),
        // int (*tcl_UnregisterChannel)(Tcl_Interp *, Tcl_Channel chan) /* 252 */
        install(t, "tcl_UnregisterChannel", unregister_channel as *const ()),
        // Tcl_Size (*tcl_Write)(Tcl_Channel chan, const char *s, Tcl_Size slen) /* 263 */
        install(t, "tcl_Write", write_bytes_slot as *const ()),
        // Tcl_Size (*tcl_ReadChars)(Tcl_Channel, Tcl_Obj *, Tcl_Size, int) /* 313 */
        install(t, "tcl_ReadChars", read_chars as *const ()),
        // Tcl_Size (*tcl_WriteChars)(Tcl_Channel, const char *, Tcl_Size) /* 338 */
        install(t, "tcl_WriteChars", write_chars as *const ()),
        // Tcl_Size (*tcl_WriteObj)(Tcl_Channel chan, Tcl_Obj *objPtr) /* 339 */
        install(t, "tcl_WriteObj", write_obj as *const ()),
        // int (*tcl_ChannelBuffered)(Tcl_Channel chan) /* 397 */
        install(t, "tcl_ChannelBuffered", channel_buffered as *const ()),
        // int (*tcl_IsChannelShared)(Tcl_Channel channel) /* 413 */
        install(t, "tcl_IsChannelShared", is_channel_shared as *const ()),
        // int (*tcl_IsChannelRegistered)(Tcl_Interp *, Tcl_Channel) /* 414 */
        install(
            t,
            "tcl_IsChannelRegistered",
            is_channel_registered as *const (),
        ),
        // long long (*tcl_Seek)(Tcl_Channel chan, long long offset, int mode) /* 491 */
        install(t, "tcl_Seek", seek_channel as *const ()),
        // long long (*tcl_Tell)(Tcl_Channel chan) /* 492 */
        install(t, "tcl_Tell", tell_channel as *const ()),
        // int (*tcl_CloseEx)(Tcl_Interp *, Tcl_Channel chan, int flags) /* 624 */
        install(t, "tcl_CloseEx", close_ex as *const ()),
    ]
}

/// As [`super::host`]'s own installer: by name, never by literal index.
///
/// # Safety
/// `f` must have the signature `tclDecls.h` gives the named slot.
unsafe fn install(t: &mut TclStubs, name: &str, f: *const ()) -> usize {
    let i = TCL_NAMES
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no slot named {name} in TclStubs"));
    t.slots[i] = std::mem::transmute::<*const (), RawStub>(f);
    i
}
