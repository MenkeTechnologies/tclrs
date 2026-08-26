//! `after`, `update` and `vwait` — the event loop a Tk script lives inside.
//!
//! # Where the events are
//!
//! Tcl's `after` is two things at once: a *record* the interpreter keeps (the
//! script text, its id, and whether it is a timer or an idle handler) and a
//! *handler* registered with the notifier so that `Tcl_DoOneEvent` knows when to
//! wake up (`generic/tclTimer.c:776-975`). Only the first half can live in C:
//! the script is Tcl text and nothing but this frontend can run it.
//!
//! So the record lives here, in `Afters` on the interpreter, for every build.
//! What differs is what else can be pending:
//!
//! * A **default build** has no notifier — `src/tk/` is behind the `tk` feature
//!   and a machine with no Tk installed never compiles it. There are then no
//!   window events and no file events, and an `after` script or an `after idle`
//!   script is the *only* thing that can be pending. `update` drains exactly
//!   those, `vwait` blocks on exactly those, and both terminate for exactly the
//!   reason Tcl's do. Nothing is stubbed out and nothing silently does less: the
//!   set of event sources is smaller, and the loop over it is the same loop.
//! * A **`--features tk` build** also has the ported notifier
//!   (`crate::tk::notifier`), which is where Tk's own window events, file
//!   handlers and C-level idle handlers (`Tk_EventuallyRedraw` and the rest)
//!   arrive. `service_one` pumps it alongside the queue below.
//!
//! # Ordering
//!
//! `Tcl_DoOneEvent` services the event queue before the idle handlers and
//! returns as soon as it has serviced one thing (`generic/tclNotify.c:917-1061`,
//! reproduced in `crate::tk::notifier` and pinned by `tests/tk_notifier.rs`).
//! Measured against tclsh 9.0.4, an `after 0` script therefore runs before an
//! `after idle` script queued before it. `service_one` keeps that order: due
//! timers first, then the notifier, then the oldest idle handler.
//!
//! # What blocking costs
//!
//! In a default build the wait is exact: the only thing that can happen is a
//! timer coming due, so the loop sleeps until the earliest deadline, and with
//! none pending it answers rather than waiting.
//!
//! Under `tk` there are two cases. With **no timer pending**, the wait goes
//! into `Tcl_DoOneEvent` itself and inherits its answer exactly — including a
//! block with no timeout, which is what `vwait` on a variable nothing can write
//! does in tclsh on macOS. With **a timer pending**, the loop sleeps in
//! ten-millisecond slices and polls the notifier between them, because the
//! queue is not registered with the notifier as an event source of its own and
//! so cannot ask it for a wake-up. That is the one divergence from
//! `Tcl_DoOneEvent`'s single blocking wait, and it is a latency bound on
//! servicing a Tk event while an `after` is outstanding, not a change to what
//! is serviced or in what order.
//!
//! # What an `after` script cannot see
//!
//! `AfterProc` evaluates with `TCL_EVAL_GLOBAL` (`generic/tclTimer.c:1157`), so
//! an `after` script runs at the global level whatever was running when it was
//! registered. That is exactly what a nested chunk reaches here, so the script
//! is run through `crate::runtime::run_source` — the same door the `eval`
//! command goes through, and with the same write-back of the running chunk's
//! variables either side of it.

use std::time::{Duration, Instant};

use fusevm::{Op, Value, VM};

use crate::compiler::{CompileError, Compiler};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, Shared, TclError};

// ── compiling ────────────────────────────────────────────────────────────

impl Compiler {
    /// `after`, `update` and `vwait` all lower the same way: every word is
    /// pushed and one op decides the rest when the command runs.
    ///
    /// None of the three can be decided earlier. `after`'s first word may be a
    /// delay or a subcommand and the reference implementation tells them apart
    /// by trying the number first; `update`'s option and `vwait`'s variable name
    /// may both be computed; and every answer any of them gives depends on what
    /// is pending, which is a property of the moment.
    pub(crate) fn cmd_event_op(
        &mut self,
        id: u16,
        name: &str,
        args: &[Word],
    ) -> Result<(), CompileError> {
        let count = u8::try_from(args.len())
            .map_err(|_| self.err(format!("too many arguments for \"{name}\"")))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(id, count), 1 - args.len() as i32);
        Ok(())
    }
}

