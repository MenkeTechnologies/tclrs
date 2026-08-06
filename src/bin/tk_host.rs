//! Hand the real Tk an interpreter that can evaluate, and see how far it gets.
//!
//! The difference from `tk-probe` is five slots — the C trampoline and the
//! three `Tcl_Eval*` bodies — and that difference is the whole of phase 2.
//! `tk-probe` keeps measuring the table with no evaluator behind it, because
//! its stopping point is a pinned measurement; this measures the one with.
//!
//! Output is the probe's format, so the two runs can be diffed line for line:
//!
//! ```text
//! tkslot <n> <table> <index> <name>   a call that was served
//! tktrap <n> <table> <index> <name>   the call that had no implementation
//! ```
//!
//! After `Tk_Init` returns — if it returns — the run prints its completion
//! code, the interpreter result, and every command Tk registered, since a
//! registered command is what a compiled tclrs script can now call.
//!
//! Built only with `--features tk`.

use tclrs::tk::{host, interp, load, trace};

fn main() {
    let lib = match load::Libtk::open() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tk-host: {e}");
            std::process::exit(2);
        }
    };
    println!("tkhost library {}", lib.path);

    let impls = host::implemented_at(host::Level::Hosting);
    println!(
        "tkhost implemented {} of {} TclStubs slots",
        impls.len(),
        tclrs::tk::abi::TCL_STUBS_SLOTS
    );

    let tk_interp = host::build_hosting();
    // The same library discovery a `tclrs --tk` session performs
    // (`tclrs::tk::session::load_tk`), so this probe measures the search that
    // ships rather than one only it does.
    let root = lib.library_root();
    if let Some(root) = &root {
        let added = unsafe { load::seed_library_path(tk_interp as *mut std::ffi::c_void, root) };
        println!("tkhost dylib root {root}");
        for dir in added {
            println!("tkhost library path {dir}");
        }
    }
    println!("tkhost calling Tk_Init");

    let rc = unsafe { load::call_tk_init(&lib, tk_interp) };
    let code = match rc {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tk-host: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "tkhost Tk_Init returned {code} after {} served calls",
        trace::served()
    );
    let result = unsafe { host::result_bytes(tk_interp as *mut std::ffi::c_void) };
    println!("tkhost result {:?}", String::from_utf8_lossy(&result));

    // Every name Tk claimed. A script compiled from here on can call any of
    // them, because `tk::dispatch` resolves an unknown name against this table
    // when the command runs.
    let host_ptr = unsafe { interp::host_of(tk_interp as *mut std::ffi::c_void) };
    let commands = unsafe { &(*host_ptr).commands };
    println!("tkhost commands {}", commands.len());
    for c in commands {
        println!("tkhost command {}", c.name);
    }

    // The point of the exercise: a Tcl script this crate compiled, calling a
    // command that did not exist when the compiler started.
    //
    // `--events N` is not a script: it spins the ported notifier N times, so a
    // window that was just created gets the passes it needs to be laid out and
    // painted. Anything else is compiled and run as Tcl.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--events" {
            let n: usize = args
                .next()
                .and_then(|s| s.parse().ok())
                .expect("--events needs a count");
            let mut serviced = 0usize;
            for _ in 0..n {
                serviced += (unsafe {
                    tclrs::tk::notifier::do_one_event_slot(
                        tclrs::tk::notifier::TCL_ALL_EVENTS | tclrs::tk::notifier::TCL_DONT_WAIT,
                    )
                } != 0) as usize;
            }
            println!("tkhost events {n} passes, {serviced} serviced, process alive");
            continue;
        }
        // Through the host's own evaluator rather than a fresh `Interp`, so
        // every argument runs against the same variables Tk reads and writes.
        // A `-textvariable` cannot be demonstrated any other way: the widget
        // watches a variable of *this* interpreter, and a script evaluated
        // against a different one would set something nothing is watching.
        let code =
            unsafe { tclrs::tk::eval::eval_script(tk_interp as *mut std::ffi::c_void, &arg) };
        let result = unsafe { host::result_bytes(tk_interp as *mut std::ffi::c_void) };
        let text = String::from_utf8_lossy(&result);
        if code == tclrs::tk::abi::TCL_OK {
            println!("tkhost eval {arg:?} -> {text:?}");
        } else {
            println!("tkhost eval {arg:?} !! {text}");
        }
    }
    let (traces, links) = tclrs::tk::linkvar::counts();
    println!("tkhost variable traces {traces}, linked variables {links}");
}
