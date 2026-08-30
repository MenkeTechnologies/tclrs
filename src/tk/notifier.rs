//! The event loop Tk drives, ported from Tcl 9.0.4's notifier.
//!
//! Tk does not have an event loop of its own. It registers an *event source*
//! and then calls `Tcl_DoOneEvent` — both of them slots in the stub table — and
//! everything a window does, from a mouse click to a redraw, arrives through
//! the queue this file implements. So hosting Tk means hosting Tcl's notifier,
//! and the notifier's contract is not a matter of taste: Tk's `after`, `update`
//! and `vwait` all depend on the exact order in which events, timers and idle
//! handlers are serviced.
//!
//! The port is in three layers, the same three the C source has:
//!
//! * The **generic layer** (`generic/tclNotify.c`) — the event queue, the list
//!   of event sources, the service mode, and `Tcl_DoOneEvent` itself. Portable,
//!   and reproduced here statement for statement.
//! * The **timer layer** (`generic/tclTimer.c`) — timer handlers and idle
//!   handlers, which are not built into the notifier at all: they are an
//!   ordinary event source that `InitTimer` registers on first use
//!   (`generic/tclTimer.c:180-191`).
//! * The **platform layer** (`macosx/tclMacOSXNotify.c`) — the part that
//!   actually blocks. On macOS that is a CFRunLoop, which is why Tk needs the
//!   main thread; the binary's entry point (`src/main.rs`) is what arranges
//!   that.
//!
//! # State is per thread, as Tcl's is
//!
//! Every function here reaches its state through [`state`], which is a
//! thread-local, exactly as the C source's `TCL_TSD_INIT(&dataKey)` is
//! (`generic/tclNotify.c:85`, `generic/tclTimer.c:108`,
//! `macosx/tclMacOSXNotify.c:263`). A notifier belongs to one thread; queueing
//! an event onto another thread's notifier is `Tcl_ThreadQueueEvent`, and it
//! goes through the same door.
//!
//! # Three places this diverges from the C, and why
//!
//! 1. **File handlers use `CFFileDescriptor`, not a `select()` thread.** Tcl
//!    watches file descriptors by starting a second thread that sits in
//!    `select()` and wakes the run loop through a pipe
//!    (`macosx/tclMacOSXNotify.c:634-673`, `1786-1976`), together with a global
//!    waiting list (`:282`), a trigger pipe (`:298-299`) and three
//!    `pthread_atfork` handlers (`:1978-2072`) to put it all back together after
//!    a fork. CoreFoundation can watch a descriptor on the run loop directly,
//!    which is the same answer with none of that machinery. What is preserved
//!    is the *contract*: the ready mask lives in the `FileHandler` and not in
//!    the queued event, so a descriptor that was closed and reopened does not
//!    fire a stale handler (`:1110-1123`); at most one event is queued while a
//!    handler's ready mask is non-zero (`:1341-1348`); and the event refuses to
//!    be serviced, rather than being discarded, when `TCL_FILE_EVENTS` is not
//!    in the flags (`:1096-1098`).
//!
//!    The cost is `TCL_EXCEPTION`: `select()` has an exceptional set and
//!    `CFFileDescriptor` has only read and write. A handler registered for
//!    `TCL_EXCEPTION` alone is accepted and never fires. That is stated at
//!    [`create_file_handler`] and is the one place a caller can tell the two
//!    apart.
//!
//! 2. **The event-source list is snapshotted before it is walked.** The C walks
//!    a linked list and reads `sourcePtr->nextPtr` *after* calling into the
//!    source (`generic/tclNotify.c:984-989`), so a source that deletes itself
//!    from its own setup function frees the node the loop is standing on. This
//!    takes a copy of the list first and re-checks each entry against the live
//!    list before calling it, which gives the documented behaviour — "the given
//!    event source is canceled, so its function will never again be called"
//!    (`generic/tclNotify.c:335-337`) — without the dangling read.
//!
//! 3. **`Tcl_AsyncReady` is a constant.** `Tcl_ServiceEvent` and
//!    `Tcl_DoOneEvent` both begin by draining async handlers
//!    (`generic/tclNotify.c:667-670`, `917-920`). An async handler can only
//!    exist if something called `Tcl_AsyncCreate`, which is not a slot this
//!    host serves, so there are none and the check is `false` by construction
//!    rather than by omission.
//!
//! Everything in this file is behind the `tk` cargo feature.

use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use super::abi::{RawStub, TclPlatStubs, TclStubs, TclTime};
use super::generated::{TCL_NAMES, TCL_PLAT_NAMES};
use super::trace::{record, Table};

// ---------------------------------------------------------------------------
// The constants the flags are made of (`generic/tcl.h`)
// ---------------------------------------------------------------------------

/// `TCL_DONT_WAIT` (`generic/tcl.h:1276`).
pub const TCL_DONT_WAIT: c_int = 1 << 1;
/// `TCL_WINDOW_EVENTS` (`generic/tcl.h:1277`).
pub const TCL_WINDOW_EVENTS: c_int = 1 << 2;
/// `TCL_FILE_EVENTS` (`generic/tcl.h:1278`).
pub const TCL_FILE_EVENTS: c_int = 1 << 3;
/// `TCL_TIMER_EVENTS` (`generic/tcl.h:1279`).
pub const TCL_TIMER_EVENTS: c_int = 1 << 4;
/// `TCL_IDLE_EVENTS` (`generic/tcl.h:1280`).
pub const TCL_IDLE_EVENTS: c_int = 1 << 5;
/// `TCL_ALL_EVENTS` — everything except the "do not block" bit
/// (`generic/tcl.h:1281`: `(~TCL_DONT_WAIT)`).
pub const TCL_ALL_EVENTS: c_int = !TCL_DONT_WAIT;

/// `TCL_QUEUE_TAIL` (`generic/tcl.h:1301-1304`).
pub const TCL_QUEUE_TAIL: c_int = 0;
/// `TCL_QUEUE_HEAD`.
pub const TCL_QUEUE_HEAD: c_int = 1;
/// `TCL_QUEUE_MARK`.
pub const TCL_QUEUE_MARK: c_int = 2;
/// `TCL_QUEUE_ALERT_IF_EMPTY`, which is a flag bit rather than a position.
pub const TCL_QUEUE_ALERT_IF_EMPTY: c_int = 4;

/// `TCL_SERVICE_NONE` (`generic/tcl.h:1311`).
pub const TCL_SERVICE_NONE: c_int = 0;
/// `TCL_SERVICE_ALL` (`generic/tcl.h:1312`).
pub const TCL_SERVICE_ALL: c_int = 1;

/// `TCL_READABLE` (`generic/tcl.h:1349`).
pub const TCL_READABLE: c_int = 1 << 1;
/// `TCL_WRITABLE` (`generic/tcl.h:1350`).
pub const TCL_WRITABLE: c_int = 1 << 2;
/// `TCL_EXCEPTION` (`generic/tcl.h:1351`). Accepted and never reported; see the
/// module documentation.
pub const TCL_EXCEPTION: c_int = 1 << 3;

// ---------------------------------------------------------------------------
// The ABI of an event and its handlers (`generic/tcl.h`)
// ---------------------------------------------------------------------------

/// `Tcl_Event` (`generic/tcl.h:1292-1295`).
///
/// Caller-allocated and caller-extended: an event source allocates a larger
/// struct whose first member is one of these and casts. Both fields are read
/// and written by this file and by whoever queued the event, so the layout is
/// shared ABI, not private state.
#[repr(C)]
pub struct TclEvent {
    /// `Tcl_EventProc *proc`. Set to NULL while the event is being serviced,
    /// which is how re-entrancy is detected (`generic/tclNotify.c:694-696`).
    pub proc_: Option<TclEventProc>,
    pub next_ptr: *mut TclEvent,
}

/// `typedef int (Tcl_EventProc) (Tcl_Event *evPtr, int flags)`
/// (`generic/tcl.h:575`).
pub type TclEventProc = unsafe extern "C" fn(*mut TclEvent, c_int) -> c_int;
/// `typedef void (Tcl_EventSetupProc) (void *clientData, int flags)`
/// (`generic/tcl.h:578`).
pub type TclEventSetupProc = unsafe extern "C" fn(*mut c_void, c_int);
/// `typedef void (Tcl_EventCheckProc) (void *clientData, int flags)`
/// (`generic/tcl.h:576`).
pub type TclEventCheckProc = unsafe extern "C" fn(*mut c_void, c_int);
/// `typedef int (Tcl_EventDeleteProc) (Tcl_Event *evPtr, void *clientData)`
/// (`generic/tcl.h:577`).
pub type TclEventDeleteProc = unsafe extern "C" fn(*mut TclEvent, *mut c_void) -> c_int;
/// `typedef void (Tcl_TimerProc) (void *clientData)` (`generic/tcl.h:608`).
pub type TclTimerProc = unsafe extern "C" fn(*mut c_void);
/// `typedef void (Tcl_IdleProc) (void *clientData)` (`generic/tcl.h:583`).
pub type TclIdleProc = unsafe extern "C" fn(*mut c_void);
/// `typedef void (Tcl_FileProc) (void *clientData, int mask)`
/// (`generic/tcl.h:580`).
pub type TclFileProc = unsafe extern "C" fn(*mut c_void, c_int);