/// How long the loop sleeps in one go while something outside the queue could
/// become ready. Only reached under `--features tk`; a default build sleeps
/// until the next deadline instead.
#[cfg(feature = "tk")]
const POLL_SLICE: Duration = Duration::from_millis(10);

/// Which kinds of event a pass over the loop may service, in the notifier's own
/// bits so that the `tk` build can hand them straight to `Tcl_DoOneEvent`.
mod flags {
    pub const DONT_WAIT: i32 = 1 << 1;
    pub const WINDOW: i32 = 1 << 2;
    pub const FILE: i32 = 1 << 3;
    pub const TIMER: i32 = 1 << 4;
    pub const IDLE: i32 = 1 << 5;
    pub const ALL: i32 = WINDOW | FILE | TIMER | IDLE;
}

/// The interpreter's state, for the queue below. No lock is ever held across a
/// script's evaluation: running one re-enters the interpreter through the same
/// handle, and would deadlock on this.
fn state(interp: &Shared) -> std::sync::MutexGuard<'_, crate::runtime::State> {
    interp.lock().expect("interpreter lock")
}

/// One registered `after` script.
pub(crate) struct After {
    id: u64,
    /// The script, as `after` concatenated it.
    script: String,
    /// `None` for an idle handler, which has no deadline.
    due: Option<Instant>,
}

/// Every `after` script an interpreter has that has not yet run, newest first.
///
/// Tcl keeps this list on the *interpreter* — `Tcl_SetAssocData(interp,
/// "tclAfter", …)` (`generic/tclTimer.c:801-807`) — and so does this, in
/// [`crate::runtime::State`]. A process-wide list would have been simpler and
/// wrong in a way a test suite notices: `tests/` runs many programs in one
/// process where tclsh runs each in one of its own, and the ids `after` answers
/// with start at zero per interpreter.
#[derive(Default)]
pub(crate) struct Afters {
    queue: Vec<After>,
    /// The next id. Tcl's counter is per thread and starts at zero
    /// (`generic/tclTimer.c:849-850`).
    next_id: u64,
}

impl Afters {
    /// Add a script to the front of the queue and answer with its handle.
    fn register(&mut self, script: String, due: Option<Instant>) -> String {
        let id = self.next_id;
        self.next_id += 1;
        self.queue.insert(0, After { id, script, due });
        format!("after#{id}")
    }

    /// Every timer that is already due, removed from the queue and answered in
    /// the order they must run.
    ///
    /// One timer *event* runs all of them, not one: `TimerHandlerEventProc`
    /// (`generic/tclTimer.c:606-694`) loops over the handler list until it
    /// reaches one whose time has not come. Servicing them one per pass would
    /// diverge visibly — `after 0 {set ::n 1}; after 0 {set ::n 2}; vwait ::n`
    /// is 2 in tclsh 9.0.4 and would have been 1, because `vwait` re-tests its
    /// variable between passes.
    ///
    /// Two rules come with that loop, and both are here. The clock is read once
    /// before it starts, so a handler that becomes due *while* it runs waits for
    /// the next event; and a handler registered by one of these scripts is of a
    /// "newer generation" and waits too, which is what stops
    /// `after 0 {after 0 …}` from starving every other event source. `newest` is
    /// that generation bound.
    ///
    /// The order is by deadline, and by registration among handlers sharing one
    /// — the order `TclCreateAbsoluteTimerHandler` keeps the list in
    /// (`generic/tclTimer.c:249-281`), and ids are handed out in registration
    /// order.
    fn take_due_timers(&mut self, now: Instant) -> Vec<String> {
        let newest = self.next_id;
        let mut due: Vec<(Instant, u64, String)> = Vec::new();
        self.queue.retain(|a| match a.due {
            Some(at) if at <= now && a.id < newest => {
                due.push((at, a.id, a.script.clone()));
                false
            }
            _ => true,
        });
        due.sort_by_key(|(at, id, _)| (*at, *id));
        due.into_iter().map(|(_, _, script)| script).collect()
    }

    /// The oldest idle handler, removed from the queue. `Tcl_DoWhenIdle`
    /// appends to the tail of the idle list (`generic/tclTimer.c:568-590`) and
    /// `TclServiceIdle` runs from the head, so the oldest runs first — which is
    /// the *end* of this newest-first queue.
    fn take_idle(&mut self) -> Option<String> {
        let at = self.queue.iter().rposition(|a| a.due.is_none())?;
        Some(self.queue.remove(at).script)
    }

