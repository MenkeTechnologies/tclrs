//! Calling a command Tk registered, from a script tclrs compiled.
//!
//! # The problem this solves
//!
//! tclrs resolves a command name while compiling. `Compiler::dispatch` matches
//! the name against the builtins, the procedures the script defined, the
//! coroutines it created and the functions an inline `rust { … }` block
//! exported, and a name that matches none of them is `invalid command name`.
//! That is the whole reason a Tcl script lowers to straight-line bytecode with
//! no dispatch table in it, and it is what keeps a counted loop inside fusevm's
//! tracing JIT.
//!
//! Tk does not fit that. `button`, `pack`, `wm`, `canvas`, `bind` and the rest
//! are registered with `Tcl_CreateObjCommand` *while `Tk_Init` runs*
//! (`tk9.0.4/generic/tkWindow.c:1004-1096`), which is long after this crate
//! finished compiling whatever script asked for Tk. No amount of compile-time
//! knowledge can name them.
//!
//! # The shape of the answer
//!
//! One extension op, [`crate::compiler::ext::TK_DISPATCH`], and it is emitted
//! under exactly one condition: the name matched nothing at compile time *and*
//! a Tk interpreter exists in this process. Both halves matter.
//!
//! * A process that never loaded Tk never emits it, so every script that
//!   compiled before this existed compiles to the same bytecode now —
//!   including `bench/counted_loop_proc.tcl`, whose trace eligibility is what
//!   the tiers report measures.
//! * A name that *is* a builtin never reaches it, so no builtin becomes
//!   dynamic and no hot loop grows an extension op it did not have.
//!
//! When the op runs and finds nothing registered under the name, it raises the
//! error the compiler would have deferred — same wording, same script line —
//! which is why the fallback is not a second, weaker diagnosis.
//!
//! # What "calling a C command" means
//!
//! A `Tcl_ObjCmdProc` is
//! `int (*)(void *clientData, Tcl_Interp *, int objc, Tcl_Obj *const *objv)`
//! (`generic/tcl.h:587-588`); the `2` variant differs only in taking `objc` as
//! a `Tcl_Size` (`generic/tcl.h:590-591`), which on this platform is
//! `ptrdiff_t` (`generic/tcl.h:332`). By Tcl's convention `objv[0]` is the
//! command name as it was invoked and `objc` counts it, so a command with two
//! arguments is called with `objc == 3`.
//!
//! The values are built as `Tcl_Obj`s of this host's own making, retained
//! across the call so a command that keeps one keeps something live, and
//! released afterwards. The command's answer is its return code plus whatever
//! it left in the interpreter result, which is read back out and becomes the
//! value of the Tcl command that called it.

use std::ffi::{c_int, c_void};

use fusevm::{Op, Value, VM};

use super::abi::{TclObj, TCL_OK};
use super::host;
use super::interp;
use crate::compiler::{ext, CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, TclError};

/// Whether a Tk interpreter exists in this process at all.
///
/// The compile-time half of the condition above. False in every build without
/// the feature, and false in a `--features tk` build until something has
/// actually created a host.
pub fn may_exist() -> bool {
    interp::any_exists()
}

/// Whether this name should be lowered as a run-time lookup rather than handed
/// to the list commands, which is where the unknown-command error is raised.
///
/// The list commands are asked by name rather than by trying them and catching
/// the refusal, because a refusal is not always "unknown": `llength` with three
/// arguments refuses too, and that one has to stay a `wrong # args` on
/// `llength`, not become a lookup for a Tk command called `llength`.
/// `cmd_list::COMMANDS` is exactly the set `cmd_list::compile` accepts —
/// `names::tests::every_offered_command_is_known_to_the_compiler` is what keeps
/// that true.
pub fn takes_over(name: &str) -> bool {
    may_exist() && !crate::cmd_list::COMMANDS.contains(&name)
}

/// `int (*)(void *clientData, Tcl_Interp *, int objc, Tcl_Obj *const *objv)` —
/// `generic/tcl.h:587-588`.
type ObjCmdProc =
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *const *mut TclObj) -> c_int;

/// `int (*)(void *clientData, Tcl_Interp *, Tcl_Size objc, Tcl_Obj *const
/// *objv)` — `generic/tcl.h:590-591`. `Tcl_Size` is `ptrdiff_t`
/// (`generic/tcl.h:332`), so the third argument is wider than the `1` variant's
/// and the two signatures are *not* interchangeable: calling a `2` procedure
/// through the `1` type leaves the upper half of `objc` undefined.
type ObjCmdProc2 =
    unsafe extern "C" fn(*mut c_void, *mut c_void, isize, *const *mut TclObj) -> c_int;