// ---------------------------------------------------------------------------
// CoreFoundation
// ---------------------------------------------------------------------------
//
// Declared here rather than pulled from a crate: these are eleven functions and
// four constants with a frozen ABI, and a dependency that has to keep building
// in 2035 costs more than the declarations do.

type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopTimerRef = *mut c_void;
type CFFileDescriptorRef = *mut c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFIndex = isize;
type CFOptionFlags = usize;
type CFHashCode = usize;
type CFTimeInterval = f64;
type CFAbsoluteTime = f64;

/// `CFRunLoopSourceContext` (`CFRunLoop.h`), the version-0 shape Tcl fills in
/// (`macosx/tclMacOSXNotify.c:473-477`).
#[repr(C)]
struct CFRunLoopSourceContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<unsafe extern "C" fn(*const c_void) -> CFHashCode>,
    schedule: Option<unsafe extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    cancel: Option<unsafe extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// `CFFileDescriptorContext` (`CFFileDescriptor.h`) — the five-field context
/// shape CoreFoundation shares between several of its sources.
#[repr(C)]
struct CFContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

/// `CFRunLoopRunInMode` return codes (`CFRunLoop.h:36-39`).
const K_CF_RUN_LOOP_RUN_FINISHED: i32 = 1;
const K_CF_RUN_LOOP_RUN_STOPPED: i32 = 2;
const K_CF_RUN_LOOP_RUN_TIMED_OUT: i32 = 3;
const K_CF_RUN_LOOP_RUN_HANDLED_SOURCE: i32 = 4;

/// `kCFFileDescriptorReadCallBack` / `WriteCallBack` (`CFFileDescriptor.h`).
const K_CF_FD_READ: CFOptionFlags = 1 << 0;
const K_CF_FD_WRITE: CFOptionFlags = 1 << 1;

/// `CF_TIMEINTERVAL_FOREVER` (`macosx/tclMacOSXNotify.c:350`) — Tcl's own
/// spelling of "no timeout", chosen so that adding it to the current absolute
/// time stays inside the range CoreFoundation handles.
const CF_TIMEINTERVAL_FOREVER: CFTimeInterval = 5.05e8;

/// `TCL_EVENTS_ONLY_RUN_LOOP_MODE` (`macosx/tclMacOSXNotify.c:337-338`): the
/// private run loop mode a recursive `Tcl_WaitForEvent` runs in, so that a
/// nested wait cannot dispatch a source that the outer one owns.
const TCL_EVENTS_ONLY_RUN_LOOP_MODE: &str = "com.tcltk.tclEventsOnlyRunLoopMode";

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CFStringRef;
    static kCFRunLoopCommonModes: CFStringRef;

    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: CFTimeInterval, return_after: u8) -> i32;
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFRunLoopSourceCreate(
        allocator: CFAllocatorRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
    fn CFRunLoopTimerCreate(
        allocator: CFAllocatorRef,
        fire_date: CFAbsoluteTime,
        interval: CFTimeInterval,
        flags: CFOptionFlags,
        order: CFIndex,
        callout: unsafe extern "C" fn(CFRunLoopTimerRef, *mut c_void),
        context: *mut CFContext,
    ) -> CFRunLoopTimerRef;
    fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, at: CFAbsoluteTime);
    fn CFRunLoopTimerGetNextFireDate(timer: CFRunLoopTimerRef) -> CFAbsoluteTime;
    fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;
    fn CFStringCreateWithCString(
        allocator: CFAllocatorRef,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const c_void);

    fn CFFileDescriptorCreate(
        allocator: CFAllocatorRef,
        fd: c_int,
        close_on_invalidate: u8,
        callout: unsafe extern "C" fn(CFFileDescriptorRef, CFOptionFlags, *mut c_void),
        context: *mut CFContext,
    ) -> CFFileDescriptorRef;
    fn CFFileDescriptorEnableCallBacks(f: CFFileDescriptorRef, types: CFOptionFlags);
    fn CFFileDescriptorDisableCallBacks(f: CFFileDescriptorRef, types: CFOptionFlags);
    fn CFFileDescriptorInvalidate(f: CFFileDescriptorRef);
    fn CFFileDescriptorCreateRunLoopSource(
        allocator: CFAllocatorRef,
        f: CFFileDescriptorRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
}

/// `kCFStringEncodingUTF8`.
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One registered event source (`generic/tclNotify.c:35-40`).
#[derive(Clone, Copy)]
struct EventSource {
    setup: Option<TclEventSetupProc>,
    check: Option<TclEventCheckProc>,
    client_data: *mut c_void,
}

impl EventSource {
    /// Whether two registrations name the same source.
    ///
    /// All three fields, because that is what `Tcl_DeleteEventSource` matches
    /// on (`generic/tclNotify.c:359-363`) — the same setup function registered
    /// twice with different client data is two sources. Written out rather than
    /// derived because comparing two function *pointers* is only meaningful
    /// through `ptr::fn_addr_eq`.
    fn same(&self, other: &EventSource) -> bool {
        self.setup.map(|f| f as usize) == other.setup.map(|f| f as usize)
            && self.check.map(|f| f as usize) == other.check.map(|f| f as usize)
            && self.client_data == other.client_data
    }
}

/// One pending timer (`generic/tclTimer.c`'s `TimerHandler`).
struct TimerHandler {
    /// Absolute time at which it fires.
    time: TclTime,
    proc_: TclTimerProc,
    client_data: *mut c_void,
    /// The `Tcl_TimerToken` handed back, which is the integer id cast to a
    /// pointer (`generic/tclTimer.c:307`).
    token: c_int,
}

/// One pending idle handler (`generic/tclTimer.c:72-78`).
struct IdleHandler {
    proc_: TclIdleProc,
    client_data: *mut c_void,
    /// Which pass of `TclServiceIdle` this handler belongs to, so a handler
    /// created by a handler is not run in the same pass
    /// (`generic/tclTimer.c:725-730`).
    generation: c_int,
}

/// One watched descriptor (`macosx/tclMacOSXNotify.c:155-166`).
struct FileHandler {
    fd: c_int,
    /// The events wanted, as last set by `Tcl_CreateFileHandler`.
    mask: c_int,
    /// The events seen since the handler last ran. Kept here rather than in the
    /// queued event so that closing and reopening the descriptor cannot fire a
    /// stale handler (`macosx/tclMacOSXNotify.c:1115-1119`).
    ready_mask: c_int,
    /// Whether an event for this descriptor is already on the queue, which is
    /// what stops a second readiness queueing a second event
    /// (`macosx/tclMacOSXNotify.c:1336-1348`).
    event_queued: bool,
    proc_: TclFileProc,
    client_data: *mut c_void,
    /// The CoreFoundation objects standing in for Tcl's `select()` masks.
    cf_fd: CFFileDescriptorRef,
    cf_source: CFRunLoopSourceRef,
}

/// The event that a ready descriptor queues (`macosx/tclMacOSXNotify.c:173-181`).
///
/// `header` first, because the queue only ever sees a `Tcl_Event *` and casts.
#[repr(C)]
struct FileHandlerEvent {
    header: TclEvent,
    fd: c_int,
}

/// Everything a notifier owns, per thread.
///
/// The field list is `generic/tclNotify.c:55-83` and `generic/tclTimer.c:92-106`
/// merged: this host has one notifier per thread and the timer module's data is
/// per thread for the same reason, so keeping them apart would buy nothing but
/// a second thread-local.
pub struct Notifier {
    // ── the event queue (`generic/tclNotify.c:56-63`) ──
    first_event: *mut TclEvent,
    last_event: *mut TclEvent,
    marker_event: *mut TclEvent,
    event_count: isize,

    // ── notifier state (`generic/tclNotify.c:64-76`) ──
    service_mode: c_int,
    block_time_set: bool,
    block_time: TclTime,
    in_traversal: bool,
    sources: Vec<EventSource>,
    thread_id: *mut c_void,

    // ── the timer module (`generic/tclTimer.c:92-106`) ──
    timers: Vec<TimerHandler>,
    last_timer_id: c_int,
    timer_pending: bool,
    idles: Vec<IdleHandler>,
    idle_generation: c_int,
    /// Whether the timer event source has been registered
    /// (`generic/tclTimer.c:185-189`).
    timer_source_registered: bool,

    // ── the platform layer (`macosx/tclMacOSXNotify.c:200-261`) ──
    run_loop: CFRunLoopRef,
    run_loop_source: CFRunLoopSourceRef,
    run_loop_timer: CFRunLoopTimerRef,
    events_only_mode: CFStringRef,
    /// Set by the run loop source's perform callback, read by
    /// `Tcl_WaitForEvent` to tell a Tcl wakeup from a foreign one
    /// (`macosx/tclMacOSXNotify.c:1243`, `1274`).
    run_loop_source_performed: bool,
    /// Whether a `Tcl_WaitForEvent` is already running a run loop on this
    /// thread (`macosx/tclMacOSXNotify.c:1254-1259`).
    run_loop_running: bool,
    /// True while inside `Tcl_Sleep` (`macosx/tclMacOSXNotify.c:1516`).
    sleeping: bool,
    files: Vec<FileHandler>,
}

