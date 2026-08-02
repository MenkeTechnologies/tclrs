//! Hosting the real Tk toolkit: measuring what it needs from an interpreter.
//!
//! Tk 9.0 does not link against Tcl. Its dylib has no undefined `Tcl_*` symbol
//! at all — every call it makes goes through a table of function pointers it
//! reads out of the interpreter it is handed. That makes the question "what
//! would tclrs have to provide to run Tk" answerable by measurement instead of
//! by reading 258 pages of documentation: hand Tk a table in which every slot
//! is a trap that names itself, call `Tk_Init`, and read off what it asks for.
//!
//! This module is that instrument, and nothing more. It creates no windows,
//! runs no event loop and implements no widget. Its output is a list.
//!
//! * [`abi`] — the layouts, each one taken from a cited line of the Tcl 9.0.4
//!   source and confirmed with `offsetof` rather than inferred.
//! * [`generated`] — the four stub tables' slot names and one trap per slot,
//!   derived from the headers by `scripts/gen_tk_stubs.py`.
//! * [`obj`] — the shadow `Tcl_Obj`: its pinned storage, the ownership rule for
//!   every pointer that crosses the boundary, and the bridge to this crate's
//!   own values.
//! * [`objtype`] — the `Tcl_ObjType` contract: the registry, the four procs,
//!   and the host's own list, dictionary and scalar types.
//! * [`dstring`] — `Tcl_DString`, which Tk allocates itself and reads two
//!   fields of directly.
//! * [`hash`] — `Tcl_HashTable`, which Tk allocates itself and calls into
//!   directly, so it cannot live behind the table.
//! * [`trace`] — the recorder that turns a call into a line of output.
//! * [`host`] — the stand-in interpreter and the slots implemented so far.
//! * [`load`] — `dlopen` of the real libtk and the `Tk_Init` call.
//!
//! Everything here is behind the `tk` cargo feature, and a build without that
//! feature never compiles a line of it, so a machine with no Tk installed is
//! unaffected.
//!
//! # What the measurement found
//!
//! Against Homebrew's arm64 tcl-tk 9.0.4, `cargo run --features tk --bin
//! tk-probe`:
//!
//! * Tk called **39 distinct slots of the 691** before it asked for something
//!   that could not be answered, over 276 calls. With one slot deliberately
//!   faked (see below) it reaches **47 distinct slots** over 419 calls. So
//!   `Tk_Init` exercises under 7% of the table, and the other 93% can be traps
//!   for as long as no widget is created.
//!
//!   (That number was 421 while the host counted two frees of its own result
//!   value as calls Tk had made — [`host`] reached the `tclFreeObj` *slot* to
//!   release it rather than the plain function behind it. The two calls Tk
//!   itself makes through `Tcl_DecrRefCount` are still there.)
//! * The run ends at `Tcl_EvalEx(interp, "file tildeexpand ~/.Xdefaults", ...)`
//!   (`tk9.0.4/generic/tkOption.c:1592`) — the first request that needs an
//!   evaluator rather than a data structure.
//! * A static scan of the whole dylib finds at least 217 distinct slots
//!   referenced somewhere in Tk, and exactly one in `TclPlatStubs`
//!   (`Tcl_MacOSXNotifierAddRunLoopMode`). None in either internal table, which
//!   matches the source: the three mentions of `TclIntStubs` functions in Tk are
//!   all inside comments.
//!
//! # Three things the stub table does not cover
//!
//! Reading the header's function list suggests Tk can be satisfied by supplying
//! 691 functions. It cannot. Three data structures are shared by *layout*, and
//! Tk operates on them with macros that never reach the table:
//!
//! 1. **`Tcl_Obj`.** `Tcl_IncrRefCount`, `Tcl_DecrRefCount` and `Tcl_IsShared`
//!    read and write `objPtr->refCount` in place (`generic/tcl.h:2517-2534`),
//!    and Tk's twelve `Tcl_ObjType` implementations write `typePtr` and
//!    `internalRep` directly. Only the free path is a slot. Worse than that:
//!    two of the objects Tk operates on are not Tcl's memory at all but Tk's own
//!    C stack (`tk9.0.4/macosx/tkMacOSXEmbed.c:160-165`,
//!    `tk9.0.4/generic/tkObj.c:201-206`), and the second leaves `refCount`
//!    uninitialised. See [`obj`] and [`objtype`].
//! 2. **`Tcl_HashTable`.** `Tcl_FindHashEntry` and `Tcl_CreateHashEntry` call
//!    function pointers stored inside the caller's own table
//!    (`generic/tcl.h:2607-2610`), so a host has to implement Tcl's hash table,
//!    not just answer questions about it. See [`hash`].
//! 3. **`Tcl_DString`, `Tcl_CmdInfo`, `Tcl_Time`, `Tcl_Namespace`,
//!    `Tcl_DictSearch`.** Declared by Tk on its own stack; `Tcl_DStringValue`
//!    and `Tcl_DStringLength` are field accesses (`generic/tcl.h:892-893`).
//!    See [`dstring`].
//!
//! # The one slot that cannot be written in stable Rust
//!
//! Seven slots are variadic. Six of them Tk calls only for error reporting, and
//! ignoring the variadic arguments is harmless. `Tcl_AppendStringsToObj`
//! (slot 15) is not one of those: Tk builds a command name out of its variadic
//! arguments (`tk9.0.4/generic/tkUtil.c:1222`). Defining a C-variadic function
//! is rejected by stable rustc (`error[E0658]`, tracking issue 44930), and on
//! AAPCS64 there is no non-variadic declaration that can reach the arguments
//! either, because they are all passed on the stack. A C trampoline is the fix
//! and is not part of this phase; setting `TCLRS_TK_DEGRADED` installs a body
//! that appends nothing, purely so the enumeration can continue past it, and
//! every line of that run describes a Tk that was fed a truncated string.

pub mod abi;
pub mod dstring;
pub mod generated;
pub mod hash;
pub mod host;
pub mod load;
pub mod obj;
pub mod objtype;
pub mod trace;

pub use abi::RawStub;