/// Call the registered command `name` with `words` as `objv`, returning its
/// completion code. The interpreter result is left as the command left it.
///
/// # Safety
/// `interp_ptr` is a `Tcl_Interp *` this crate handed to Tk, and every entry of
/// `words` is a live `Tcl_Obj`.
pub unsafe fn invoke_objv(
    interp_ptr: *mut c_void,
    name: &str,
    words: &[*mut TclObj],
) -> Result<c_int, String> {
    let host_ptr = interp::host_of(interp_ptr);
    let Some(cmd) = host::command_named(host_ptr, name) else {
        return Err(format!("invalid command name \"{name}\""));
    };
    if cmd.proc_.is_null() {
        // An ensemble created by `Tcl_CreateEnsemble` has no procedure of its
        // own: Tcl resolves the subcommand through the mapping dictionary and
        // calls *that* command (`generic/tclEnsemble.c`). Nothing here does
        // that yet, and inventing an answer would be worse than saying so.
        return Err(format!(
            "command \"{name}\" is an ensemble, which this host does not dispatch yet"
        ));
    }
    // Tcl clears the result before every command it invokes
    // (`generic/tclBasic.c:4153-4158`, `TclInterpReady`), which is what lets a
    // command that succeeds without setting one produce the empty string rather
    // than whatever the previous command left behind.
    host::set_result_bytes(interp_ptr, b"");
    let objc = words.len();
    let objv = words.as_ptr();
    Ok(if cmd.proc2 {
        let f: ObjCmdProc2 = std::mem::transmute(cmd.proc_);
        f(cmd.client_data, interp_ptr, objc as isize, objv)
    } else {
        let f: ObjCmdProc = std::mem::transmute(cmd.proc_);
        f(cmd.client_data, interp_ptr, objc as c_int, objv)
    })
}

/// Call the registered command `name` with tclrs values, and give back what it
/// left as the interpreter result.
///
/// The words are `Tcl_Obj`s built here: retained before the call because a
/// command may keep one (`Tcl_IncrRefCount` is a macro over `refCount`,
/// `generic/tcl.h:2517-2519`, so a command that keeps one has already written
/// to it by the time this returns), released after.
fn call(name: &str, args: &[Value]) -> Result<String, String> {
    let interp_ptr = interp::current() as *mut c_void;
    if interp_ptr.is_null() {
        return Err(format!("invalid command name \"{name}\""));
    }
    unsafe {
        let mut words = Vec::with_capacity(args.len() + 1);
        words.push(host::retained_obj(name.as_bytes()));
        for a in args {
            words.push(host::retained_obj(to_tcl_string(a).as_bytes()));
        }
        let outcome = invoke_objv(interp_ptr, name, &words);
        for w in words {
            host::release_obj(w);
        }
        let result = String::from_utf8_lossy(&host::result_bytes(interp_ptr)).into_owned();
        match outcome? {
            TCL_OK => Ok(result),
            _ => Err(result),
        }
    }
}

/// [`ext::TK_DISPATCH`]: the operands are the script line, the command name and
/// then the arguments, in the order the compiler pushed them.
///
/// The line rides on the stack because the failure this op can produce is the
/// one the compiler used to defer, and that one is *located* — dropping the
/// line would turn `(file "x.tcl" line 7)` into a message with no place, which
/// a differential test against tclsh would notice.
pub fn extension(vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    let line = match values.first() {
        Some(Value::Int(n)) => *n as usize,
        _ => 0,
    };
    let name = to_tcl_string(&values[1]);
    match call(&name, &values[2..]) {
        Ok(result) => {
            vm.push(Value::Str(std::sync::Arc::new(result)));
            Ok(())
        }
        Err(msg) => Err(TclError {
            msg,
            line: Some(line),
        }),
    }
}

impl Compiler {
    /// A call to a command that may be registered by the time this runs: the
    /// line, the name, then the arguments, then the op that pops all of them.
    ///
    /// The arguments are lowered exactly as any other command's are, so a
    /// command substitution inside one still runs before the dispatch — and
    /// still runs even when the dispatch then fails, which is the order Tcl
    /// substitutes in and the order `Compiler::defer` already preserved.
    pub(crate) fn call_foreign(&mut self, name: &str, args: &[Word]) -> Result<(), CompileError> {
        let count = u8::try_from(args.len() + 2)
            .map_err(|_| self.err(format!("more than 253 arguments to the command \"{name}\"")))?;
        self.push_value(Value::Int(self.command_line as i64));
        self.push_str(name);
        for arg in args {
            self.word(arg)?;
        }
        // The op consumes the line, the name and every argument, and leaves the
        // command's value: `count` off the stack for one on. The two operands
        // the compiler synthesised are part of that count, so the depth this
        // reports is one deeper than a call with the name alone would be.
        self.emit(Op::Extended(ext::TK_DISPATCH, count), 1 - count as i32);
        Ok(())
    }
}
