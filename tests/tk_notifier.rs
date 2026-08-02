//! The event loop, driven without Tk.
//!
//! Tk is the reason the notifier exists, but nothing in it needs Tk to run:
//! the queue, the timers, the idle handlers and the file handlers are Tcl's,
//! and every one of them can be exercised from Rust through the same entry
//! points the stub table hands Tk. That is what this file does, so the loop is
//! pinned before a window ever exists to test it with.
//!
//! Each assertion below names the lines of Tcl 9.0.4 that specify the
//! behaviour, because "the queue works" is not a contract and "an event queued
//! with `TCL_QUEUE_MARK` goes behind the previous mark and in front of the
//! tail" is.

#![cfg(feature = "tk")]

use std::ffi::{c_int, c_void};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tclrs::tk::notifier::{self, TclEvent};

// ── the recorder every callback writes into ────────────────────────────────
//
// A slot rather than a closure: the callbacks are `extern "C"` function
// pointers with no environment, exactly as Tcl's are. One recorder per test
// thread, so the tests do not have to run one at a time.

thread_local! {
    static LOG: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn log(what: usize) {
    LOG.with(|l| l.borrow_mut().push(what));
}

fn taken() -> Vec<usize> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// A stand-in for client data.
///
/// The notifier never dereferences a `clientData`; it is an opaque word that
/// comes back to the callback unchanged (`generic/tcl.h:583`, `608`). A small
/// integer is therefore a perfectly good one, and this is how to spell that
/// without claiming the integer addresses anything.
fn tag(n: usize) -> *mut c_void {
    std::ptr::without_provenance_mut(n)
}

// ── events ─────────────────────────────────────────────────────────────────

/// An event that records the tag it was queued with and reports itself handled.
#[repr(C)]
struct Tagged {
    header: TclEvent,
    tag: usize,
}

unsafe extern "C" fn tagged_proc(ev: *mut TclEvent, _flags: c_int) -> c_int {
    log((*(ev as *mut Tagged)).tag);
    1
}

/// An event that only accepts `TCL_FILE_EVENTS`, so it can be left on the
/// queue on purpose.
unsafe extern "C" fn file_only_proc(ev: *mut TclEvent, flags: c_int) -> c_int {
    if flags & notifier::TCL_FILE_EVENTS == 0 {
        return 0;
    }
    log((*(ev as *mut Tagged)).tag);
    1
}

unsafe fn queue(tag: usize, proc_: unsafe extern "C" fn(*mut TclEvent, c_int) -> c_int, at: c_int) {
    let ev = notifier::alloc_event(std::mem::size_of::<Tagged>()) as *mut Tagged;
    (*ev).header.proc_ = Some(proc_);
    (*ev).tag = tag;
    notifier::queue_event_slot(ev as *mut TclEvent, at);
}

#[test]
fn the_three_queue_positions_order_events_the_way_tcl_says() {
    unsafe {
        // TAIL appends, HEAD prepends, MARK goes after the previous mark and
        // in front of everything queued at the tail — the priority mechanism
        // Tk's grab code uses (`generic/tclNotify.c:44-50`, `495-534`).
        queue(1, tagged_proc, notifier::TCL_QUEUE_TAIL);
        queue(2, tagged_proc, notifier::TCL_QUEUE_TAIL);
        queue(3, tagged_proc, notifier::TCL_QUEUE_HEAD);
        queue(4, tagged_proc, notifier::TCL_QUEUE_MARK);
        queue(5, tagged_proc, notifier::TCL_QUEUE_MARK);

        // Queue is now: 4 5 3 1 2 — the first mark went to the front (there
        // was no marker yet, `:523-525`), the second went after it (`:527-529`),
        // and 3 stayed ahead of the tail entries it was pushed in front of.
        let mut serviced = 0;
        while notifier::service_event_slot(0) == 1 {
            serviced += 1;
            assert!(serviced < 10, "the queue never drained");
        }
        assert_eq!(taken(), vec![4, 5, 3, 1, 2]);
    }
}

#[test]
fn an_event_that_refuses_the_flags_stays_on_the_queue() {
    unsafe {
        queue(1, file_only_proc, notifier::TCL_QUEUE_TAIL);
        queue(2, tagged_proc, notifier::TCL_QUEUE_TAIL);

        // The file event returns 0 for a timer-only pass, which means "not
        // handled, leave me here" rather than "discard me"
        // (`generic/tclNotify.c:766-773`). So the pass reaches the second
        // event instead, and the first is still there afterwards.
        assert_eq!(notifier::service_event_slot(notifier::TCL_TIMER_EVENTS), 1);
        assert_eq!(taken(), vec![2]);

        assert_eq!(notifier::service_event_slot(notifier::TCL_FILE_EVENTS), 1);
        assert_eq!(taken(), vec![1]);

        assert_eq!(
            notifier::service_event_slot(0),
            0,
            "the queue should be empty now"
        );
    }
}

#[test]
fn servicing_an_empty_queue_reports_that_it_did_nothing() {
    unsafe {
        assert_eq!(notifier::service_event_slot(0), 0);
        // And `Tcl_DoOneEvent` with TCL_DONT_WAIT polls rather than blocking,
        // returning 0 when there was nothing to do
        // (`generic/tclNotify.c:886-889`).
        assert_eq!(notifier::do_one_event_slot(notifier::TCL_DONT_WAIT), 0);
    }
}

// ── timers ─────────────────────────────────────────────────────────────────

unsafe extern "C" fn timer_proc(client_data: *mut c_void) {
    log(client_data.addr());
}

#[test]
fn timers_fire_in_time_order_and_a_deleted_one_never_fires() {
    unsafe {
        // Registered out of order on purpose: the list is kept sorted by fire
        // time (`generic/tclTimer.c:314-325`), so registration order is not
        // what decides.
        notifier::create_timer_handler(60, timer_proc, tag(3));
        notifier::create_timer_handler(20, timer_proc, tag(1));
        let doomed = notifier::create_timer_handler(200, timer_proc, tag(99));
        notifier::create_timer_handler(40, timer_proc, tag(2));

        // A token that has not fired yet cancels the handler outright
        // (`generic/tclTimer.c:350-376`).
        notifier::delete_timer_handler(doomed);

        // Blocking, so each pass waits out the next timer rather than spinning:
        // the timer source's setup function hands the notifier exactly the
        // interval to the head of the list (`generic/tclTimer.c:412-433`).
        let mut fired = Vec::new();
        let started = Instant::now();
        while fired.len() < 3 {
            assert!(started.elapsed().as_secs() < 5, "timers never fired");
            notifier::do_one_event_slot(notifier::TCL_TIMER_EVENTS);
            fired.extend(taken());
        }
        assert_eq!(fired, vec![1, 2, 3]);

        // The cancelled one is not merely late: give it well past its deadline
        // and it is still absent.
        notifier::sleep_ms(250);
        notifier::do_one_event_slot(notifier::TCL_TIMER_EVENTS | notifier::TCL_DONT_WAIT);
        assert!(taken().is_empty(), "a deleted timer fired anyway");
    }
}

#[test]
fn a_timer_waits_at_least_as_long_as_it_asked_for() {
    unsafe {
        let started = Instant::now();
        notifier::create_timer_handler(120, timer_proc, tag(7));
        let mut fired = Vec::new();
        while fired.is_empty() {
            assert!(started.elapsed().as_secs() < 5, "the timer never fired");
            notifier::do_one_event_slot(notifier::TCL_TIMER_EVENTS);
            fired.extend(taken());
        }
        assert_eq!(fired, vec![7]);
        // `Tcl_CreateTimerHandler` promises "when milliseconds have elapsed"
        // (`generic/tclTimer.c:241-242`), so early is a bug and late is the
        // scheduler. The upper bound is loose because it is not this code's
        // to keep.
        let waited = started.elapsed().as_millis();
        assert!(waited >= 120, "fired after only {waited}ms");
        assert!(waited < 2000, "fired after {waited}ms");
    }
}

// ── idle handlers ──────────────────────────────────────────────────────────

unsafe extern "C" fn idle_proc(client_data: *mut c_void) {
    log(client_data.addr());
}

/// An idle handler that registers another one, to prove the second is deferred.
unsafe extern "C" fn idle_that_breeds(client_data: *mut c_void) {
    log(client_data.addr());
    notifier::do_when_idle(idle_proc, tag(99));
}

#[test]
fn idle_handlers_run_in_order_and_not_the_ones_they_create() {
    unsafe {
        notifier::do_when_idle(idle_proc, tag(1));
        notifier::do_when_idle(idle_that_breeds, tag(2));
        notifier::do_when_idle(idle_proc, tag(3));

        // One pass runs the handlers that were present when it started and
        // stops there — the generation test at `generic/tclTimer.c:739-742`.
        // That is what makes `update idletasks` terminate.
        assert_eq!(notifier::do_one_event_slot(notifier::TCL_IDLE_EVENTS), 1);
        assert_eq!(taken(), vec![1, 2, 3]);

        // The one handler 2 created is still waiting, and the next pass takes
        // it.
        assert_eq!(notifier::do_one_event_slot(notifier::TCL_IDLE_EVENTS), 1);
        assert_eq!(taken(), vec![99]);

        assert_eq!(
            notifier::do_one_event_slot(notifier::TCL_IDLE_EVENTS),
            0,
            "an idle pass with nothing to do should report 0"
        );
    }
}

#[test]
fn a_cancelled_idle_call_does_not_run() {
    unsafe {
        notifier::do_when_idle(idle_proc, tag(1));
        notifier::do_when_idle(idle_proc, tag(2));
        notifier::do_when_idle(idle_proc, tag(1));

        // Cancels *every* registration of that function/data pair, not the
        // first (`generic/tclTimer.c:668-685`) — the loop inside the loop.
        notifier::cancel_idle_call(idle_proc, tag(1));
        notifier::do_one_event_slot(notifier::TCL_IDLE_EVENTS);
        assert_eq!(taken(), vec![2]);
    }
}

// ── Tcl_Sleep ──────────────────────────────────────────────────────────────

#[test]
fn tcl_sleep_sleeps_for_about_as_long_as_it_was_asked_to() {
    unsafe {
        for ms in [0, 1, 40, 150] {
            let started = Instant::now();
            notifier::sleep_ms(ms);
            let waited = started.elapsed().as_millis() as i64;
            assert!(
                waited >= i64::from(ms),
                "Tcl_Sleep({ms}) returned after {waited}ms, which is early"
            );
            // The run loop is running during the sleep
            // (`macosx/tclMacOSXNotify.c:1517-1531`), so the cost is a run
            // loop pass and not a `nanosleep`; the slack is for that.
            assert!(
                waited <= i64::from(ms) + 250,
                "Tcl_Sleep({ms}) returned after {waited}ms, which is late"
            );
        }
    }
}

#[test]
fn a_sleep_does_not_service_events() {
    unsafe {
        queue(1, tagged_proc, notifier::TCL_QUEUE_TAIL);
        notifier::sleep_ms(60);
        // `CFRunLoopRunInMode` is called with `returnAfterSourceHandled` false
        // during a sleep (`macosx/tclMacOSXNotify.c:1518-1519`) and Tcl's own
        // queue is never touched, so a sleep is not a disguised `update`.
        assert!(taken().is_empty(), "Tcl_Sleep serviced a queued event");
        assert_eq!(notifier::service_event_slot(0), 1);
        assert_eq!(taken(), vec![1]);
    }
}

// ── file handlers ──────────────────────────────────────────────────────────

static READ_MASK: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn file_proc(client_data: *mut c_void, mask: c_int) {
    READ_MASK.store(mask as usize, Ordering::SeqCst);
    log(client_data.addr());
}

#[test]
fn a_readable_descriptor_reaches_its_handler_through_the_queue() {
    unsafe {
        let (mut a, mut b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let fd = b.as_raw_fd();
        notifier::create_file_handler(fd, notifier::TCL_READABLE, file_proc, tag(5));

        a.write_all(b"x").expect("write");

        let started = Instant::now();
        let mut fired = Vec::new();
        while fired.is_empty() {
            assert!(
                started.elapsed().as_secs() < 5,
                "the file handler never ran"
            );
            notifier::do_one_event_slot(notifier::TCL_FILE_EVENTS | notifier::TCL_DONT_WAIT);
            fired.extend(taken());
        }
        assert_eq!(fired, vec![5]);
        assert_eq!(
            READ_MASK.load(Ordering::SeqCst) as c_int,
            notifier::TCL_READABLE,
            "the handler should be told which condition fired"
        );

        // The handler ran with the descriptor still readable, which is the
        // contract: it is the handler's job to consume the data.
        let mut byte = [0u8; 1];
        b.read_exact(&mut byte).expect("read");
        assert_eq!(&byte, b"x");

        notifier::delete_file_handler(fd);

        // Deleted means deleted: more data arrives and nothing runs.
        a.write_all(b"y").expect("write");
        for _ in 0..5 {
            notifier::do_one_event_slot(notifier::TCL_FILE_EVENTS | notifier::TCL_DONT_WAIT);
        }
        assert!(taken().is_empty(), "a deleted file handler ran anyway");
    }
}

// ── event sources ──────────────────────────────────────────────────────────

unsafe extern "C" fn source_setup(client_data: *mut c_void, _flags: c_int) {
    log(client_data.addr());
}

unsafe extern "C" fn source_check(client_data: *mut c_void, _flags: c_int) {
    log(client_data.addr() + 1000);
}

#[test]
fn an_event_source_is_set_up_before_the_wait_and_checked_after_it() {
    unsafe {
        notifier::create_event_source(Some(source_setup), Some(source_check), tag(7));
        notifier::do_one_event_slot(notifier::TCL_DONT_WAIT);
        // Setup runs before `Tcl_WaitForEvent` and check runs after it
        // (`generic/tclNotify.c:983-1018`) — the order is the whole point of
        // there being two functions.
        assert_eq!(taken(), vec![7, 1007]);

        // A deleted source is never called again
        // (`generic/tclNotify.c:335-337`).
        notifier::delete_event_source(Some(source_setup), Some(source_check), tag(7));
        notifier::do_one_event_slot(notifier::TCL_DONT_WAIT);
        assert!(
            taken().is_empty(),
            "a deleted event source was still called"
        );
    }
}

#[test]
fn the_same_function_with_different_client_data_is_a_different_source() {
    unsafe {
        notifier::create_event_source(Some(source_setup), Some(source_check), tag(1));
        notifier::create_event_source(Some(source_setup), Some(source_check), tag(2));
        // All three fields are matched (`generic/tclNotify.c:359-363`), so
        // deleting one leaves the other.
        notifier::delete_event_source(Some(source_setup), Some(source_check), tag(1));
        notifier::do_one_event_slot(notifier::TCL_DONT_WAIT);
        assert_eq!(taken(), vec![2, 1002]);
        notifier::delete_event_source(Some(source_setup), Some(source_check), tag(2));
    }
}

// ── the table ──────────────────────────────────────────────────────────────

#[test]
fn every_notifier_slot_tk_can_reach_has_a_body() {
    // Driving the notifier from Rust proves it works; this proves Tk can get
    // at it, which is a different claim. `host::implemented` reports the slots
    // that were patched over the traps, by the name the header gives them.
    let installed: Vec<&str> = tclrs::tk::host::implemented()
        .into_iter()
        .map(|(_, name)| name)
        .collect();

    // The whole event-loop surface of `TclStubs`, in slot order. Every one of
    // them is a function Tk calls: `Tcl_DoOneEvent` is `Tk_MainLoop`,
    // `Tcl_CreateEventSource` is how the Aqua event source is registered
    // (`tk9.0.4/macosx/tkMacOSXNotify.c:267-268`), and the rest are what
    // `after`, `update` and `vwait` are made of.
    for name in [
        "tcl_CreateFileHandler",
        "tcl_DeleteFileHandler",
        "tcl_SetTimer",
        "tcl_Sleep",
        "tcl_WaitForEvent",
        "tcl_CancelIdleCall",
        "tcl_CreateEventSource",
        "tcl_CreateTimerHandler",
        "tcl_DeleteEvents",
        "tcl_DeleteEventSource",
        "tcl_DeleteTimerHandler",
        "tcl_DoOneEvent",
        "tcl_DoWhenIdle",
        "tcl_GetServiceMode",
        "tcl_QueueEvent",
        "tcl_ServiceAll",
        "tcl_ServiceEvent",
        "tcl_SetMaxBlockTime",
        "tcl_SetServiceMode",
        "tcl_GetCurrentThread",
        "tcl_ThreadAlert",
        "tcl_ThreadQueueEvent",
        "tcl_AlertNotifier",
        "tcl_ServiceModeHook",
    ] {
        assert!(
            installed.contains(&name),
            "{name} is still a trap; Tk would abort on it"
        );
    }
}

#[test]
fn the_one_platform_slot_tk_needs_is_the_one_that_was_filled_in() {
    // A static scan of libtk finds exactly one `TclPlatStubs` entry referenced
    // anywhere in it. Tk calls it from `Tk_MacOSXSetupTkNotifier`
    // (`tk9.0.4/macosx/tkMacOSXNotify.c:270-271`) to keep Tcl events flowing
    // while a menu is held open, and it is slot 2 of that table
    // (`generic/tclPlatDecls.h:101`).
    let mut table = tclrs::tk::abi::TclPlatStubs {
        magic: 0,
        hooks: std::ptr::null(),
        slots: tclrs::tk::generated::TCL_PLAT_TRAPS,
    };
    let slot = unsafe { notifier::install_plat(&mut table) };
    assert_eq!(
        tclrs::tk::generated::TCL_PLAT_NAMES[slot],
        "tcl_MacOSXNotifierAddRunLoopMode"
    );
}

// ── the service mode ───────────────────────────────────────────────────────

#[test]
fn the_service_mode_is_reported_and_returned_the_way_tcl_reports_it() {
    unsafe {
        // A fresh notifier starts in TCL_SERVICE_NONE — the C zeroes its
        // thread-specific data (`generic/tclNotify.c:51-52`).
        assert_eq!(notifier::get_service_mode(), notifier::TCL_SERVICE_NONE);
        let previous = notifier::set_service_mode(notifier::TCL_SERVICE_ALL);
        assert_eq!(previous, notifier::TCL_SERVICE_NONE);
        assert_eq!(notifier::get_service_mode(), notifier::TCL_SERVICE_ALL);

        // `Tcl_ServiceAll` returns 0 without doing anything while the mode is
        // NONE (`generic/tclNotify.c:1095-1097`), and services the queue once
        // it is ALL.
        queue(8, tagged_proc, notifier::TCL_QUEUE_TAIL);
        assert_eq!(notifier::service_all_slot(), 1);
        assert_eq!(taken(), vec![8]);

        notifier::set_service_mode(notifier::TCL_SERVICE_NONE);
        queue(9, tagged_proc, notifier::TCL_QUEUE_TAIL);
        assert_eq!(notifier::service_all_slot(), 0);
        assert!(taken().is_empty());
        assert_eq!(notifier::service_event_slot(0), 1);
        assert_eq!(taken(), vec![9]);
    }
}

// ── what `update` and `vwait` are made of ──────────────────────────────────
//
// Neither command is a notifier function: `update` is a loop around
// `Tcl_DoOneEvent` (`generic/tclEvent.c:1982-1991`) and `vwait` is another one
// (`generic/tclEvent.c:1731-1753`). Both belong to the evaluator, which does
// not exist here yet. What *is* here is everything they lean on, and these
// three tests are those loops written out with the surrounding Tcl removed —
// so that when the commands are added, the reason they terminate is already
// pinned.

/// A timer callback that sets a flag, standing in for the variable write that
/// ends a `vwait`.
unsafe extern "C" fn set_done(client_data: *mut c_void) {
    log(client_data.addr());
    DONE.store(true, Ordering::SeqCst);
}

static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[test]
fn the_update_loop_drains_everything_and_stops() {
    unsafe {
        // `update` is `while (Tcl_DoOneEvent(TCL_ALL_EVENTS|TCL_DONT_WAIT))`
        // (`generic/tclEvent.c:1964`, `1982`). Three kinds of pending work, one
        // of each, so that "drains everything" means all three.
        queue(1, tagged_proc, notifier::TCL_QUEUE_TAIL);
        notifier::create_timer_handler(0, timer_proc, tag(2));
        notifier::do_when_idle(idle_proc, tag(3));

        let started = Instant::now();
        let mut passes = 0;
        while notifier::do_one_event_slot(notifier::TCL_ALL_EVENTS | notifier::TCL_DONT_WAIT) != 0 {
            passes += 1;
            assert!(passes < 100, "the update loop did not converge");
            assert!(started.elapsed().as_secs() < 5, "the update loop hung");
        }

        let mut ran = taken();
        ran.sort_unstable();
        assert_eq!(ran, vec![1, 2, 3], "update left work behind");
    }
}

#[test]
fn the_update_idletasks_loop_runs_only_idle_handlers() {
    unsafe {
        // `update idletasks` is the same loop with
        // TCL_IDLE_EVENTS|TCL_DONT_WAIT (`generic/tclEvent.c:1972`). The queued
        // event must survive it: an idle pass is not allowed to eat a window
        // event, which is the whole reason the two spellings exist.
        queue(1, tagged_proc, notifier::TCL_QUEUE_TAIL);
        notifier::do_when_idle(idle_proc, tag(3));

        let mut passes = 0;
        while notifier::do_one_event_slot(notifier::TCL_IDLE_EVENTS | notifier::TCL_DONT_WAIT) != 0
        {
            passes += 1;
            assert!(passes < 100, "the idletasks loop did not converge");
        }
        assert_eq!(taken(), vec![3]);

        assert_eq!(
            notifier::service_event_slot(0),
            1,
            "update idletasks swallowed a queued event"
        );
        assert_eq!(taken(), vec![1]);
    }
}

#[test]
fn the_vwait_loop_gets_control_back_after_each_event() {
    unsafe {
        // `vwait` blocks — no TCL_DONT_WAIT — and re-tests its variable after
        // every pass (`generic/tclEvent.c:1732-1734`). That only works because
        // `Tcl_DoOneEvent` returns as soon as it has serviced one thing rather
        // than looping until the queue is empty
        // (`generic/tclNotify.c:1046-1061`).
        DONE.store(false, Ordering::SeqCst);
        notifier::create_timer_handler(80, set_done, tag(4));

        let started = Instant::now();
        let mut passes = 0;
        let mut found_event = 1;
        while !DONE.load(Ordering::SeqCst) && found_event != 0 {
            found_event = notifier::do_one_event_slot(0);
            passes += 1;
            assert!(started.elapsed().as_secs() < 5, "the vwait loop hung");
        }
        assert!(DONE.load(Ordering::SeqCst), "the vwait never completed");
        assert_eq!(taken(), vec![4]);
        // And it did so without spinning: the timer source hands the notifier
        // the interval to the next timer (`generic/tclTimer.c:412-433`), so the
        // wait is one block and not a poll.
        assert!(passes <= 4, "the vwait loop spun {passes} times");
    }
}
