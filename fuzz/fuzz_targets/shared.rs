//! What every fuzz target needs: the link stub the AOT path leaves behind, and
//! the input decoder the runtime targets use.
//!
//! Included with `#[path = "shared.rs"] mod shared;` rather than pulled from a
//! library, because the `#[no_mangle]` stub below has to be emitted into the
//! target's *own* object file to be seen by the linker.

// ── the AOT link stubs ───────────────────────────────────────────────────
//
// `fusevm_aot_run_embedded` is `#[no_mangle] pub extern "C"` in fusevm, so it
// is a linker-visible symbol in fusevm's archive, and it declares three symbols
// an AOT object supplies: the compiled driver `fusevm_aot_entry` and the
// embedded chunk's blob and length. A normal `cargo build` never pulls that
// archive member in — nothing references it — but cargo-fuzz builds with
// `-Ccodegen-units=1`, which puts all of fusevm in one member, so the member is
// pulled in wholesale and its undefined symbols become a link error:
//
//   "_fusevm_aot_entry", referenced from:
//       _fusevm_aot_run_embedded in libfusevm-….rlib[4](….cgu.0.rcgu.o)
//
// Defining them here is not a stand-in for the real ones. Reaching any of them
// means a fuzz target called `run_embedded` on a binary with no AOT object
// linked in, which has no sane answer — so the entry aborts rather than
// returning a lie, and the chunk is empty rather than plausible.

/// Never called; the signature matches fusevm's declaration so the symbol
/// resolves.
///
/// # Safety
///
/// Unreachable. A fuzz target has no AOT object linked in.
#[no_mangle]
pub unsafe extern "C" fn fusevm_aot_entry(_vm: *mut std::ffi::c_void) -> i64 {
    unreachable!("a fuzz target has no AOT object linked in");
}

/// The embedded chunk an AOT object would carry. There is none.
#[no_mangle]
#[used]
pub static fusevm_aot_chunk_blob: u8 = 0;

/// Its length: zero, so `run_embedded` would report a corrupt chunk rather than
/// read the byte above.
#[no_mangle]
#[used]
pub static fusevm_aot_chunk_len: u64 = 0;

/// The longest input a target will look at. Above this the time goes into
/// memcpy rather than into new coverage.
pub const MAX_INPUT: usize = 65_536;

/// The input as a Tcl script, or `None` when it is not one.
pub fn source(data: &[u8]) -> Option<&str> {
    let src = std::str::from_utf8(data).ok()?;
    (src.len() <= MAX_INPUT).then_some(src)
}

// ── the stack a host owes this library ───────────────────────────────────
//
// libfuzzer runs a target on the process's main thread, which is 8 MiB on
// macOS. Every depth limit in this crate is calibrated for
// `runtime::RECOMMENDED_STACK` instead — `parser::MAX_NESTING_DEPTH` and
// `expr::MAX_EXPR_DEPTH` both — and the `tclrs` binary spawns exactly that
// thread (`src/main.rs`), so a host that does not is running the library
// outside its documented contract.
//
// Measured, not assumed: with the target body on the main thread, the `expr`
// corpus' 8_000-parenthesis seed aborts with
// `AddressSanitizer: stack-overflow … in ExprParser::parse_binary`, while the
// same input under `tclrs` reports `too many nested subexpressions`. The
// fuzzer is a host; it owes the library the stack.

/// A closure to run on the worker.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The one worker thread, started on first use.
///
/// One for the process rather than one per execution: a spawn costs tens of
/// microseconds, which at ten thousand executions a second is most of the
/// budget. A rendezvous on a channel costs a fraction of that.
fn worker() -> &'static std::sync::mpsc::SyncSender<Job> {
    static WORKER: std::sync::OnceLock<std::sync::mpsc::SyncSender<Job>> =
        std::sync::OnceLock::new();
    WORKER.get_or_init(|| {
        let (jobs, queue) = std::sync::mpsc::sync_channel::<Job>(0);
        std::thread::Builder::new()
            .stack_size(tclrs::runtime::RECOMMENDED_STACK)
            .spawn(move || {
                for job in queue {
                    job();
                }
            })
            .expect("spawn the deep-stack worker");
        jobs
    })
}

