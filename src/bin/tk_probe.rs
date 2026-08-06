//! Hand the real Tk a stand-in interpreter and record what it asks for.
//!
//! Every line of output is one call Tk made through the stub table:
//!
//! ```text
//! tkslot <n> <table> <index> <name>   a call that was served
//! tktrap <n> <table> <index> <name>   the call that had no implementation
//! ```
//!
//! A `tktrap` line is followed by `abort`, so a run ends either at the first
//! unimplemented slot or when `Tk_Init` returns. Either way the preceding
//! `tkslot` lines are the ordered call sequence, which is the deliverable.
//!
//! Built only with `--features tk`.

use tclrs::tk::{host, load, trace};

fn main() {
    let lib = match load::Libtk::open() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tk-probe: {e}");
            std::process::exit(2);
        }
    };
    println!("tkprobe library {}", lib.path);

    let impls = host::implemented();
    println!(
        "tkprobe implemented {} of {} TclStubs slots",
        impls.len(),
        tclrs::tk::abi::TCL_STUBS_SLOTS
    );
    for (i, name) in &impls {
        println!("tkprobe impl {i} {name}");
    }

    let interp = host::build();
    println!("tkprobe calling Tk_Init");

    let rc = unsafe { load::call_tk_init(&lib, interp) };
    match rc {
        Ok(code) => println!(
            "tkprobe Tk_Init returned {code} after {} served calls",
            trace::served()
        ),
        Err(e) => {
            eprintln!("tk-probe: {e}");
            std::process::exit(2);
        }
    }
}
