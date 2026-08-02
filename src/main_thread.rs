//! Running the interpreter on the main thread, on a stack big enough for it.
//!
//! # The conflict
//!
//! Two requirements meet head-on in a Tk session, and neither of them bends.
//!
//! **Tk needs the main thread.** `Tk_MacOSXSetupTkNotifier` installs the Aqua
//! event source — the thing that turns a mouse click into a `Tcl_Event` — only
//! when the current run loop is the process's main run loop, and calls
//! `Tcl_Panic("first [load] of TkAqua has to occur in the main thread!")` if
//! that is true on a thread AppKit does not consider the main one
//! (`tk9.0.4/macosx/tkMacOSXNotify.c:258-272`). The two
//! `Tcl_MacOSXNotifierAddRunLoopMode` calls that keep events flowing during a
//! menu drag and a modal dialog are inside the same branch (`:270-271`). On any
//! other thread Tk initialises without complaint and no window event ever
//! arrives.
//!
//! **The interpreter needs a stack.** A nested `eval` runs a VM of its own, so
//! nesting costs native stack, and the interpreter's answer to a runaway script
//! is to refuse at the depth `tclsh` refuses at rather than overflow — because
//! an overflow is a signal, not an error a script can be blamed for. Measured
//! on this tree at the default limit of 1000 levels: **99 MiB** unoptimized (98
//! fails, 99 passes) and **14 MiB** optimized (12 fails, 14 passes).
//! `runtime::RECOMMENDED_STACK` is 256 MiB.
//!
//! macOS gives the main thread 8.0 MiB
//! (`pthread_get_stacksize_np(pthread_self())`, measured). That is not enough
//! for either profile.
//!
//! # What was rejected, and what it measured
//!
//! * **Raise `RLIMIT_STACK` and re-exec.** The soft limit is 8176 KiB and the
//!   hard limit is 65520 KiB (`ulimit -s`, `ulimit -Hs`); raising the soft
//!   limit to the hard one and `execv`-ing the binary gives the child a main
//!   thread of exactly 64.0 MiB (measured). That covers an optimized build with
//!   four times the room and misses an unoptimized one by 35 MiB, so the Tk
//!   path would work in release and die on a signal under `cargo test`. The
//!   64 MiB is a ceiling, not a setting: it is the hard limit.
//! * **`-Wl,-stack_size` at link time.** This does work — a binary linked with
//!   `-Wl,-stack_size,0x10000000` reports a 256.0 MiB main stack, and costs
//!   nothing resident (1328 KiB either way, measured with `task_info`). But the
//!   linker rejects the option for anything that is not a main executable
//!   (`ld: -stack_size option can only be used when linking a main
//!   executable`), so it cannot be a blanket `rustflags`: this tree links 13
//!   proc-macro crates, each of which is a dylib. Scoping it to binaries is
//!   `cargo::rustc-link-arg-bins` in `build.rs`, and `build.rs` belongs to
//!   another part of this work.
//! * **Keep the interpreter on the worker and drive the run loop from the main
//!   thread.** Tk's C would then run on the main thread while the interpreter's
//!   state lived on another, and Tk calls back into the interpreter from inside
//!   its own event dispatch. That is two threads sharing one interpreter, which
//!   Tcl does not do and this host would have to invent.
//!
//! # What this does instead
//!
//! Keeps the thread and changes its stack. The main thread runs the
//! interpreter on a region this module maps, by switching the stack pointer for
//! the duration of the call and switching it back afterwards. No thread is
//! created, so `pthread_self`, `[NSThread isMainThread]` and
//! `CFRunLoopGetMain() == CFRunLoopGetCurrent()` are all exactly as they were —
//! every one of them is a property of the thread, not of the stack it is
//! standing on.
//!
//! # What it costs
//!
//! * Two short pieces of architecture-specific assembly, one per calling
//!   convention this crate targets. On any other architecture there is no
//!   switch and [`run`] says so and exits rather than running the interpreter
//!   on a stack that cannot hold its own recursion limit.
//! * Rust's stack-overflow handler recognizes a fault in the guard page of a
//!   thread *it* created, and this stack is not one of those. A runaway on the
//!   borrowed stack still stops at a guard page — [`Stack`] maps one — but it
//!   is reported as a segmentation fault rather than as `fatal runtime error:
//!   stack overflow`. The recursion limit is what makes that unreachable, and
//!   is exactly why it may not be lowered.
//! * A panic may not unwind across the switch, so the payload runs inside
//!   `catch_unwind` and the failure is turned into an exit status on the
//!   original stack.
//! * 256 MiB of address space, and no resident memory until it is used.
//!
//! None of it applies without `--tk`: that path is the same `std::thread`
//! spawn it has always been.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::ExitCode;