    /// When the earliest pending timer comes due.
    fn soonest(&self) -> Option<Instant> {
        self.queue.iter().filter_map(|a| a.due).min()
    }
}

// ── the `after` command ──────────────────────────────────────────────────

/// `after` (`ext::AFTER`): `[arg …]` with the count in the inline operand.
pub(crate) fn after_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let args = pop_args(vm, argc);
    let result = after(interp, &args).map_err(TclError::plain)?;
    vm.push(Value::Str(std::sync::Arc::new(result)));
    Ok(())
}

fn after(interp: &Shared, args: &[String]) -> Result<String, String> {
    let Some(first) = args.first() else {
        return Err("wrong # args: should be \"after option ?arg ...?\"".to_string());
    };
    // Tcl tries the argument as a number before it tries it as a subcommand
    // (`generic/tclTimer.c:815-829`), which is why `after 0` is a delay and not
    // an ambiguous prefix of anything.
    if let Ok(ms) = first.trim().parse::<i64>() {
        return after_delay(interp, ms.max(0), &args[1..]);
    }
    let Some(sub) = crate::cmd_string::resolve(first, &["cancel", "idle", "info"]) else {
        return Err(format!(
            "bad argument \"{first}\": must be cancel, idle, info, or an integer"
        ));
    };
    match sub {
        "cancel" => after_cancel(interp, &args[1..]),
        "idle" => after_idle(interp, &args[1..]),
        _ => after_info(interp, &args[1..]),
    }
}

/// `after ms` and `after ms script ?script …?`.
fn after_delay(interp: &Shared, ms: i64, scripts: &[String]) -> Result<String, String> {
    if scripts.is_empty() {
        // `AfterDelay` (`generic/tclTimer.c:990-1063`) sleeps; it services no
        // events, so nothing the interpreter holds can change under it and the
        // running chunk's variables do not have to be written back.
        //
        // Under `tk` the notifier's own `Tcl_Sleep` is used, because on macOS it
        // sleeps by running the CFRunLoop in the Tcl-events-only mode
        // (`macosx/tclMacOSXNotify.c`) rather than blocking the thread the GUI
        // needs.
        sleep(Duration::from_millis(ms as u64));
        return Ok(String::new());
    }
    let due = Instant::now() + Duration::from_millis(ms as u64);
    Ok(state(interp).afters.register(joined(scripts), Some(due)))
}

/// `after idle script ?script …?`.
fn after_idle(interp: &Shared, scripts: &[String]) -> Result<String, String> {
    if scripts.is_empty() {
        return Err("wrong # args: should be \"after idle script ?script ...?\"".to_string());
    }
    Ok(state(interp).afters.register(joined(scripts), None))
}

/// `after cancel id` or `after cancel script ?script …?`.
///
/// The script text is tried first and the id second, which is the order
/// `generic/tclTimer.c:882-909` searches in: a script whose text happens to
/// spell an id cancels by text.
fn after_cancel(interp: &Shared, rest: &[String]) -> Result<String, String> {
    if rest.is_empty() {
        return Err("wrong # args: should be \"after cancel id|command\"".to_string());
    }
    let text = joined(rest);
    let mut guard = state(interp);
    let afters = &mut guard.afters;
    let found = afters
        .queue
        .iter()
        .position(|a| a.script == text)
        .or_else(|| afters.queue.iter().position(|a| id_of(&text) == Some(a.id)));
    if let Some(at) = found {
        afters.queue.remove(at);
    }
    // Cancelling something that is not registered is not an error, as it is not
    // in tclsh: `after cancel nosuch` answers with the empty string.
    Ok(String::new())
}

/// `after info ?id?`.
fn after_info(interp: &Shared, rest: &[String]) -> Result<String, String> {
    let guard = state(interp);
    let afters = &guard.afters;
    match rest {
        [] => {
            let ids: Vec<String> = afters
                .queue
                .iter()
                .map(|a| format!("after#{}", a.id))
                .collect();
            Ok(crate::list::join(&ids))
        }
        [id] => {
            let found = id_of(id).and_then(|id| afters.queue.iter().find(|a| a.id == id));
            match found {
                Some(a) => {
                    let kind = if a.due.is_some() { "timer" } else { "idle" };
                    Ok(crate::list::join(&[a.script.clone(), kind.to_string()]))
                }
                None => Err(format!("event \"{id}\" doesn't exist")),
            }
        }
        _ => Err("wrong # args: should be \"after info ?id?\"".to_string()),
    }
}