/// Run `body` on a thread of [`tclrs::runtime::RECOMMENDED_STACK`] and wait for
/// it.
///
/// Waiting is what makes a crash attributable: libfuzzer reports the input it
/// last handed to the target, so the target must not return before the work on
/// that input is done. A panic in `body` drops the completion channel, and the
/// `expect` below turns that into a panic on this thread — which the
/// `fuzz_target!` macro's hook aborts on, as it would have anyway.
///
/// `body` must own its data (`'static`): the input is a borrow of libfuzzer's
/// buffer, so a target copies it before handing it over. That copy is at most
/// [`MAX_INPUT`] bytes and is not what the time goes into.
pub fn on_deep_stack(body: impl FnOnce() + Send + 'static) {
    let (done, wait) = std::sync::mpsc::sync_channel::<()>(0);
    worker()
        .send(Box::new(move || {
            body();
            let _ = done.send(());
        }))
        .expect("the deep-stack worker is gone");
    wait.recv()
        .expect("the deep-stack worker died on this input");
}

// ── the script generator ─────────────────────────────────────────────────
//
// A byte string is a good input for the parser and a weak one for the runtime:
// almost every mutation is a parse error, so the VM is never reached and the
// commands themselves are never run. The targets that execute build a script
// instead — fixed command skeletons, with the fuzzer's bytes as the *arguments*,
// which is where the crashes have been (a `format` field width, a `string
// repeat` count, a list index).
//
// Every skeleton is bounded by construction: loops count to a literal, no
// command reads the filesystem or the network (the compiler has no `exec`,
// `open`, `source` or `exit`), and the interpreter the targets build has a low
// recursion limit. So a generated script terminates, and a libfuzzer timeout is
// a real finding rather than a `while {1} {}` the mutator happened to write.

/// How many commands a generated script may hold. Past this the extra commands
/// buy repetition rather than coverage.
const MAX_FRAGMENTS: usize = 40;

/// What every generated script starts with, so the commands below have
/// something to work on: a scalar, a list, an array and a dict, plus a
/// procedure to call and a counter the bounded loops use.
const PRELUDE: &str = "set a 1\n\
     set b [list x y z]\n\
     set i 0\n\
     array set arr {p 1 q 2}\n\
     set d [dict create k v]\n\
     proc p1 {x} {return [string length $x]}\n";

