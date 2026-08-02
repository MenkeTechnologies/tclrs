//! Hosting the real Tk toolkit: what it needs from an interpreter, and the
//! interpreter it is given.
//!
//! Tk 9.0 does not link against Tcl. Its dylib has no undefined `Tcl_*` symbol
//! at all — every call it makes goes through a table of function pointers it
//! reads out of the interpreter it is handed. That makes the question "what
//! would tclrs have to provide to run Tk" answerable by measurement instead of
//! by reading 258 pages of documentation: hand Tk a table in which every slot
//! is a trap that names itself, call `Tk_Init`, and read off what it asks for.
//!
//! That measurement came first and is still here: [`host::build`] hands Tk a
//! table whose unimplemented slots trap, and [`host::Level::Probe`] is the name
//! of that table. On top of it sits the host proper — [`host::build_hosting`] —
//! which adds the evaluator, so a script Tk hands over is compiled by this
//! crate's own parser and compiler and run on fusevm rather than trapped on.
//! Neither creates a window, runs an event loop or implements a widget.
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
//! * [`host`] — the interpreter Tk is handed and the slots implemented so far.
//! * [`interp`] — one [`crate::runtime::Interp`] behind every `Tcl_Interp *`,
//!   including the second one Tk creates for its option database.
//! * [`eval`] — the `Tcl_Eval*` slots, and the two callbacks the C trampoline
//!   in `trampoline.c` calls back through.
//! * [`dispatch`] — calling a command Tk registered from a script this crate
//!   compiled, which is the one thing compile-time name resolution cannot do.
//! * [`notifier`] — the event loop: Tcl's event queue, timers, idle handlers
//!   and file handlers, ported from Tcl 9.0.4 onto a CFRunLoop.
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
//!   that could not be answered, over 274 calls. With one slot deliberately
//!   faked (see below) it reaches **47 distinct slots** over 419 calls. So
//!   `Tk_Init` exercises under 7% of the table, and the other 93% can be traps
//!   for as long as no widget is created.
//!
//!   Those totals were 276 and 421 when they were first measured, and the two
//!   calls that went are not Tk's: `Tcl_ResetResult` and `Tcl_SetObjResult`
//!   released the previous result by calling slot 30's *body*, which logged a
//!   `TclFreeObj` line as though Tk had asked for one. Splitting the body out
//!   of the slot — as `Tcl_DStringInit` and `reset_dstring` already were —
//!   leaves the log holding only what Tk called. The two `TclFreeObj` lines
//!   that remain are Tk's own, through the `Tcl_DecrRefCount` macro, and are
//!   what `tests/tk_probe_session.rs` asserts on. Distinct slots and the
//!   stopping point are unchanged.
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
//! Seven slots are variadic. Defining a C-variadic function is rejected by
//! stable rustc (`error[E0658]`, tracking issue 44930), and on AAPCS64 there is
//! no non-variadic declaration that can reach the arguments either, because
//! they are all passed on the stack. Five of the seven Tk calls only to build
//! text a script would read, and ignoring their variadic arguments costs the
//! text and nothing else — `eval` argues that one slot at a time. The other two
//! carry a payload and go through `src/tk/trampoline.c`, a C file compiled by
//! `build.rs`:
//!
//! * `Tcl_AppendStringsToObj` (slot 15), because Tk builds a fully qualified
//!   command name out of its arguments (`tk9.0.4/generic/tkUtil.c:1222`) and a
//!   body that ignored them registers every ensemble subcommand under the
//!   ensemble's own name;
//! * `Tcl_Panic` (slot 2), because it never returns and the formatted message
//!   is the only account of why.
//!
//! `TCLRS_TK_DEGRADED` still installs the truncating body phase 1 used for slot
//! 15, so the run that motivated the trampoline can be reproduced.
//!
//! # What the hosting table reaches
//!
//! `cargo run --features tk --bin tk-host` against the same library, with the
//! object layer, the evaluator and the notifier all behind the table: **2726
//! calls over 71 distinct slots**, and `Tk_Init` *returns* — it does not stop
//! on a missing slot at any point. 142 of the 691 `TclStubs` slots have bodies.
//!
//! On the way it evaluates `file tildeexpand ~/.Xdefaults` in a second
//! interpreter created and deleted for the purpose
//! (`tk9.0.4/generic/tkOption.c:1496-1499`), builds every `::tk::…` ensemble
//! subcommand name through the trampoline, registers 106 commands including the
//! main window command `.` and the whole widget set, creates the main window,
//! runs `TkpInit` — which instantiates `NSApplication` and opens the connection
//! to the window server — and initialises Ttk.
//!
//! It returns `TCL_ERROR`, and the reason is not on the Tk side of the
//! boundary. `Tk_Init`'s last statement evaluates a script that defines a
//! procedure inside an `if`:
//!
//! ```text
//! if {[namespace which -command tkInit] eq ""} {
//!   proc tkInit {} { ... rename tkInit {} ... tcl_findLibrary tk ... }
//! }
//! tkInit
//! ```
//!
//! (`tk9.0.4/generic/tkWindow.c:3508-3516`), and `Tk_Init` returns whatever
//! that evaluation returns (`:3518`, `:3536`).
//!
//! That script *compiles* now. This crate used to refuse a `proc` that is not
//! at the top level of a script, because a procedure registered while compiling
//! would exist whether or not the branch defining it is ever reached; the
//! run-time command table in [`crate::procs`] removed the reason for the
//! refusal, so the `proc tkInit` is lowered like any other and binds its name
//! when the branch runs.
//!
//! What stops the evaluation now is the *first command the script runs*:
//! `namespace`, in the condition of that same `if`. Three of the script's
//! commands are absent from this frontend — `namespace`, `rename` and
//! `tcl_findLibrary` — and behind them is `tk.tcl` itself, which is what
//! `tkInit` exists to `source`. That library is where Tk's class bindings live,
//! so `bind Button` is empty in this host and a mouse click on a button reaches
//! nothing. The gap between here and a `TCL_OK` is that much more of the Tcl
//! language, not more of the Tk ABI. Measured: with those three commands
//! supplied as procedures, the script above runs to completion and its output
//! matches tclsh 9.0.4 byte for byte.
//!
//! The call and slot counts did not move when the refusal did. The whole
//! failure is on this side of the stub table, so Tk asked for exactly what it
//! asked for before — 2726 calls over 71 slots, either way.
//!
//! # What works anyway
//!
//! Everything `Tk_Init` built before that last statement is live, and a script
//! this crate compiles can drive it through [`dispatch`]. Measured with
//! `tk-host`, whose remaining arguments are scripts:
//!
//! * `winfo exists .` → `1`, `winfo class .` → `Tk`, `wm geometry .` →
//!   `200x200+5+38`.
//! * `button .b -text hello` → `.b`; `pack .b`; then `winfo ismapped .b` → `1`,
//!   `winfo viewable .b` → `1`, `wm state .` → `normal`, and `wm geometry .`
//!   → `65x28+5+38` — the toplevel resized to its content.
//! * `--events 200` spins `Tcl_DoOneEvent` 200 times through the ported
//!   notifier; 17 of those passes service an event and the process survives all
//!   of them.
//! * A window appears on screen. `CGWindowListCopyWindowInfo` with
//!   `kCGWindowListOptionOnScreenOnly` reports one window owned by the
//!   `tk-host` process, which is the window server's own account of what is
//!   being displayed.
//! * `button .b -command {puts CALLBACK-FIRED}` followed by `.b invoke` prints
//!   `CALLBACK-FIRED`: Tk evaluated the callback back through
//!   `Tcl_EvalObjEx`, this crate compiled it, and fusevm ran it. A *click* does
//!   not reach it, for the `bind Button` reason above.

pub mod abi;
pub mod dispatch;
pub mod dstring;
pub mod eval;
pub mod generated;
pub mod hash;
pub mod host;
pub mod index;
pub mod interp;
pub mod load;
pub mod notifier;
pub mod obj;
pub mod objtype;
pub mod pkg;
pub mod preserve;
pub mod trace;
pub mod utf16;

pub use abi::RawStub;