thread_local! {
    /// The one notifier of this thread, created on first use.
    ///
    /// A raw pointer rather than a `RefCell` because the notifier calls out to
    /// arbitrary code — an event's `proc`, a timer's callback — and that code
    /// re-enters `Tcl_DoOneEvent` as a matter of routine (`vwait` inside a
    /// button handler is the ordinary case). A borrow held across such a call
    /// would panic on the second level. The C reaches its state through a bare
    /// `tsdPtr` for the same reason.
    static NOTIFIER: std::cell::Cell<*mut Notifier> = const { std::cell::Cell::new(ptr::null_mut()) };
}

/// This thread's notifier, initialising it on first use.
///
/// `TclInitNotifier` runs from `Tcl_CreateInterp` in the C
/// (`generic/tclNotify.c:120-145`); doing it on first use instead is
/// indistinguishable from the outside and keeps the cost off a run that never
/// touches an event.
///
/// # Safety
/// The returned reference is valid for the life of the thread, and no two live
/// references are ever taken across a callback — every function here fetches it
/// again after calling out.
pub unsafe fn state() -> &'static mut Notifier {
    let p = NOTIFIER.with(|c| c.get());
    if !p.is_null() {
        return &mut *p;
    }
    let fresh = Box::into_raw(Box::new(Notifier {
        first_event: ptr::null_mut(),
        last_event: ptr::null_mut(),
        marker_event: ptr::null_mut(),
        event_count: 0,
        service_mode: TCL_SERVICE_NONE,
        block_time_set: false,
        block_time: TclTime { sec: 0, usec: 0 },
        in_traversal: false,
        sources: Vec::new(),
        thread_id: libc::pthread_self() as *mut c_void,
        timers: Vec::new(),
        last_timer_id: 0,
        timer_pending: false,
        idles: Vec::new(),
        idle_generation: 0,
        timer_source_registered: false,
        run_loop: ptr::null_mut(),
        run_loop_source: ptr::null_mut(),
        run_loop_timer: ptr::null_mut(),
        events_only_mode: ptr::null(),
        run_loop_source_performed: false,
        run_loop_running: false,
        sleeping: false,
        files: Vec::new(),
    }));
    NOTIFIER.with(|c| c.set(fresh));
    init_platform(&mut *fresh);
    &mut *fresh
}

/// Whether this thread's run loop is the process's main run loop.
///
/// The one thing Tk checks before it installs its Aqua event source
/// (`tk9.0.4/macosx/tkMacOSXNotify.c:258`): if this is false Tk skips the
/// installation entirely and no window event ever reaches the queue.
pub fn on_main_run_loop() -> bool {
    unsafe { CFRunLoopGetCurrent() == CFRunLoopGetMain() }
}

// ---------------------------------------------------------------------------
// The platform layer (`macosx/tclMacOSXNotify.c`)
// ---------------------------------------------------------------------------

/// `TclpInitNotifier` (`macosx/tclMacOSXNotify.c:442-583`), minus the parts
/// that exist only to run the `select()` thread.
///
/// What is kept is what `Tcl_MacOSXNotifierAddRunLoopMode` needs to have
/// something to add (`:609-615`): a run loop, a source on it, and later a
/// timer. The observer the C installs maintains the global waiting list
/// (`:1370-1403`), which this port does not have, so it is not installed.
unsafe fn init_platform(n: &mut Notifier) {
    let run_loop = CFRunLoopGetCurrent();

    let mode = CString::new(TCL_EVENTS_ONLY_RUN_LOOP_MODE).expect("mode name has no NUL");
    n.events_only_mode =
        CFStringCreateWithCString(ptr::null(), mode.as_ptr(), K_CF_STRING_ENCODING_UTF8);

    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: n as *mut Notifier as *mut c_void,
        retain: None,
        release: None,
        copy_description: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: Some(queue_file_events),
    };
    // LONG_MIN as the order, so this source is served before any source a
    // third party added (`macosx/tclMacOSXNotify.c:476`).
    let source = CFRunLoopSourceCreate(ptr::null(), CFIndex::MIN, &mut context);
    assert!(!source.is_null(), "could not create CFRunLoopSource");
    CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
    CFRunLoopAddSource(run_loop, source, n.events_only_mode);

    n.run_loop = run_loop;
    n.run_loop_source = source;
}

/// `TimerWakeUp` (`macosx/tclMacOSXNotify.c:869-874`): the CFRunLoopTimer
/// callback, whose entire job is to end `CFRunLoopRunInMode`. Empty in the C
/// too — waking up *is* the effect.
unsafe extern "C" fn timer_wake_up(_timer: CFRunLoopTimerRef, _info: *mut c_void) {}

/// `TclpSetTimer` (`macosx/tclMacOSXNotify.c:823-851`).
///
/// A NULL `timePtr` means "no timeout"; anything else is the maximum the next
/// block may last. Does nothing before the run loop timer exists, exactly as
/// the C returns early on `!runLoopTimer` (`:833-835`).
unsafe fn set_timer_impl(time: *const TclTime) {
    let n = state();
    if n.run_loop_timer.is_null() {
        return;
    }
    let wait = if time.is_null() {
        CF_TIMEINTERVAL_FOREVER
    } else if (*time).sec != 0 || (*time).usec != 0 {
        // TIP #233 scales virtual time here (`:840`). No scale proc can be
        // registered through this host, so the scaling is the identity.
        (*time).sec as f64 + 1.0e-6 * (*time).usec as f64
    } else {
        0.0
    };
    CFRunLoopTimerSetNextFireDate(n.run_loop_timer, CFAbsoluteTimeGetCurrent() + wait);
}

/// `TclpServiceModeHook` (`macosx/tclMacOSXNotify.c:892-912`).
///
/// The run loop timer is created the first time the service mode becomes
/// `TCL_SERVICE_ALL` and never destroyed. The C also starts the `select()`
/// thread here (`:909`); this port has no such thread.
unsafe fn service_mode_hook_impl(mode: c_int) {
    let n = state();
    if mode == TCL_SERVICE_ALL && n.run_loop_timer.is_null() {
        assert!(
            !n.run_loop.is_null(),
            "Tcl_ServiceModeHook: notifier not initialized"
        );
        let mut context = CFContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let timer = CFRunLoopTimerCreate(
            ptr::null(),
            CFAbsoluteTimeGetCurrent() + CF_TIMEINTERVAL_FOREVER,
            CF_TIMEINTERVAL_FOREVER,
            0,
            0,
            timer_wake_up,
            &mut context,
        );
        if !timer.is_null() {
            CFRunLoopAddTimer(n.run_loop, timer, kCFRunLoopCommonModes);
            n.run_loop_timer = timer;
        }
    }
}

/// `TclpWaitForEvent` (`macosx/tclMacOSXNotify.c:1184-1279`).
///
/// Returns -1 if the wait would block forever with nothing to wake it, 1 if a
/// run loop source that is not Tcl's was serviced, 0 otherwise
/// (`generic/tclNotify.c:1350-1351`).
unsafe fn wait_for_event_impl(time: *const TclTime) -> c_int {
    let n = state();
    assert!(
        !n.run_loop.is_null(),
        "Tcl_WaitForEvent: notifier not initialized"
    );

    let mut wait_time = CF_TIMEINTERVAL_FOREVER;
    if !time.is_null() {
        if (*time).sec != 0 || (*time).usec != 0 {
            wait_time = (*time).sec as f64 + 1.0e-6 * (*time).usec as f64;
        } else {
            // A zero block time is a poll. Passing 0 to CFRunLoopRunInMode
            // makes it service at most one source per pass, which loses
            // events; the C uses a small positive interval instead unless
            // another run loop is already running (`:1219-1235`).
            wait_time = if n.run_loop_running { 0.0 } else { 0.0001 };
        }
    }

    n.run_loop_source_performed = false;

    // A recursive wait runs in the private mode, so that it cannot dispatch a
    // source belonging to the wait below it (`:1245-1259`).
    let was_running = n.run_loop_running;
    n.run_loop_running = true;
    let mode = if was_running {
        n.events_only_mode
    } else {
        kCFRunLoopDefaultMode
    };
    let status = CFRunLoopRunInMode(mode, wait_time, 1);
    let n = state();
    n.run_loop_running = was_running;

    match status {
        K_CF_RUN_LOOP_RUN_FINISHED => panic!("Tcl_WaitForEvent: CFRunLoop finished"),
        K_CF_RUN_LOOP_RUN_TIMED_OUT => {
            queue_file_events(ptr::null_mut());
            0
        }
        K_CF_RUN_LOOP_RUN_STOPPED | K_CF_RUN_LOOP_RUN_HANDLED_SOURCE => {
            if state().run_loop_source_performed {
                0
            } else {
                1
            }
        }
        _ => -1,
    }
}

/// `TclpAlertNotifier` (`macosx/tclMacOSXNotify.c:793-805`): wake this thread's
/// run loop from anywhere.
unsafe fn alert_impl(n: &mut Notifier) {
    if !n.run_loop.is_null() {
        CFRunLoopSourceSignal(n.run_loop_source);
        CFRunLoopWakeUp(n.run_loop);
    }
}