/// A mapped region to run on, with a guard page under it.
///
/// The guard is what turns a runaway recursion into a fault at a known address
/// instead of a silent write into whatever the allocator put below the stack.
/// It is the same protection `std::thread` gives a thread it creates, arranged
/// by hand because this stack has no thread of its own.
struct Stack {
    base: *mut c_void,
    len: usize,
}

impl Stack {
    /// Map `size` bytes to run on, plus one page of guard beneath it.
    fn map(size: usize) -> Result<Stack, String> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let len = size.next_multiple_of(page) + page;
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(format!(
                "could not reserve {size} bytes of stack: {}",
                std::io::Error::last_os_error()
            ));
        }
        // The stack grows down, so the guard goes at the low end.
        if unsafe { libc::mprotect(base, page, libc::PROT_NONE) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::munmap(base, len) };
            return Err(format!("could not protect the stack guard page: {e}"));
        }
        Ok(Stack { base, len })
    }

    /// The address to set the stack pointer to: the top of the region, rounded
    /// down to the 16-byte alignment both calling conventions require.
    fn top(&self) -> *mut u8 {
        let end = self.base as usize + self.len;
        (end & !0xf) as *mut u8
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base, self.len) };
    }
}

/// Call `f(arg)` with the stack pointer set to `top`, then restore it.
///
/// Naked because the whole body is the switch: any prologue the compiler
/// generated would address locals through a stack pointer this changes
/// underneath it.
///
/// The frame pointer is saved on the *old* stack and used to find the way back,
/// so the callee's frames chain into the caller's and a backtrace taken inside
/// `f` still walks out through `main`.
///
/// # Safety
/// `top` must be 16-byte aligned, must be the high end of a writable region
/// large enough for everything `f` will do, and that region must outlive the
/// call.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn call_on_stack(
    f: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
    top: *mut u8,
) {
    // AAPCS64: x0/x1/x2 are the arguments, x29 the frame pointer, x30 the link
    // register, and SP must stay 16-byte aligned at all times.
    std::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!", // save the frame and link registers
        "mov x29, sp",               // remember where the old stack was
        "mov x9,  x0",               // f, out of the way of the argument shuffle
        "mov x0,  x1",               // arg becomes the first argument
        "mov sp,  x2",               // the switch
        "blr x9",
        "mov sp,  x29", // back
        "ldp x29, x30, [sp], #16",
        "ret",
    )
}

/// The x86-64 half of [`call_on_stack`]. See that function for the contract.
///
/// # Safety
/// As [`call_on_stack`].
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn call_on_stack(
    f: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
    top: *mut u8,
) {
    // System V AMD64: rdi/rsi/rdx are the arguments, and rsp must be 16-byte
    // aligned at the point of a `call`, which then pushes the return address.
    std::arch::naked_asm!(
        "push rbp",
        "mov  rbp, rsp",
        "mov  rsp, rdx", // the switch
        "mov  rax, rdi", // f
        "mov  rdi, rsi", // arg becomes the first argument
        "call rax",
        "mov  rsp, rbp", // back
        "pop  rbp",
        "ret",
    )
}

/// What crosses the switch: the function to run and the status it produced.
struct Payload {
    run: fn() -> ExitCode,
    outcome: Option<ExitCode>,
}

/// The far side of the switch. Runs on the mapped stack.
///
/// A panic may not unwind past this frame — the unwinder would be walking a
/// stack the runtime does not know about — so it is caught here and reported as
/// a failing exit status once control is back on the original stack.
unsafe extern "C" fn trampoline(arg: *mut c_void) {
    let payload = &mut *(arg as *mut Payload);
    let run = payload.run;
    payload.outcome = catch_unwind(AssertUnwindSafe(run)).ok();
}

/// Run `f` on this thread, on a stack of [`tclrs::runtime::RECOMMENDED_STACK`]
/// bytes.
///
/// The thread is the caller's — which for a `--tk` run is the main thread, and
/// that is the point. See the module documentation for why the stack has to be
/// borrowed rather than the thread replaced.
pub fn run(f: fn() -> ExitCode) -> ExitCode {
    if !cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) {
        eprintln!(
            "tclrs: --tk needs a stack switch, which is not written for this \
             architecture"
        );
        return ExitCode::FAILURE;
    }

    let stack = match Stack::map(tclrs::runtime::RECOMMENDED_STACK) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tclrs: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The one thing worth checking before Tk is handed anything: that this is
    // in fact the thread whose run loop is the main one. It always is when
    // `main` calls this, and saying so out loud is cheaper than debugging a
    // `Tcl_Panic` from inside libtk.
    if !tclrs::tk::notifier::on_main_run_loop() {
        eprintln!("tclrs: --tk must run on the main thread");
        return ExitCode::FAILURE;
    }

    let mut payload = Payload {
        run: f,
        outcome: None,
    };
    unsafe {
        call_on_stack(
            trampoline,
            &mut payload as *mut Payload as *mut c_void,
            stack.top(),
        );
    }
    payload.outcome.unwrap_or(ExitCode::FAILURE)
}