/// The numeric part of an `after#N` handle.
fn id_of(text: &str) -> Option<u64> {
    text.strip_prefix("after#")?.parse().ok()
}

/// Several script arguments concatenate the way `concat` concatenates them
/// (`generic/tclTimer.c:844-848`); one is taken as it stands, so its own
/// spacing survives.
fn joined(scripts: &[String]) -> String {
    match scripts {
        [one] => one.clone(),
        many => crate::cmd_list::concat(many),
    }
}

// ── `update` and `vwait` ─────────────────────────────────────────────────

/// `update ?idletasks?` (`ext::UPDATE`).
///
/// `while (Tcl_DoOneEvent(flags) != 0)` with `TCL_ALL_EVENTS|TCL_DONT_WAIT`, or
/// `TCL_IDLE_EVENTS|TCL_DONT_WAIT` for `idletasks`
/// (`generic/tclEvent.c:1953-1999`).
pub(crate) fn update_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let args = pop_args(vm, argc);
    let mask = match args.as_slice() {
        [] => flags::ALL,
        [one] if one == "idletasks" => flags::IDLE,
        [one] => {
            return Err(TclError::plain(format!(
                "bad option \"{one}\": must be idletasks"
            )))
        }
        _ => {
            return Err(TclError::plain(
                "wrong # args: should be \"update ?idletasks?\"",
            ))
        }
    };
    while service_one(interp, vm, mask | flags::DONT_WAIT)? {}
    vm.push(empty());
    Ok(())
}

/// `vwait ?varName?` (`ext::VWAIT`).
///
/// The loop is `generic/tclEvent.c:1731-1753`: block in `Tcl_DoOneEvent` and
/// re-test the variable after every pass, because `Tcl_DoOneEvent` returns as
/// soon as it has serviced one thing. `vwait` with no argument is `update` in
/// Tcl 9 (`generic/tclEvent.c:1721-1728`), which is where this sends it.
///
/// **How the write is noticed.** Tcl puts a write trace on the variable. There
/// is no variable trace here, so the value is compared against the one the
/// variable held when the wait began: a write that stores what was already
/// there does not end the wait. Recorded in BUGS.md.
pub(crate) fn vwait_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let args = pop_args(vm, argc);
    let name = match args.as_slice() {
        [] => {
            // "vwait" with nothing to wait for is equivalent to "update".
            while service_one(interp, vm, flags::ALL | flags::DONT_WAIT)? {}
            vm.push(empty());
            return Ok(());
        }
        // The name is taken as it stands. `vwait` reads a *global*
        // (`TCL_GLOBAL_ONLY`, `generic/tclEvent.c:1604`), which is the only
        // table this frontend has, and `::x` and `x` are two names here rather
        // than one — a namespace-qualified name is not resolved anywhere in
        // this crate yet. Stripping the qualifier here would make `vwait` the
        // one command that resolves it, and then `vwait ::done` would watch a
        // variable that `set ::done 1` does not write.
        [name] => name.clone(),
        _ => {
            return Err(TclError::plain(
                "\"vwait\" takes at most one variable name in this phase",
            ))
        }
    };

    // The variable is read out of the interpreter, not the running chunk, so the
    // chunk's own writes have to be there before the first look — and an `after`
    // script's write has to reach the chunk when the wait ends. Both directions
    // are the write-back `eval` makes.
    crate::runtime::flush_globals(vm, interp);
    let before = crate::runtime::global_value(interp, &name);
    loop {
        if crate::runtime::global_value(interp, &name) != before {
            break;
        }
        if !service_one(interp, vm, flags::ALL)? {
            crate::runtime::reseed_globals(vm, interp);
            return Err(TclError::plain(
                "can't wait for variable(s)/channel(s): would wait forever",
            ));
        }
    }
    crate::runtime::reseed_globals(vm, interp);
    vm.push(empty());
    Ok(())
}