/// `QueueFileEvents` (`macosx/tclMacOSXNotify.c:1297-1351`), the run loop
/// source's perform callback.
///
/// One event per handler whose ready mask was zero, and the ready mask is
/// written afterwards so a second readiness while an event is still queued does
/// not queue a second one.
unsafe extern "C" fn queue_file_events(_info: *mut c_void) {
    let n = state();
    n.run_loop_source_performed = true;

    // Collected first: queueing an event does not call out, but taking the
    // descriptors out of the borrow keeps the two loops independent.
    let ready: Vec<(usize, c_int)> = n
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.ready_mask != 0)
        .map(|(i, f)| (i, f.fd))
        .collect();

    for (i, fd) in ready {
        if n.files[i].ready_mask == 0 {
            continue;
        }
        if !n.files[i].event_queued {
            let ev = libc::malloc(std::mem::size_of::<FileHandlerEvent>()) as *mut FileHandlerEvent;
            assert!(!ev.is_null(), "out of memory queueing a file event");
            ptr::write(
                ev,
                FileHandlerEvent {
                    header: TclEvent {
                        proc_: Some(file_handler_event_proc),
                        next_ptr: ptr::null_mut(),
                    },
                    fd,
                },
            );
            n.files[i].event_queued = true;
            queue_event(n, ev as *mut TclEvent, TCL_QUEUE_TAIL);
        }
    }
}

/// The `CFFileDescriptor` callout: record what the descriptor is ready for and
/// hand the rest to the run loop source, which is where the C's `select()`
/// thread hands off too (`macosx/tclMacOSXNotify.c:1946-1955`).
unsafe extern "C" fn file_ready(f: CFFileDescriptorRef, types: CFOptionFlags, _info: *mut c_void) {
    let n = state();
    let Some(i) = n.files.iter().position(|h| h.cf_fd == f) else {
        return;
    };
    let mut mask = 0;
    if types & K_CF_FD_READ != 0 {
        mask |= TCL_READABLE;
    }
    if types & K_CF_FD_WRITE != 0 {
        mask |= TCL_WRITABLE;
    }
    n.files[i].ready_mask |= mask & n.files[i].mask;
    // CoreFoundation disables a callback once it has fired; the handler is
    // re-armed when it runs, in `file_handler_event_proc`.
    alert_impl(n);
}

/// `FileHandlerEventProc` (`macosx/tclMacOSXNotify.c:1085-1140`).
///
/// Returning 0 without `TCL_FILE_EVENTS` leaves the event on the queue rather
/// than discarding it, which is what lets `update idletasks` run without
/// swallowing pending file events.
unsafe extern "C" fn file_handler_event_proc(ev: *mut TclEvent, flags: c_int) -> c_int {
    if flags & TCL_FILE_EVENTS == 0 {
        return 0;
    }
    let fd = (*(ev as *mut FileHandlerEvent)).fd;
    let n = state();
    let Some(i) = n.files.iter().position(|h| h.fd == fd) else {
        return 1;
    };
    n.files[i].event_queued = false;
    // The wanted mask may have changed since the event was queued, so the two
    // are intersected here rather than when it was queued (`:1112-1114`).
    let mask = n.files[i].ready_mask & n.files[i].mask;
    n.files[i].ready_mask = 0;
    if mask != 0 {
        let (proc_, client_data) = (n.files[i].proc_, n.files[i].client_data);
        rearm(&n.files[i]);
        proc_(client_data, mask);
    } else {
        rearm(&n.files[i]);
    }
    1
}

/// Re-enable the CoreFoundation callbacks a handler wants. They are one-shot,
/// so this runs after every delivery.
unsafe fn rearm(h: &FileHandler) {
    let mut types = 0;
    if h.mask & TCL_READABLE != 0 {
        types |= K_CF_FD_READ;
    }
    if h.mask & TCL_WRITABLE != 0 {
        types |= K_CF_FD_WRITE;
    }
    if types != 0 {
        CFFileDescriptorEnableCallBacks(h.cf_fd, types);
    }
}

// ---------------------------------------------------------------------------
// The generic layer (`generic/tclNotify.c`)
// ---------------------------------------------------------------------------

/// `QueueEvent` (`generic/tclNotify.c:480-541`).
///
/// Returns whether the queue was empty before the insertion, which is what
/// `TCL_QUEUE_ALERT_IF_EMPTY` asks for.
unsafe fn queue_event(n: &mut Notifier, ev: *mut TclEvent, position: c_int) -> bool {
    match position & 3 {
        TCL_QUEUE_TAIL => {
            (*ev).next_ptr = ptr::null_mut();
            if n.first_event.is_null() {
                n.first_event = ev;
            } else {
                (*n.last_event).next_ptr = ev;
            }
            n.last_event = ev;
        }
        TCL_QUEUE_HEAD => {
            (*ev).next_ptr = n.first_event;
            if n.first_event.is_null() {
                n.last_event = ev;
            }
            n.first_event = ev;
        }
        TCL_QUEUE_MARK => {
            // Behind every high-priority event already queued, in front of
            // everything else. Tk generates the Enter/Leave sequence of a grab
            // this way (`generic/tclNotify.c:44-50`).
            if n.marker_event.is_null() {
                (*ev).next_ptr = n.first_event;
                n.first_event = ev;
            } else {
                (*ev).next_ptr = (*n.marker_event).next_ptr;
                (*n.marker_event).next_ptr = ev;
            }
            n.marker_event = ev;
            if (*ev).next_ptr.is_null() {
                n.last_event = ev;
            }
        }
        _ => return false,
    }
    let was_empty = position & TCL_QUEUE_ALERT_IF_EMPTY != 0 && n.event_count <= 0;
    n.event_count += 1;
    was_empty
}

/// `Tcl_ServiceEvent` (`generic/tclNotify.c:646-777`).
///
/// The two subtleties the C comment calls out are both reproduced: `proc` is
/// nulled before the handler runs so a re-entered loop skips it, and the queue
/// is searched again from the front afterwards because the handler may have
/// changed it arbitrarily.
unsafe fn service_event(mut flags: c_int) -> c_int {
    // `Tcl_AsyncReady()` — always false here; see the module documentation.
    if flags & TCL_ALL_EVENTS == 0 {
        flags |= TCL_ALL_EVENTS;
    }

    let n = state();
    let mut ev = n.first_event;
    while !ev.is_null() {
        let next = (*ev).next_ptr;
        let Some(proc_) = (*ev).proc_ else {
            ev = next;
            continue;
        };
        (*ev).proc_ = None;

        // The count is remembered and zeroed so a nested Tcl_ServiceEvent sees
        // an empty queue for the purposes of TCL_QUEUE_ALERT_IF_EMPTY, and is
        // added back on the way out (`generic/tclNotify.c:716-727`).
        let saved = n.event_count;
        n.event_count = 0;
        let handled = proc_(ev, flags);
        let n = state();
        n.event_count += saved;

        if handled == 0 {
            // Not handled: put the handler back so it can be tried again.
            (*ev).proc_ = Some(proc_);
            ev = (*ev).next_ptr;
            continue;
        }

        // Handled: unlink it, searching from the front because the queue may
        // be a different queue by now.
        let mut unlinked = false;
        if n.first_event == ev {
            n.first_event = (*ev).next_ptr;
            if (*ev).next_ptr.is_null() {
                n.last_event = ptr::null_mut();
            }
            if n.marker_event == ev {
                n.marker_event = ptr::null_mut();
            }
            unlinked = true;
        } else {
            let mut prev = n.first_event;
            while !prev.is_null() && (*prev).next_ptr != ev {
                prev = (*prev).next_ptr;
            }
            if !prev.is_null() {
                (*prev).next_ptr = (*ev).next_ptr;
                if (*ev).next_ptr.is_null() {
                    n.last_event = prev;
                }
                if n.marker_event == ev {
                    n.marker_event = prev;
                }
                unlinked = true;
            }
        }
        if unlinked {
            libc::free(ev as *mut c_void);
            n.event_count -= 1;
        }
        return 1;
    }
    0
}

/// The list of sources to walk, as it stands right now.
///
/// See the module documentation for why this is a copy: the C reads the next
/// pointer out of a node the callee may already have freed.
unsafe fn source_snapshot(n: &Notifier) -> Vec<EventSource> {
    n.sources.clone()
}

/// Run every source's setup function, skipping any that has been deleted since
/// the snapshot was taken (`generic/tclNotify.c:983-990`).
unsafe fn setup_sources(flags: c_int) {
    let n = state();
    n.in_traversal = true;
    for s in source_snapshot(n) {
        let n = state();
        if !n.sources.iter().any(|live| live.same(&s)) {
            continue;
        }
        if let Some(f) = s.setup {
            f(s.client_data, flags);
        }
    }
    state().in_traversal = false;
}

/// Run every source's check function (`generic/tclNotify.c:1013-1018`).
unsafe fn check_sources(flags: c_int) {
    let n = state();
    for s in source_snapshot(n) {
        let n = state();
        if !n.sources.iter().any(|live| live.same(&s)) {
            continue;
        }
        if let Some(f) = s.check {
            f(s.client_data, flags);
        }
    }
}

