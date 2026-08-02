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
//! * [`channel`] — the channel slots, and the `Tcl_ChannelType` driver table
//!   Tk supplies and this side calls *into*.
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
//! * [`session`] — the product binary's entry points: what `tclrs --tk` opens
//!   before the script is compiled, what `package require Tk` does inside it,
//!   and the Tk main loop the application sits in afterwards.
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
//! # Four things the stub table does not cover
//!
//! Reading the header's function list suggests Tk can be satisfied by supplying
//! 691 functions. It cannot. Four data structures are shared by *layout*.
//! Tk operates on three of them with macros that never reach the table, and
//! hands the fourth over for the host to call back through:
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
//! 3. **`Tcl_ChannelType`.** A driver is a table of Tk's own function pointers
//!    (`generic/tcl.h:1445-1494`) handed to `Tcl_CreateChannel`, and the host
//!    calls *into* it on every read, write and close — the only place the
//!    boundary runs that way round. Tk's console is one
//!    (`tk9.0.4/generic/tkConsole.c:66-84`). See [`channel`].
//! 4. **`Tcl_DString`, `Tcl_CmdInfo`, `Tcl_Time`, `Tcl_Namespace`,
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
//! on a missing slot at any point. 189 of the 691 `TclStubs` slots have bodies.
//!
//! That measurement is of the run whose **stdin is a pipe**, and stdin decides
//! which of two branches `TkpInit` takes. With stdin on `/dev/null` — a
//! character device with no blocks, which is what a test harness gives a
//! process — Tk opens a console instead
//! (`tk9.0.4/macosx/tkMacOSXInit.c:493-494`, `:585-598`), and that branch is a
//! different measurement: **2666 calls over 72 distinct slots**, stopping at
//! `Tcl_Init` on the second interpreter `Tk_CreateConsoleWindow` creates
//! (`tk9.0.4/generic/tkConsole.c:344-345`). Before [`channel`] existed it
//! stopped 27 calls earlier, at `Tcl_CreateChannel`. Both are pinned:
//! `tests/tk_utf16_window.rs` runs the pipe branch and
//! `tests/tk_console_channels.rs` the console one.
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
//! That script *runs* now, to its last statement. Four separate refusals stood
//! in its way and each has gone:
//!
//! * a `proc` that is not at a script's top level, which
//!   [`crate::procs`]' run-time command table made lowerable;
//! * `namespace`, in the condition of that same `if`
//!   ([`crate::cmd_namespace`]);
//! * `rename`, which the same module supplies;
//! * `tk_version` and `tk_patchLevel`, which `global` reads inside `tkInit` —
//!   Tk writes them through `Tcl_SetVar2` (`:1066-1067`) and [`linkvar`]
//!   bridges the host's table to the interpreter's globals.
//!
//! What stops it now is the statement those four were in the way of:
//! `tcl_findLibrary tk $tk_version $tk_patchLevel tk.tcl TK_LIBRARY tk_library`
//! (`:3513`). The search runs, and with `TK_LIBRARY` pointing at an installed
//! Tk it finds `tk.tcl` and reads it — and then cannot compile it. `tk.tcl`
//! uses `{*}` argument expansion in eleven places, and this frontend refuses
//! one (`crate::compiler`, `{*} argument expansion is not supported yet`),
//! because an expanded word decides an argument count when the command runs
//! and every call site here is resolved while the script is read.
//!
//! `tk.tcl` is where Tk's class bindings live, so `bind Button` is empty in
//! this host and a mouse click on a button reaches nothing. The gap between
//! here and a `TCL_OK` is one Tcl language feature, not more of the Tk ABI.
//!
//! The call and slot counts did not move as the refusal walked forward. The
//! whole failure is on this side of the stub table, so Tk asked for exactly
//! what it asked for before — 2726 calls over 71 slots, every time.
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
//! * `label .l -textvariable v -text initial`, then `set v hello`, then
//!   `.l cget -text` → `hello`. The widget option is a variable trace
//!   ([`linkvar`]); the answer is read back out of the widget by a real Tk
//!   command rather than assumed.
//! * `checkbutton .c -variable cv` creates `cv` at `0`; `.c select` makes it
//!   `1` and `.c deselect` makes it `0` again, so the variable follows the
//!   widget as well as the widget following the variable.
//! * `set tk_strictMotif 1` writes the C `int` behind it
//!   (`tk9.0.4/generic/tkWindow.c:900`), and reading the variable back reads
//!   the C storage through the link's read trace.
//!
//! Those three need a terminal. `TkpInit` opens a console window when stdin is
//! not a tty and there is no startup script
//! (`tk9.0.4/macosx/tkMacOSXInit.c:583-606`); with [`channel`] behind it that
//! branch now reaches `Tcl_Init` on the console interpreter rather than
//! stopping at `Tcl_CreateChannel`. Run `tk-host` under a pty
//! (`script -q /dev/null …`) for the numbers above.
//!
//! # The same thing, from the product binary
//!
//! [`session`] is the sequence above with the script in charge of it, and
//! `tclrs --tk app.tcl` is where it runs. Measured on this tree, against the
//! same library:
//!
//! ```text
//! package require Tk            → 9.0.4
//! button .b -text hello         → .b
//! pack .b
//! .b invoke                     → the -command body runs, in the script's
//!                                 own interpreter
//! ```
//!
//! `Tk_Init` still returns `TCL_ERROR` there, for the reason above and after
//! the same 2726 served calls, and `package require Tk` still answers `9.0.4`,
//! because Tk provided itself as a package (`:3461-3469`) several hundred calls
//! before it reached the statement that failed. What decides whether the
//! package is present is the registry, not the completion code.
//!
//! One thing does differ between the two hosts, and it is the variable bridge
//! showing through. `tk-host` sets no `argv0`, so `TkpGetAppName` falls back to
//! its literal `"tk"` (`tk9.0.4/macosx/tkMacOSXInit.c:789-797`) and
//! `winfo class .` is `Tk`. `tclrs --tk app.tcl` sets `argv0` the way `tclsh`
//! does, so the name is the script's file name and the class is
//! `Tcl_UtfToTitle` of it (`generic/tkWindow.c:3363-3375`) — `App.tcl`. The
//! reference interpreter answers the same way for the same input; it is Tk's
//! rule, not this host's.
//!
//! The same `argv0` is why `Tcl_ParseArgsObjv` (slot 667) has a body at all:
//! `Tk_Init` skips its whole argument block when `argv` cannot be read
//! (`:3312-3341`), and once the bridge makes it readable, that block runs.
//!
//! The window is the window server's account and not Tk's: with the script
//! sitting in `Tk_MainLoop`, `CGWindowListCopyWindowInfo` reports
//! `pid=43113 owner="tclrs" bounds=67x60+5+38` against that live process — a
//! toplevel resized to the button it contains, which is layout the main loop
//! ran. The same process had just evaluated 999 levels of nested `eval`
//! (226 MB resident), so the borrowed 256 MiB stack in `src/main_thread.rs` and
//! Tk's main-thread requirement hold at the same time and in the same process.

pub mod abi;
pub mod channel;
pub mod dispatch;
pub mod dstring;
pub mod eval;
pub mod generated;
pub mod hash;
pub mod host;
pub mod index;
pub mod interp;
pub mod linkvar;
pub mod load;
pub mod notifier;
pub mod obj;
pub mod objtype;
pub mod pkg;
pub mod preserve;
pub mod session;
pub mod trace;
pub mod utf16;

pub use abi::RawStub;
