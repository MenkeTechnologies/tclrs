//! The recorder that turns "Tk crashed somewhere" into "Tk called slot N".
//!
//! Every slot of every stub table starts out pointing at a trap that knows its
//! own table and index. A trap prints its identity and aborts, so the first
//! call to an unimplemented slot names itself instead of jumping through
//! uninitialised memory. Slots that do have an implementation announce
//! themselves through [`record`] and then carry on, which is what makes the
//! output an ordered call log rather than only a list of failures.
//!
//! Output goes to stderr, one line per call, in a fixed format:
//!
//! ```text
//! tkslot <table> <index> <name>
//! tktrap <table> <index> <name>
//! ```
//!
//! `tkslot` is a call that was served; `tktrap` is the call that ended the run.
//! stderr rather than a buffered writer because the process aborts immediately
//! afterwards and a buffer would not survive it.
//!
//! The served-call half is a *probe's* instrument, and a session opened by
//! `tclrs --tk` turns it off ([`set_logging`]) because an application's stderr
//! is for the application's errors. `TCLRS_TK_TRACE` keeps it, so the same
//! measurement can be taken through the product binary.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Which of the four stub tables a slot belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Table {
    Tcl,
    TclInt,
    TclPlat,
    TclIntPlat,
}

impl Table {
    pub fn as_str(self) -> &'static str {
        match self {
            Table::Tcl => "Tcl",
            Table::TclInt => "TclInt",
            Table::TclPlat => "TclPlat",
            Table::TclIntPlat => "TclIntPlat",
        }
    }

    /// The slot names of this table, in declaration order.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Table::Tcl => &super::generated::TCL_NAMES,
            Table::TclInt => &super::generated::TCL_INT_NAMES,
            Table::TclPlat => &super::generated::TCL_PLAT_NAMES,
            Table::TclIntPlat => &super::generated::TCL_INT_PLAT_NAMES,
        }
    }

    pub fn name_of(self, slot: usize) -> &'static str {
        self.names().get(slot).copied().unwrap_or("<out of range>")
    }
}

/// Calls served so far. Printed by a trap so the abort message says how far in
/// the run got, which is the only ordering information that survives when the
/// log is long.
static SERVED: AtomicU64 = AtomicU64::new(0);

/// Whether a served call prints a line.
///
/// On, because the log is the instrument this whole subtree was built to read
/// and every binary and test that existed before this switch is a measurement.
/// The one caller that turns it off is [`super::session::open`]: `tclrs --tk`
/// is an application session rather than a probe, and 2726 lines of call log on
/// its stderr would be output the script did not ask for. `TCLRS_TK_TRACE`
/// takes the measurement through the product binary anyway.
///
/// A trap is never silenced. It is the only account of why the process is about
/// to stop.
static LOGGING: AtomicBool = AtomicBool::new(true);

/// Turn the served-call log off, or back on.
pub fn set_logging(on: bool) {
    LOGGING.store(on, Ordering::Relaxed);
}

/// Distinct slots reached, as `(table, slot)`. The headline number of the whole
/// exercise is "how much of a 691-slot table does Tk actually touch", so it is
/// counted as the run goes rather than reconstructed from the log afterwards.
static TOUCHED: Mutex<BTreeSet<(u8, u16)>> = Mutex::new(BTreeSet::new());

fn touch(table: Table, slot: usize) -> usize {
    let mut set = TOUCHED.lock().expect("trace set poisoned");
    set.insert((table as u8, slot as u16));
    set.len()
}

/// Note a call to a slot that has an implementation.
///
/// Every implementation calls this on entry, before doing anything else, so the
/// log is in call order even when one implementation calls back into another.
pub fn record(table: Table, slot: usize) {
    let n = SERVED.fetch_add(1, Ordering::Relaxed);
    touch(table, slot);
    if !LOGGING.load(Ordering::Relaxed) {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "tkslot {} {} {} {}",
        n,
        table.as_str(),
        slot,
        table.name_of(slot)
    );
}

/// Note a call to a slot that has no implementation, and stop.
///
/// Aborting rather than returning is deliberate. Returning a plausible-looking
/// zero from a slot whose contract is "a live `Tcl_Obj *`" would turn a precise
/// answer — Tk wants this function next — into a crash several frames later
/// with no way back to the cause.
pub fn unimplemented(table: Table, slot: usize) -> ! {
    let n = SERVED.load(Ordering::Relaxed);
    let distinct = touch(table, slot);
    {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "tktrap {} {} {} {}",
            n,
            table.as_str(),
            slot,
            table.name_of(slot)
        );
        let _ = writeln!(
            err,
            "tkdone {n} calls, {distinct} distinct slots, stopped at {} slot {slot} {}",
            table.as_str(),
            table.name_of(slot)
        );
        let _ = err.flush();
    }
    std::process::abort()
}

/// How many calls have been served. Used by the probe's summary line.
pub fn served() -> u64 {
    SERVED.load(Ordering::Relaxed)
}

/// Note a string that passed through a slot, when `TCLRS_TK_STRINGS` is set.
///
/// The call log says which slots Tk used; this says what it used them on, which
/// is the only way to read an error message Tk built and then handed back
/// through `Tcl_SetObjResult` without an interpreter to print it.
pub fn note(what: &str, text: &str) {
    if std::env::var_os("TCLRS_TK_STRINGS").is_none() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "tktext {what} {text:?}");
}