/// `Tcl_DoOneEvent` (`generic/tclNotify.c:900-1066`).
unsafe fn do_one_event(mut flags: c_int) -> c_int {
    let mut result = 0;
    if flags & TCL_ALL_EVENTS == 0 {
        flags |= TCL_ALL_EVENTS;
    }

    // Servicing is turned off for the duration so a notifier callback cannot
    // recurse into the queue (`generic/tclNotify.c:930-936`).
    let old_mode = state().service_mode;
    state().service_mode = TCL_SERVICE_NONE;

    loop {
        // Idle events on their own never block, whatever the flags say.
        let idle_only = flags & TCL_ALL_EVENTS == TCL_IDLE_EVENTS;
        if idle_only {
            flags = TCL_IDLE_EVENTS | TCL_DONT_WAIT;
        } else {
            if service_event(flags) != 0 {
                result = 1;
                break;
            }

            let n = state();
            if flags & TCL_DONT_WAIT != 0 {
                n.block_time = TclTime { sec: 0, usec: 0 };
                n.block_time_set = true;
            } else {
                n.block_time_set = false;
            }

            setup_sources(flags);

            let n = state();
            let wait_for = if flags & TCL_DONT_WAIT != 0 || n.block_time_set {
                &n.block_time as *const TclTime
            } else {
                ptr::null()
            };

            result = wait_for_event_impl(wait_for);
            if result < 0 {
                result = 0;
                break;
            }

            check_sources(flags);

            if service_event(flags) != 0 {
                result = 1;
                break;
            }
        }

        // idleEvents:
        if flags & TCL_IDLE_EVENTS != 0 && service_idle() != 0 {
            result = 1;
            break;
        }
        if flags & TCL_DONT_WAIT != 0 {
            break;
        }
        // A system event was dispatched, which may have run Tcl code; return so
        // that a `vwait` gets a chance to notice its variable changed
        // (`generic/tclNotify.c:1046-1061`).
        if result != 0 {
            break;
        }
    }

    state().service_mode = old_mode;
    result
}

/// `Tcl_ServiceAll` (`generic/tclNotify.c:1088-1151`).
///
/// One pass over every source, then the whole queue, then the idle handlers.
/// The notifier timer is updated once at the end rather than after each step.
unsafe fn service_all() -> c_int {
    let mut result = 0;
    let n = state();
    if n.service_mode == TCL_SERVICE_NONE {
        return 0;
    }
    n.service_mode = TCL_SERVICE_NONE;
    n.in_traversal = true;
    n.block_time_set = false;

    setup_sources_all();
    check_sources(TCL_ALL_EVENTS);

    while service_event(0) != 0 {
        result = 1;
    }
    if service_idle() != 0 {
        result = 1;
    }

    let n = state();
    if n.block_time_set {
        let t = TclTime {
            sec: n.block_time.sec,
            usec: n.block_time.usec,
        };
        set_timer_impl(&t);
    } else {
        set_timer_impl(ptr::null());
    }
    let n = state();
    n.in_traversal = false;
    n.service_mode = TCL_SERVICE_ALL;
    result
}

/// The setup pass `Tcl_ServiceAll` makes, which does not touch `inTraversal`
/// because the caller already owns it (`generic/tclNotify.c:1120-1128`).
unsafe fn setup_sources_all() {
    let n = state();
    for s in source_snapshot(n) {
        let n = state();
        if !n.sources.iter().any(|live| live.same(&s)) {
            continue;
        }
        if let Some(f) = s.setup {
            f(s.client_data, TCL_ALL_EVENTS);
        }
    }
}

/// `Tcl_SetMaxBlockTime` (`generic/tclNotify.c:852-875`).
///
/// Only ever lowers the block time: several sources ask, and the shortest one
/// wins.
unsafe fn set_max_block_time(time: *const TclTime) {
    let n = state();
    if !n.block_time_set
        || (*time).sec < n.block_time.sec
        || ((*time).sec == n.block_time.sec && (*time).usec < n.block_time.usec)
    {
        n.block_time = TclTime {
            sec: (*time).sec,
            usec: (*time).usec,
        };
        n.block_time_set = true;
    }
    if !n.in_traversal {
        let t = TclTime {
            sec: n.block_time.sec,
            usec: n.block_time.usec,
        };
        set_timer_impl(&t);
    }
}

// ---------------------------------------------------------------------------
// The timer layer (`generic/tclTimer.c`)
// ---------------------------------------------------------------------------

/// `InitTimer` (`generic/tclTimer.c:180-191`): registers the timer event source
/// the first time a timer or idle handler is created.
///
/// Timers are not privileged. They reach `Tcl_DoOneEvent` through exactly the
/// same setup/check pair any other source uses, and that is why an `after`
/// callback and a window event interleave the way they do.
unsafe fn init_timer() -> &'static mut Notifier {
    let n = state();
    if !n.timer_source_registered {
        n.timer_source_registered = true;
        n.sources.push(EventSource {
            setup: Some(timer_setup_proc),
            check: Some(timer_check_proc),
            client_data: ptr::null_mut(),
        });
    }
    n
}

/// The current time, as `Tcl_GetTime` reports it (`generic/tclUnixTime.c`).
unsafe fn now() -> TclTime {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    libc::gettimeofday(&mut tv, ptr::null_mut());
    TclTime {
        sec: tv.tv_sec,
        usec: tv.tv_usec as std::ffi::c_long,
    }
}

/// `TCL_TIME_BEFORE` (`generic/tclTimer.c:121-122`).
fn time_before(a: &TclTime, b: &TclTime) -> bool {
    a.sec < b.sec || (a.sec == b.sec && a.usec < b.usec)
}

/// How long until `at`, clamped at zero (`generic/tclTimer.c:417-428`).
fn until(at: &TclTime, from: &TclTime) -> TclTime {
    let mut sec = at.sec - from.sec;
    let mut usec = at.usec - from.usec;
    if usec < 0 {
        sec -= 1;
        usec += 1_000_000;
    }
    if sec < 0 {
        return TclTime { sec: 0, usec: 0 };
    }
    TclTime { sec, usec }
}

/// `TclCreateAbsoluteTimerHandler` (`generic/tclTimer.c:289-330`).
///
/// The list stays sorted by fire time, and an insertion goes *after* every
/// handler with the same time, so two `after 0` callbacks run in the order they
/// were registered.
unsafe fn create_timer_at(at: TclTime, proc_: TclTimerProc, client_data: *mut c_void) -> c_int {
    let n = init_timer();
    n.last_timer_id += 1;
    let token = n.last_timer_id;
    let pos = n
        .timers
        .iter()
        .position(|t| time_before(&at, &t.time))
        .unwrap_or(n.timers.len());
    n.timers.insert(
        pos,
        TimerHandler {
            time: at,
            proc_,
            client_data,
            token,
        },
    );
    timer_setup_proc(ptr::null_mut(), TCL_ALL_EVENTS);
    token
}

/// `TimerSetupProc` (`generic/tclTimer.c:396-434`): tell the notifier how long
/// it may block.
unsafe extern "C" fn timer_setup_proc(_client_data: *mut c_void, flags: c_int) {
    let n = state();
    let block = if (flags & TCL_IDLE_EVENTS != 0 && !n.idles.is_empty())
        || (flags & TCL_TIMER_EVENTS != 0 && n.timer_pending)
    {
        // Work is already waiting, so do not block at all.
        TclTime { sec: 0, usec: 0 }
    } else if flags & TCL_TIMER_EVENTS != 0 && !n.timers.is_empty() {
        until(&n.timers[0].time, &now())
    } else {
        return;
    };
    set_max_block_time(&block);
}

/// `TimerCheckProc` (`generic/tclTimer.c:454-493`): queue one event when the
/// earliest timer is due.
///
/// One event covers every expired timer; `TimerHandlerEventProc` drains them.
unsafe extern "C" fn timer_check_proc(_client_data: *mut c_void, flags: c_int) {
    let n = state();
    if flags & TCL_TIMER_EVENTS == 0 || n.timers.is_empty() {
        return;
    }
    let block = until(&n.timers[0].time, &now());
    if block.sec == 0 && block.usec == 0 && !n.timer_pending {
        n.timer_pending = true;
        let ev = libc::malloc(std::mem::size_of::<TclEvent>()) as *mut TclEvent;
        assert!(!ev.is_null(), "out of memory queueing a timer event");
        ptr::write(
            ev,
            TclEvent {
                proc_: Some(timer_handler_event_proc),
                next_ptr: ptr::null_mut(),
            },
        );
        queue_event(n, ev, TCL_QUEUE_TAIL);
    }
}