/// One pass of `Tcl_DoOneEvent`: service at most one thing and answer whether
/// anything was serviced. Blocks unless `TCL_DONT_WAIT` is in `mask`.
fn service_one(interp: &Shared, vm: &mut VM, mask: i32) -> Result<bool, TclError> {
    loop {
        if mask & flags::TIMER != 0 {
            // The lock is taken and given back before the scripts run: running
            // one re-enters the interpreter, which takes the same lock.
            let due = state(interp).afters.take_due_timers(Instant::now());
            if !due.is_empty() {
                for script in due {
                    run_script(interp, vm, &script);
                }
                return Ok(true);
            }
        }
        #[cfg(feature = "tk")]
        if unsafe { crate::tk::notifier::do_one_event_slot(mask | flags::DONT_WAIT) } != 0 {
            return Ok(true);
        }
        if mask & flags::IDLE != 0 {
            let idle = state(interp).afters.take_idle();
            if let Some(script) = idle {
                run_script(interp, vm, &script);
                return Ok(true);
            }
        }
        if mask & flags::DONT_WAIT != 0 {
            return Ok(false);
        }
        // Blocking. A timer of this interpreter's coming due is one thing that
        // can still happen, and the wait is bounded by the earliest.
        if let Some(wait) = next_deadline(interp, mask) {
            sleep(wait);
            continue;
        }
        // Nothing of this interpreter's can come due. Whether anything *else*
        // can is a question only the notifier can answer, and only a `tk` build
        // has one.
        #[cfg(feature = "tk")]
        {
            // Block in `Tcl_DoOneEvent` and let its answer decide, which is
            // what `Tcl_VwaitObjCmd` does (`generic/tclEvent.c:1733`). On macOS
            // it blocks with no timeout when no source asked for one, so this
            // waits exactly as long as tclsh waits — including forever.
            return Ok(unsafe { crate::tk::notifier::do_one_event_slot(mask) } != 0);
        }
        // Without a notifier a timer is the only thing that could have become
        // ready, so there is nothing left that could end the wait — the
        // condition `Tcl_DoOneEvent` reports by answering 0
        // (`generic/tclEvent.c:1755-1763`).
        #[cfg(not(feature = "tk"))]
        return Ok(false);
    }
}

/// How long to sleep before one of this interpreter's own timers comes due, or
/// `None` when none is pending.
///
/// Under `tk` the sleep is capped, because a window or file event can arrive at
/// any moment and this loop does not register the queue with the notifier as an
/// event source of its own: it polls between sleeps instead. That is a latency
/// bound on servicing a Tk event while an `after` is outstanding, not a change
/// to what is serviced or in what order.
fn next_deadline(interp: &Shared, mask: i32) -> Option<Duration> {
    if mask & flags::TIMER == 0 {
        return None;
    }
    let now = Instant::now();
    let wait = state(interp)
        .afters
        .soonest()
        .map(|due| due.saturating_duration_since(now))?;
    #[cfg(feature = "tk")]
    let wait = wait.min(POLL_SLICE);
    Some(wait)
}

/// Run one `after` script at the global level, reporting a failure the way
/// `AfterProc` reports it: to stderr, with the run continuing
/// (`generic/tclTimer.c:1155-1163`).
fn run_script(interp: &Shared, vm: &mut VM, script: &str) {
    crate::runtime::flush_globals(vm, interp);
    let outcome = crate::runtime::run_source(interp, script);
    crate::runtime::reseed_globals(vm, interp);
    if let Err(e) = outcome {
        // tclsh prints the message, the stack that produced it and then
        // `("after" script)`. There is no error stack here, so the two lines
        // that exist are printed and the third is not invented.
        eprintln!("{}\n    (\"after\" script)", e.msg);
    }
}

fn sleep(d: Duration) {
    #[cfg(feature = "tk")]
    {
        // The notifier's sleep runs the CFRunLoop, which is what keeps a Tk
        // window alive across the wait; `thread::sleep` would freeze it.
        let ms = d.as_millis().min(i32::MAX as u128) as i32;
        if ms > 0 {
            unsafe { crate::tk::notifier::sleep_ms(ms) };
            return;
        }
    }
    std::thread::sleep(d);
}

fn pop_args(vm: &mut VM, argc: u8) -> Vec<String> {
    let mut args: Vec<String> = (0..argc).map(|_| to_tcl_string(&vm.pop())).collect();
    args.reverse();
    args
}

fn empty() -> Value {
    Value::Str(std::sync::Arc::new(String::new()))
}