/// The command skeletons, one per generated fragment.
///
/// `@` is a placeholder: each one is replaced by a braced word built from the
/// fuzzer's bytes. A skeleton with three of them splits its payload three ways.
///
/// Every skeleton here compiles for *any* payload, which is what makes the
/// `catch` around each fragment worth having: a command tclrs refuses at compile
/// time takes the whole script down with it, and the other thirty-nine fragments
/// never run. That is why the fuzzer-controlled expression goes through `eval`,
/// which compiles at run time, and why the commands this frontend has not
/// implemented yet (`string wordend`, `dict with`, `catch`'s options variable)
/// are not listed: they would spend most of the fuzzer's executions compiling
/// six lines and then failing.
const SKELETONS: &[&str] = &[
    // Scalars.
    "set a @",
    "append a @ @",
    "eval {incr a @}",
    "unset a",
    "set a [set b]",
    // Lists.
    "set b [list @ @ @]",
    "lindex $b @",
    "lindex $b @ @",
    "lrange $b @ @",
    "linsert $b @ @",
    "lreplace $b @ @ @",
    "lsearch -start @ $b @",
    "lsearch -glob $b @",
    "lsort -unique $b",
    "lsort -integer $b",
    "llength @",
    "lreverse @",
    "lappend b @",
    "concat $b @",
    "join $b @",
    "split @ @",
    // Strings — `format` first, since its width and precision come from the
    // script and both scale the result.
    "format @ @",
    "format @ @ @",
    "format @ $a $b",
    "string repeat @ @",
    "string index @ @",
    "string range @ @ @",
    "string map @ @",
    "string match @ @",
    "string first @ @ @",
    "string last @ @ @",
    "string compare -length @ @ @",
    "string is integer -strict @",
    "string trim @ @",
    "string trimleft @ @",
    "string totitle @ @ @",
    "string replace @ @ @ @",
    "string tolower @ @ @",
    // Expressions. The fuzzer-controlled one goes through `eval` so that an
    // expression it cannot parse is a runtime error the `catch` takes, rather
    // than a compile error that ends the script.
    "eval {expr @}",
    "eval {expr {@ + 1}}",
    "expr {[string length @] + $a}",
    "incr i [string length @]",
    // Arrays and dicts.
    "array set arr @",
    "array get arr @",
    "array names arr -glob @",
    "array unset arr @",
    "dict set d @ @",
    "dict get $d @",
    "dict merge $d @",
    "dict for {k v} $d {set a $k}",
    // Control flow, every loop bounded by a literal.
    "for {set i 0} {$i < 4} {incr i} {set a [string index @ $i]}",
    "foreach x $b {append a $x}",
    "foreach {x y} @ {set a $x}",
    "while {[incr i] < 4} {lappend b @}",
    "if {[string length @] > 2} {set a 1} else {set a 2}",
    "switch -glob -- @ @ {set a 1} default {set a 2}",
    "for {set i 0} {$i < 3} {incr i} {if {$i == 1} break}",
    "for {set i 0} {$i < 3} {incr i} {if {$i == 1} continue}",
    // Procedures, errors and the nested evaluator.
    "p1 @",
    "catch {error @} m",
    "catch {p1 @} m",
    "eval @",
    "eval {set a @}",
    "eval {p1 @}",
    "info coroutine",
];

/// The fuzzer's bytes as one Tcl word.
///
/// Braces are the only quoting that leaves the text alone, so the three
/// characters that would end the word early — `{`, `}` and a backslash — are
/// dropped and everything else is kept. `$`, `[`, `"` and `;` therefore reach
/// the command as literal data, which is the point: they are what a `format`
/// specifier or a `string map` pattern is made of.
fn word(payload: &[u8]) -> String {
    let mut out = String::with_capacity(payload.len() + 2);
    out.push('{');
    for c in String::from_utf8_lossy(payload).chars() {
        if !matches!(c, '{' | '}' | '\\') {
            out.push(c);
        }
    }
    out.push('}');
    out
}

/// Build a runnable Tcl script from the input.
///
/// The input is read as a sequence of fragments: one byte chooses a skeleton
/// from [`SKELETONS`], one gives the length of its payload, and the payload
/// follows. A truncated fragment ends the script, so the mutator can grow one
/// by appending.
pub fn script(data: &[u8]) -> String {
    let mut out = String::from(PRELUDE);
    let mut rest = data;
    for _ in 0..MAX_FRAGMENTS {
        let [choice, len, payload @ ..] = rest else {
            break;
        };
        let len = (*len as usize).min(payload.len());
        let (payload, tail) = payload.split_at(len);
        rest = tail;

        let skeleton = SKELETONS[*choice as usize % SKELETONS.len()];
        let slots = skeleton.matches('@').count();
        // Each fragment is wrapped, so one command's error does not end the
        // script and the rest of the fragments still run.
        out.push_str("catch {");
        let mut chunks = payload.chunks(payload.len().div_ceil(slots.max(1)).max(1));
        for (nth, piece) in skeleton.split('@').enumerate() {
            out.push_str(piece);
            // `split` yields one more piece than there are slots. Every slot
            // gets a word even when the payload ran out, so a short input still
            // produces a command with the right number of arguments.
            if nth < slots {
                out.push_str(&word(chunks.next().unwrap_or_default()));
            }
        }
        out.push_str("}\n");
    }
    out
}