/// `TimerHandlerEventProc` (`generic/tclTimer.c:516-595`).
///
/// Three rules, all of them load-bearing and all of them from the C: a handler
/// is unlinked before it is called, so a handler that re-enters the event loop
/// cannot run itself again; a handler created during the drain is not run in
/// the same drain, which is what the token comparison tests; and the whole
/// thing refuses without `TCL_TIMER_EVENTS`, leaving the event queued.
unsafe extern "C" fn timer_handler_event_proc(_ev: *mut TclEvent, flags: c_int) -> c_int {
    if flags & TCL_TIMER_EVENTS == 0 {
        return 0;
    }
    let n = state();
    n.timer_pending = false;
    let current_id = n.last_timer_id;
    let time = now();
    loop {
        let n = state();
        let Some(first) = n.timers.first() else { break };
        if time_before(&time, &first.time) {
            break;
        }
        // A timer created since this drain started has a newer token.
        if current_id.wrapping_sub(first.token) < 0 {
            break;
        }
        let handler = n.timers.remove(0);
        (handler.proc_)(handler.client_data);
    }
    timer_setup_proc(ptr::null_mut(), TCL_TIMER_EVENTS);
    1
}

/// `TclServiceIdle` (`generic/tclTimer.c:707-756`).
///
/// Runs the handlers that were present when it started, and no others: a
/// handler that schedules another idle handler does not spin the loop.
unsafe fn service_idle() -> c_int {
    let n = state();
    if n.idles.is_empty() {
        return 0;
    }
    let old_generation = n.idle_generation;
    n.idle_generation += 1;

    loop {
        let n = state();
        let Some(first) = n.idles.first() else { break };
        if old_generation.wrapping_sub(first.generation) < 0 {
            break;
        }
        let handler = n.idles.remove(0);
        (handler.proc_)(handler.client_data);
    }
    let n = state();
    if !n.idles.is_empty() {
        set_max_block_time(&TclTime { sec: 0, usec: 0 });
    }
    1
}

// ---------------------------------------------------------------------------
// The slots
//
// Each is `pub` as well as installed in the table. An event source is not
// something only Tk can be: anything hosting this notifier — a channel driver,
// a test — queues events and creates timers through the same entry points, and
// hiding them behind the table would mean the only way to drive the loop is to
// be a dynamically loaded C library.
// ---------------------------------------------------------------------------

/// Storage for an event, from the allocator that will free it.
///
/// An event handed to `Tcl_QueueEvent` becomes the queue's property and is
/// released with `Tcl_Free` once it has been serviced
/// (`generic/tclNotify.c:392-396`), so it cannot come from Rust's allocator.
/// `size` is the size of the caller's whole struct, whose first member must be
/// a [`TclEvent`].
///
/// # Safety
/// `size` must be at least `size_of::<TclEvent>()`, and the caller must fill in
/// `proc_` before queueing.
pub unsafe fn alloc_event(size: usize) -> *mut TclEvent {
    assert!(size >= std::mem::size_of::<TclEvent>());
    let p = libc::malloc(size) as *mut TclEvent;
    assert!(!p.is_null(), "out of memory allocating a Tcl_Event");
    (*p).proc_ = None;
    (*p).next_ptr = ptr::null_mut();
    p
}

macro_rules! entered {
    ($name:literal) => {
        record(
            Table::Tcl,
            TCL_NAMES
                .iter()
                .position(|n| *n == $name)
                .expect("no such slot"),
        )
    };
}

/// Slot 92. `void Tcl_CreateEventSource(Tcl_EventSetupProc *, Tcl_EventCheckProc *, void *)`
/// (`generic/tclDecls.h:1983`; body at `generic/tclNotify.c:303-322`).
///
/// # Safety
/// `setup` and `check` are kept and called until the source is deleted, so
/// both must outlive it; `client_data` is handed back to them unread.
pub unsafe extern "C" fn create_event_source(
    setup: Option<TclEventSetupProc>,
    check: Option<TclEventCheckProc>,
    client_data: *mut c_void,
) {
    entered!("tcl_CreateEventSource");
    state().sources.push(EventSource {
        setup,
        check,
        client_data,
    });
}

/// Slot 106. `void Tcl_DeleteEventSource(...)` (`generic/tclDecls.h:1997`;
/// body at `generic/tclNotify.c:342-372`). Deleting a source that was never
/// registered is not an error.
///
/// # Safety
/// The three arguments are compared against the registered sources and never
/// called, so a triple that was never registered is harmless.
pub unsafe extern "C" fn delete_event_source(
    setup: Option<TclEventSetupProc>,
    check: Option<TclEventCheckProc>,
    client_data: *mut c_void,
) {
    entered!("tcl_DeleteEventSource");
    let want = EventSource {
        setup,
        check,
        client_data,
    };
    let n = state();
    if let Some(i) = n.sources.iter().position(|s| s.same(&want)) {
        n.sources.remove(i);
    }
}

/// Slot 205. `void Tcl_QueueEvent(Tcl_Event *evPtr, int position)`
/// (`generic/tclDecls.h:2096`; body at `generic/tclNotify.c:390-403`).
///
/// The event's storage becomes the queue's property and is freed with
/// `Tcl_Free` once serviced, which is why it must have come from `Tcl_Alloc`.
///
/// # Safety
/// `ev` must point at a `Tcl_Event`-headed struct allocated with `Tcl_Alloc`
/// and must have its `proc` set. The queue takes ownership and frees it.
pub unsafe extern "C" fn queue_event_slot(ev: *mut TclEvent, position: c_int) {
    entered!("tcl_QueueEvent");
    queue_event(state(), ev, position);
}

/// Slot 319. `void Tcl_ThreadQueueEvent(Tcl_ThreadId, Tcl_Event *, int)`
/// (`generic/tclDecls.h:2210`; body at `generic/tclNotify.c:421-456`).
///
/// An event addressed to a thread with no notifier is freed rather than queued.
///
/// # Safety
/// As `queue_event_slot`. An event addressed to a thread with no notifier is
/// freed here rather than queued, so it is owned either way.
pub unsafe extern "C" fn thread_queue_event(
    thread_id: *mut c_void,
    ev: *mut TclEvent,
    position: c_int,
) {
    entered!("tcl_ThreadQueueEvent");
    let n = state();
    if n.thread_id != thread_id {
        libc::free(ev as *mut c_void);
        return;
    }
    if queue_event(n, ev, position) {
        alert_impl(n);
    }
}

/// Slot 318. `void Tcl_ThreadAlert(Tcl_ThreadId)` (`generic/tclDecls.h:2209`;
/// body at `generic/tclNotify.c:1170-1190`).
///
/// # Safety
/// `thread_id` is compared, never dereferenced.
pub unsafe extern "C" fn thread_alert(thread_id: *mut c_void) {
    entered!("tcl_ThreadAlert");
    let n = state();
    if n.thread_id == thread_id {
        alert_impl(n);
    }
}

/// Slot 300. `Tcl_ThreadId Tcl_GetCurrentThread(void)`
/// (`generic/tclDecls.h:2191`). Tcl's `Tcl_ThreadId` is the platform thread
/// handle (`generic/tclUnixThrd.c`), which on a pthreads platform is
/// `pthread_self()`.
///
/// # Safety
/// Nothing. The signature is `unsafe` because every slot in the table is.
pub unsafe extern "C" fn get_current_thread() -> *mut c_void {
    entered!("tcl_GetCurrentThread");
    libc::pthread_self() as *mut c_void
}

/// Slot 105. `void Tcl_DeleteEvents(Tcl_EventDeleteProc *, void *)`
/// (`generic/tclDecls.h:1996`; body at `generic/tclNotify.c:562-624`).
///
/// Deletes every event the callback returns 1 for. Tk uses it to drop the
/// events of a window it is destroying.
///
/// # Safety
/// `proc` is called once for each queued event and must not free it; returning
/// 1 is what frees it, here.
pub unsafe extern "C" fn delete_events(proc_: TclEventDeleteProc, client_data: *mut c_void) {
    entered!("tcl_DeleteEvents");
    let n = state();
    let mut prev: *mut TclEvent = ptr::null_mut();
    let mut ev = n.first_event;
    while !ev.is_null() {
        let next = (*ev).next_ptr;
        if proc_(ev, client_data) == 1 {
            let n = state();
            if prev.is_null() {
                n.first_event = next;
            } else {
                (*prev).next_ptr = next;
            }
            if next.is_null() {
                n.last_event = prev;
            }
            if n.marker_event == ev {
                n.marker_event = prev;
            }
            libc::free(ev as *mut c_void);
            n.event_count -= 1;
        } else {
            prev = ev;
        }
        ev = next;
    }
}

/// Slot 222. `int Tcl_ServiceEvent(int flags)` (`generic/tclDecls.h:2113`).
///
/// # Safety
/// Runs whatever handler the head of the queue carries, on the thread that owns
/// the notifier.
pub unsafe extern "C" fn service_event_slot(flags: c_int) -> c_int {
    entered!("tcl_ServiceEvent");
    service_event(flags)
}

/// Slot 221. `int Tcl_ServiceAll(void)` (`generic/tclDecls.h:2112`).
///
/// # Safety
/// As `service_event_slot`, for the whole queue and the idle handlers.
pub unsafe extern "C" fn service_all_slot() -> c_int {
    entered!("tcl_ServiceAll");
    service_all()
}

/// Slot 115. `int Tcl_DoOneEvent(int flags)` (`generic/tclDecls.h:2006`).
///
/// The whole point of the file. `Tk_MainLoop` is a loop around this
/// (`tk9.0.4/generic/tkEvent.c`), `update` is one call with `TCL_DONT_WAIT`,
/// and `vwait` calls it until the variable it watches is written.
///
/// # Safety
/// Runs arbitrary event, timer and idle handlers, and blocks unless
/// `TCL_DONT_WAIT` is set. Must run on the thread that owns the notifier.
pub unsafe extern "C" fn do_one_event_slot(flags: c_int) -> c_int {
    entered!("tcl_DoOneEvent");
    do_one_event(flags)
}

/// Slot 171. `int Tcl_GetServiceMode(void)` (`generic/tclDecls.h:2062`).
///
/// # Safety
/// Must run on the thread that owns the notifier.
pub unsafe extern "C" fn get_service_mode() -> c_int {
    entered!("tcl_GetServiceMode");
    state().service_mode
}

/// Slot 233. `int Tcl_SetServiceMode(int mode)` (`generic/tclDecls.h:2124`;
/// body at `generic/tclNotify.c:819-831`). Returns the previous mode and runs
/// the platform hook.
///
/// # Safety
/// Must run on the thread that owns the notifier.
pub unsafe extern "C" fn set_service_mode(mode: c_int) -> c_int {
    entered!("tcl_SetServiceMode");
    let n = state();
    let old = n.service_mode;
    n.service_mode = mode;
    service_mode_hook_impl(mode);
    old
}

/// Slot 344. `void Tcl_ServiceModeHook(int mode)` (`generic/tclDecls.h:2235`).
///
/// # Safety
/// Must run on the thread that owns the notifier.
pub unsafe extern "C" fn service_mode_hook(mode: c_int) {
    entered!("tcl_ServiceModeHook");
    service_mode_hook_impl(mode);
}

/// Slot 343. `void Tcl_AlertNotifier(void *clientData)`
/// (`generic/tclDecls.h:2234`).
///
/// # Safety
/// `clientData` is ignored: this host has one notifier per thread and reaches
/// it through the thread, not through the handle.
pub unsafe extern "C" fn alert_notifier(_client_data: *mut c_void) {
    entered!("tcl_AlertNotifier");
    alert_impl(state());
}

/// Slot 229. `void Tcl_SetMaxBlockTime(const Tcl_Time *)`
/// (`generic/tclDecls.h:2120`).
///
/// # Safety
/// `time` must point at a readable `Tcl_Time`. Unlike `Tcl_SetTimer` it may not
/// be NULL — the C dereferences it unconditionally.
pub unsafe extern "C" fn set_max_block_time_slot(time: *const TclTime) {
    entered!("tcl_SetMaxBlockTime");
    set_max_block_time(time);
}

/// Slot 11. `void Tcl_SetTimer(const Tcl_Time *timePtr)`
/// (`generic/tclDecls.h:1902`).
///
/// # Safety
/// `time` must point at a readable `Tcl_Time`, or be NULL for "no timeout".
pub unsafe extern "C" fn set_timer(time: *const TclTime) {
    entered!("tcl_SetTimer");
    set_timer_impl(time);
}

/// Slot 13. `int Tcl_WaitForEvent(const Tcl_Time *timePtr)`
/// (`generic/tclDecls.h:1904`).
///
/// # Safety
/// `time` must point at a readable `Tcl_Time`, or be NULL to wait forever.
/// Blocks, and dispatches run loop sources while it does.
pub unsafe extern "C" fn wait_for_event(time: *const TclTime) -> c_int {
    entered!("tcl_WaitForEvent");
    wait_for_event_impl(time)
}

/// Slot 12. `void Tcl_Sleep(int ms)` (`generic/tclDecls.h:1903`; body at
/// `macosx/tclMacOSXNotify.c:1478-1545`).
///
/// Not a `nanosleep`. A sleep on a thread with a run loop keeps running the
/// run loop, so that a window still redraws while a script sleeps — but with
/// `returnAfterSourceHandled` false, so no Tcl event is serviced and the sleep
/// is not a disguised `update`. The notifier's own timer is pushed out of the
/// way for the duration and restored afterwards, so a `Tcl_SetTimer` from
/// before the sleep does not cut it short.
///
/// # Safety
/// Runs the platform run loop, so anything already scheduled on it may run.
/// Must be called on the thread that owns the notifier.
pub unsafe extern "C" fn sleep_ms(ms: c_int) {
    entered!("tcl_Sleep");
    if ms <= 0 {
        return;
    }
    let n = state();
    if n.run_loop.is_null() {
        let mut want = libc::timespec {
            tv_sec: (ms / 1000) as libc::time_t,
            tv_nsec: ((ms % 1000) * 1_000_000) as std::ffi::c_long,
        };
        while libc::nanosleep(&want, &mut want) != 0 {}
        return;
    }

    let mut wait_time = ms as f64 / 1000.0;
    let now = CFAbsoluteTimeGetCurrent();
    let wait_end = now + wait_time;

    let mut restore = ptr::null_mut();
    let mut next_fire = 0.0;
    if !n.run_loop_timer.is_null() {
        next_fire = CFRunLoopTimerGetNextFireDate(n.run_loop_timer);
        if next_fire < wait_end {
            restore = n.run_loop_timer;
            CFRunLoopTimerSetNextFireDate(restore, now + CF_TIMEINTERVAL_FOREVER);
        }
    }

    state().sleeping = true;
    while wait_time > 0.0 {
        match CFRunLoopRunInMode(kCFRunLoopDefaultMode, wait_time, 0) {
            K_CF_RUN_LOOP_RUN_FINISHED => panic!("Tcl_Sleep: CFRunLoop finished"),
            K_CF_RUN_LOOP_RUN_STOPPED => wait_time = wait_end - CFAbsoluteTimeGetCurrent(),
            _ => wait_time = 0.0,
        }
    }
    state().sleeping = false;
    if !restore.is_null() {
        CFRunLoopTimerSetNextFireDate(restore, next_fire);
    }
}

/// Slot 98. `Tcl_TimerToken Tcl_CreateTimerHandler(int ms, Tcl_TimerProc *, void *)`
/// (`generic/tclDecls.h:1989`; body at `generic/tclTimer.c:247-268`).
///
/// The token is the integer id cast to a pointer, so it is never NULL — a NULL
/// token means "no timer" to `Tcl_DeleteTimerHandler`
/// (`generic/tclTimer.c:358-360`).
///
/// # Safety
/// `proc` is called once, later, on this thread; it and whatever `client_data`
/// points at must still be valid then, or the timer must be deleted first.
pub unsafe extern "C" fn create_timer_handler(
    milliseconds: c_int,
    proc_: TclTimerProc,
    client_data: *mut c_void,
) -> *mut c_void {
    entered!("tcl_CreateTimerHandler");
    let mut at = now();
    at.sec += (milliseconds / 1000) as i64;
    at.usec += ((milliseconds % 1000) * 1000) as std::ffi::c_long;
    if at.usec >= 1_000_000 {
        at.usec -= 1_000_000;
        at.sec += 1;
    }
    create_timer_at(at, proc_, client_data) as usize as *mut c_void
}

/// Slot 112. `void Tcl_DeleteTimerHandler(Tcl_TimerToken token)`
/// (`generic/tclDecls.h:2003`; body at `generic/tclTimer.c:350-376`). A token
/// that has already fired, or was never issued, is silently ignored.
///
/// # Safety
/// `token` must be one this function returned, or NULL. A token whose timer has
/// already fired is accepted and does nothing.
pub unsafe extern "C" fn delete_timer_handler(token: *mut c_void) {
    entered!("tcl_DeleteTimerHandler");
    if token.is_null() {
        return;
    }
    let want = token as usize as c_int;
    let n = init_timer();
    if let Some(i) = n.timers.iter().position(|t| t.token == want) {
        n.timers.remove(i);
    }
}

/// Slot 116. `void Tcl_DoWhenIdle(Tcl_IdleProc *proc, void *clientData)`
/// (`generic/tclDecls.h:2007`; body at `generic/tclTimer.c:616-640`).
///
/// Appended, not prepended: idle handlers run in registration order, which is
/// what makes Tk's redraw arrive after the geometry change that caused it.
///
/// # Safety
/// As `create_timer_handler`: `proc` runs later, on this thread.
pub unsafe extern "C" fn do_when_idle(proc_: TclIdleProc, client_data: *mut c_void) {
    entered!("tcl_DoWhenIdle");
    let n = init_timer();
    let generation = n.idle_generation;
    n.idles.push(IdleHandler {
        proc_,
        client_data,
        generation,
    });
    set_max_block_time(&TclTime { sec: 0, usec: 0 });
}

/// Slot 80. `void Tcl_CancelIdleCall(Tcl_IdleProc *proc, void *clientData)`
/// (`generic/tclDecls.h:1971`; body at `generic/tclTimer.c:660-686`). Cancels
/// *every* registration of that pair, not just the first.
///
/// # Safety
/// Both arguments are compared, never called.
pub unsafe extern "C" fn cancel_idle_call(proc_: TclIdleProc, client_data: *mut c_void) {
    entered!("tcl_CancelIdleCall");
    let n = init_timer();
    n.idles
        .retain(|h| !(std::ptr::fn_addr_eq(h.proc_, proc_) && h.client_data == client_data));
}

/// Slot 9. `void Tcl_CreateFileHandler(int fd, int mask, Tcl_FileProc *, void *)`
/// (`generic/tclDecls.h:1900`; body at `macosx/tclMacOSXNotify.c:930-979`).
///
/// Registering the same descriptor twice replaces the handler rather than
/// stacking a second one.
///
/// `TCL_EXCEPTION` is accepted and never reported: this port watches
/// descriptors with `CFFileDescriptor`, which has read and write callbacks and
/// no exceptional one. See the module documentation.
///
/// # Safety
/// `fd` must be open and must stay open until the handler is deleted; `proc` is
/// called on this thread while it is.
pub unsafe extern "C" fn create_file_handler(
    fd: c_int,
    mask: c_int,
    proc_: TclFileProc,
    client_data: *mut c_void,
) {
    entered!("tcl_CreateFileHandler");
    let n = state();
    if let Some(i) = n.files.iter().position(|h| h.fd == fd) {
        n.files[i].mask = mask;
        n.files[i].proc_ = proc_;
        n.files[i].client_data = client_data;
        rearm(&n.files[i]);
        return;
    }

    let mut context = CFContext {
        version: 0,
        info: ptr::null_mut(),
        retain: None,
        release: None,
        copy_description: None,
    };
    let cf_fd = CFFileDescriptorCreate(ptr::null(), fd, 0, file_ready, &mut context);
    assert!(!cf_fd.is_null(), "could not watch descriptor {fd}");
    let cf_source = CFFileDescriptorCreateRunLoopSource(ptr::null(), cf_fd, 0);
    assert!(
        !cf_source.is_null(),
        "could not create a source for descriptor {fd}"
    );
    CFRunLoopAddSource(n.run_loop, cf_source, kCFRunLoopCommonModes);
    CFRunLoopAddSource(n.run_loop, cf_source, n.events_only_mode);

    n.files.push(FileHandler {
        fd,
        mask,
        ready_mask: 0,
        event_queued: false,
        proc_,
        client_data,
        cf_fd,
        cf_source,
    });
    let last = n.files.len() - 1;
    rearm(&n.files[last]);
}

/// Slot 10. `void Tcl_DeleteFileHandler(int fd)` (`generic/tclDecls.h:1901`;
/// body at `macosx/tclMacOSXNotify.c:997-1061`). Removing a descriptor that is
/// not watched is not an error.
///
/// # Safety
/// Any descriptor may be passed; one that is not watched is ignored.
pub unsafe extern "C" fn delete_file_handler(fd: c_int) {
    entered!("tcl_DeleteFileHandler");
    let n = state();
    let Some(i) = n.files.iter().position(|h| h.fd == fd) else {
        return;
    };
    let h = n.files.remove(i);
    CFFileDescriptorDisableCallBacks(h.cf_fd, K_CF_FD_READ | K_CF_FD_WRITE);
    CFRunLoopRemoveSource(n.run_loop, h.cf_source, kCFRunLoopCommonModes);
    CFRunLoopRemoveSource(n.run_loop, h.cf_source, n.events_only_mode);
    CFFileDescriptorInvalidate(h.cf_fd);
    CFRelease(h.cf_source);
    CFRelease(h.cf_fd);
}

/// `TclPlatStubs` slot 2. `void Tcl_MacOSXNotifierAddRunLoopMode(const void *)`
/// (`generic/tclPlatDecls.h:101`; body at `macosx/tclMacOSXNotify.c:602-616`).
///
/// The only platform slot referenced anywhere in libtk, and Tk calls it exactly
/// twice, for `NSEventTrackingRunLoopMode` and `NSModalPanelRunLoopMode`
/// (`tk9.0.4/macosx/tkMacOSXNotify.c:270-271`) — which is what keeps Tcl events
/// flowing while a menu is held open or a modal dialog is up.
///
/// # Safety
/// `mode` must be a live `CFStringRef` naming a run loop mode.
pub unsafe extern "C" fn macosx_notifier_add_run_loop_mode(mode: *const c_void) {
    record(
        Table::TclPlat,
        TCL_PLAT_NAMES
            .iter()
            .position(|n| *n == "tcl_MacOSXNotifierAddRunLoopMode")
            .expect("no such plat slot"),
    );
    let n = state();
    if n.run_loop.is_null() {
        return;
    }
    CFRunLoopAddSource(n.run_loop, n.run_loop_source, mode as CFStringRef);
    if !n.run_loop_timer.is_null() {
        CFRunLoopAddTimer(n.run_loop, n.run_loop_timer, mode as CFStringRef);
    }
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// Install `f` at the named slot of `t`, by name and never by index.
///
/// # Safety
/// `f` must have the signature `tclDecls.h` gives that slot; each call below
/// cites the header line it was written from.
unsafe fn install(t: &mut TclStubs, name: &str, f: *const ()) -> usize {
    let i = TCL_NAMES
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no slot named {name} in TclStubs"));
    t.slots[i] = std::mem::transmute::<*const (), RawStub>(f);
    i
}

/// Patch every notifier slot into `t`, returning their indices.
///
/// # Safety
/// See `install`.
pub unsafe fn install_impls(t: &mut TclStubs) -> Vec<usize> {
    vec![
        install(t, "tcl_CreateFileHandler", create_file_handler as *const ()),
        install(t, "tcl_DeleteFileHandler", delete_file_handler as *const ()),
        install(t, "tcl_SetTimer", set_timer as *const ()),
        install(t, "tcl_Sleep", sleep_ms as *const ()),
        install(t, "tcl_WaitForEvent", wait_for_event as *const ()),
        install(t, "tcl_CancelIdleCall", cancel_idle_call as *const ()),
        install(t, "tcl_CreateEventSource", create_event_source as *const ()),
        install(
            t,
            "tcl_CreateTimerHandler",
            create_timer_handler as *const (),
        ),
        install(t, "tcl_DeleteEvents", delete_events as *const ()),
        install(t, "tcl_DeleteEventSource", delete_event_source as *const ()),
        install(
            t,
            "tcl_DeleteTimerHandler",
            delete_timer_handler as *const (),
        ),
        install(t, "tcl_DoOneEvent", do_one_event_slot as *const ()),
        install(t, "tcl_DoWhenIdle", do_when_idle as *const ()),
        install(t, "tcl_GetServiceMode", get_service_mode as *const ()),
        install(t, "tcl_QueueEvent", queue_event_slot as *const ()),
        install(t, "tcl_ServiceAll", service_all_slot as *const ()),
        install(t, "tcl_ServiceEvent", service_event_slot as *const ()),
        install(
            t,
            "tcl_SetMaxBlockTime",
            set_max_block_time_slot as *const (),
        ),
        install(t, "tcl_SetServiceMode", set_service_mode as *const ()),
        install(t, "tcl_GetCurrentThread", get_current_thread as *const ()),
        install(t, "tcl_ThreadAlert", thread_alert as *const ()),
        install(t, "tcl_ThreadQueueEvent", thread_queue_event as *const ()),
        install(t, "tcl_AlertNotifier", alert_notifier as *const ()),
        install(t, "tcl_ServiceModeHook", service_mode_hook as *const ()),
        install(t, "tcl_SetMainLoop", set_main_loop as *const ()),
    ]
}

/// The `Tcl_MainLoopProc *` `Tk_Init` hands over on its way out.
///
/// `void Tk_MainLoop(void)` (`tk9.0.4/generic/tkEvent.c`), which is a
/// `while (Tk_GetNumMainWindows() > 0) Tcl_DoOneEvent(0)`. Storing it is the
/// whole of slot 284's contract, and calling it is how a session with a window
/// stays alive.
static MAIN_LOOP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Slot 284. `void Tcl_SetMainLoop(Tcl_MainLoopProc *proc)`
/// (`generic/tclDecls.h`; body at `generic/tclMain.c:647-654`, which is a
/// single assignment into thread-specific data).
///
/// Tk calls it once, with `Tk_MainLoop`, immediately after providing itself as
/// a package (`tk9.0.4/generic/tkWindow.c:3477`) — so reaching this slot means
/// `Tk_Init` has done everything it is going to do.
///
/// # Safety
/// `proc` is a `Tcl_MainLoopProc *` or NULL. Nothing here calls it.
unsafe extern "C" fn set_main_loop(proc_: *mut c_void) {
    entered!("tcl_SetMainLoop");
    MAIN_LOOP.store(proc_, Ordering::Relaxed);
}

/// The main-loop procedure Tk registered, or NULL if it has not.
pub fn main_loop_proc() -> *mut c_void {
    MAIN_LOOP.load(Ordering::Relaxed)
}

/// Patch the one platform slot Tk asks for into the `TclPlatStubs` table.
///
/// # Safety
/// See `install`.
pub unsafe fn install_plat(t: &mut TclPlatStubs) -> usize {
    let i = TCL_PLAT_NAMES
        .iter()
        .position(|n| *n == "tcl_MacOSXNotifierAddRunLoopMode")
        .expect("no tcl_MacOSXNotifierAddRunLoopMode in TclPlatStubs");
    t.slots[i] =
        std::mem::transmute::<*const (), RawStub>(macosx_notifier_add_run_loop_mode as *const ());
    i
}
