//! `uplevel`, `upvar`, `variable` and `apply` — reaching another scope.
//!
//! # How a level-relative name reaches a frame slot, and what it cost
//!
//! A procedure's variables here are `fusevm` frame slots, and a slot is an
//! *index the compiler assigned*: `Op::GetSlot(3)` says nothing about which name
//! the script wrote, and `Frame { slots: Vec<Value> }` carries no name table.
//! Reaching level *N* by name therefore needs one of two things, and this module
//! used to have neither:
//!
//! 1. **A per-procedure slot-name table, emitted while the body is lowered and
//!    keyed so a live frame can find it.** A frame can be attributed to its
//!    procedure without any new machinery, because `Op::Call` records `return_ip`
//!    and the op before it is in the *caller's* body — so a table keyed by the
//!    op-index range each body occupies answers for every frame at once. See
//!    `BodySlots` for the exact walk, and why the range rather than the entry
//!    point is what it is keyed by.
//! 2. **A run-time variable table addressable by name at any level**, which is
//!    what the reference interpreter has. It costs procedure locals their slots
//!    — and with them the block and tracing JIT tiers, which read slots out of a
//!    flat `i64` buffer (`fusevm`'s `refresh_slot_buffers`) and cannot see a hash
//!    table.
//!
//! **The first is what is implemented**, because the second is not a trade this
//! frontend can make: every procedure in the tree would lose trace eligibility,
//! and `bench/counted_loop_proc.tcl` reaching native code is the whole reason
//! locals are slots. The table is compile-time metadata — `SlotNames`, keyed
//! by chunk identity exactly as `crate::runtime`'s tolerant-read set is — so a
//! chunk that never says `upvar` carries it, never reads it, and runs the same
//! ops it ran before.
//!
//! ## What a link is at run time
//!
//! `upvar #0 other local` where both names and the level are literal is still
//! bound *while the script is read*: `Scope::aliases` records it and
//! `Compiler::var_place` answers with the global's own place, so the body pays
//! nothing. That covers the whole of what this module used to allow.
//!
//! Everything else — a computed level, a computed name, an array element as the
//! target — becomes a **link**: the local gets a frame slot, the slot holds a
//! `Link` descriptor rather than a value, and `Compiler::var_place` answers
//! `Place::Link` so that every read, write, `unset`, `incr`, `lappend` and
//! `info exists` follows the descriptor. A `Link` names one of three homes:
//!
//! * a global at its index in the running chunk's projection;
//! * a global whose *name the chunk's table does not carry*, because the script
//!   computed it — interned into the projection past the end of the table by
//!   `crate::runtime::intern_overflow`, so it flushes back to the interpreter
//!   with every other global;
//! * a frame slot, by the frame's index in `vm.frames` and the slot in it, which
//!   is what the slot-name table above is for.
//!
//! Any of the three may carry an element key, because `upvar ::tk::FocusGrab($i)
//! data` — Tk's own idiom — links a local to one element of an array.
//!
//! Outside a procedure there is no frame slot for a descriptor and no need of
//! one: both names are then globals, and the link is an alias between two entries
//! of the interpreter's own variable table, which `seed` and `write_back` resolve
//! through (`crate::runtime::alias_global`). That is what `uplevel #0 [list upvar
//! #0 ::tk::Priv.$disp ::tk::Priv]` (`library/tk.tcl:257`) makes, and it has to
//! outlive the chunk that made it, which an alias on the interpreter does and a
//! descriptor in a frame would not.
//!
//! **The cost, stated plainly.** A linked name is not a slot read any more, so a
//! loop over one is not traceable. That is confined to the names an `upvar`
//! actually binds: the surrounding procedure's other locals are slots, its loops
//! are still traced, and `--tiers` on `bench/counted_loop_proc.tcl` reports the
//! same `traced=true` it did before.
//!
//! ## `uplevel` into a procedure activation
//!
//! The level is resolved when the command runs, against `vm.frames.len()` —
//! `crate::cmd_info::current_level` is the same count `info level` answers with,
//! so the two cannot disagree. A target that is the global level is served
//! exactly, as it always was.
//!
//! A target that is a *procedure activation* is served through the same
//! slot-name table: the frame's named locals are projected into the interpreter's
//! variables, the script runs against them as a chunk of its own, and the values
//! are read back into the frame's slots afterwards.
//!
//! ## A frame that grows a name
//!
//! The compiled body is not the whole of what a frame may hold. `eval {set qq
//! 9}` in a body that never writes `qq`, `uplevel 1 {set made 1}` into a caller
//! that never writes `made`, `upvar 1 fresh alias`, and a `dict with` key the
//! body never spells all name a variable of that activation which no op in the
//! already-built body can address. tclsh keeps such a name in the frame's own
//! variable table; here it gets a **run-time slot** past the last one the
//! compiler allocated — `runtime_slot_alloc` — so it is a local like any
//! other: a later script in the same frame reads what an earlier one wrote,
//! `info locals` lists it, and it dies with the frame rather than becoming a
//! global that outlives the call.
//!
//! It costs the activation that grew one its trace eligibility, because the
//! roster is a `Value::Array` in a slot and `slots_all_numeric` then fails —
//! the same cost a procedure-local array or an `upvar` link already carries,
//! and paid only from the moment the name is created.

use std::collections::HashMap;
use std::sync::Mutex;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler, Place, Scope};
use crate::parser::Word;
use crate::runtime::{to_tcl_string, Shared, TclError};

// ── the slot-name table ──────────────────────────────────────────────────

/// One procedure body's slots, by the name the script wrote them as, together
/// with the op-index range the body occupies.
///
/// The range is what attributes a *live frame* to a body. `Op::Call` records
/// `return_ip`, so the op before a frame's `return_ip` is in the body of the
/// procedure that *called* it — which makes frame *k*'s body the one containing
/// frame *k+1*'s `return_ip`, and the innermost frame's the one containing
/// `vm.ip`. A range rather than an entry point alone, because the script's own
/// top-level code sits after the bodies and must not be attributed to the last
/// one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodySlots {
    pub(crate) entry: usize,
    pub(crate) end: usize,
    /// Slot index → the name it was written as. Sparse only in the sense that a
    /// slot never named is absent.
    pub(crate) names: Vec<(String, u16)>,
}

/// Every procedure body of one chunk, innermost last is *not* guaranteed — the
/// lookup picks the narrowest range that contains the op index.
pub(crate) type SlotNames = Vec<BodySlots>;

/// The tables of every chunk that has been lowered, keyed by fusevm's identity
/// for the chunk — the same key `crate::runtime`'s tolerant-read set uses, and
/// for the same reason: an `eval`'s chunk starts its op indices at zero again.
static SLOT_NAMES: Mutex<Option<HashMap<u64, SlotNames>>> = Mutex::new(None);

/// Record which name each of `chunk`'s frame slots was written as.
pub(crate) fn note_slot_names(chunk: &fusevm::Chunk, bodies: &SlotNames) {
    if bodies.is_empty() {
        return;
    }
    let id = crate::runtime::chunk_identity(chunk);
    let mut guard = SLOT_NAMES.lock().expect("slot names lock");
    guard
        .get_or_insert_with(HashMap::new)
        .insert(id, bodies.clone());
}

/// The body containing `ip` in the chunk `id`: the narrowest recorded range that
/// contains it, so a lambda inside a procedure wins over the procedure.
fn body_at(id: u64, ip: usize) -> Option<BodySlots> {
    let guard = SLOT_NAMES.lock().expect("slot names lock");
    guard
        .as_ref()?
        .get(&id)?
        .iter()
        .filter(|b| (b.entry..b.end).contains(&ip))
        .min_by_key(|b| b.end - b.entry)
        .cloned()
}

/// The slot names of the frame at `frame` in `vm`, or `None` when nothing was
/// recorded for the body it belongs to.
///
/// Frame *k* runs the body containing frame *k+1*'s `return_ip`; the innermost
/// frame runs the body containing `vm.ip`. Frame 0 is the chunk's own top level
/// and has no procedure, which is what the `frame == 0` answer says.
pub(crate) fn frame_slots(vm: &VM, frame: usize) -> Option<BodySlots> {
    if frame == 0 || frame >= vm.frames.len() {
        return None;
    }
    let ip = match vm.frames.get(frame + 1) {
        Some(inner) => inner.return_ip.checked_sub(1)?,
        None => vm.ip.checked_sub(1)?,
    };
    body_at(crate::runtime::chunk_identity(&vm.chunk), ip)
}

// ── locals the body never mentioned ──────────────────────────────────────

/// The tag that marks the frame slot holding an activation's run-time local
/// *names*. It carries a NUL, which no list a script built can start with, so
/// the roster cannot be mistaken for a value some other mechanism stored — the
/// same rule [`LINK_TAG`] is written by.
const ROSTER_TAG: &str = "\u{0}roster";

/// The first frame slot past the ones the compiler allocated for the body
/// running in `frame` — where that activation's run-time locals begin.
///
/// `Chunk::sub_slot_names` has one entry per slot the body was lowered with
/// (`crate::procs::slot_names_of` sizes it by `Scope::next_slot`), so its length
/// *is* the count of slots any op in the body can address. Everything from there
/// on is unreachable to the compiled code and free for this table.
///
/// `None` for a frame that is not a procedure activation — the chunk's own top
/// level, a scope frame, one materialized after a JIT side exit. Those have no
/// locals in the Tcl sense, so there is nothing to grow.
fn roster_base(vm: &VM, frame: usize) -> Option<usize> {
    let up = vm.frames.len().checked_sub(frame + 1)?;
    vm.frames.get(frame)?.entry_ip?;
    Some(vm.slot_names_at(up).len())
}

/// The run-time locals of the activation in `frame`, in the order they were
/// created — which is the order `Tcl_GetVariableFullName` walks a frame's
/// `varTablePtr` overflow in, and therefore the order `info locals` reports
/// them after the body's own names.
pub(crate) fn runtime_names(vm: &VM, frame: usize) -> Vec<String> {
    let Some(base) = roster_base(vm, frame) else {
        return Vec::new();
    };
    match vm.frames[frame].slots.get(base) {
        Some(Value::Array(items)) => match items.split_first() {
            Some((tag, names)) if to_tcl_string(tag) == ROSTER_TAG => {
                names.iter().map(to_tcl_string).collect()
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The slot the run-time local `name` of the activation in `frame` occupies, or
/// `None` when that activation has never created it.
pub(crate) fn runtime_slot(vm: &VM, frame: usize, name: &str) -> Option<u16> {
    let base = roster_base(vm, frame)?;
    let at = runtime_names(vm, frame).iter().position(|n| n == name)?;
    u16::try_from(base + 1 + at).ok()
}

/// The same slot, creating it when the activation does not have the name yet.
///
/// A name created this way is a local of that activation for the rest of its
/// life: it is written back into the frame, a later script in the same frame
/// finds it, `info locals` lists it, and it dies with the frame — which is what
/// tclsh's own `varTablePtr` gives a name a compiled body never mentioned.
///
/// The roster and the values live in the frame's own `slots`, past the last one
/// the compiler allocated, so nothing has to know when the activation ends. The
/// cost is that the frame stops being all-numeric and so stops being traceable
/// — for the one activation that created a name this way, and only from the
/// moment it did.
pub(crate) fn runtime_slot_alloc(vm: &mut VM, frame: usize, name: &str) -> Option<u16> {
    if let Some(slot) = runtime_slot(vm, frame, name) {
        return Some(slot);
    }
    let base = roster_base(vm, frame)?;
    let mut names = runtime_names(vm, frame);
    names.push(name.to_string());
    let at = names.len() - 1;
    let slot = u16::try_from(base + 1 + at).ok()?;
    let mut roster = Vec::with_capacity(names.len() + 1);
    roster.push(Value::Str(std::sync::Arc::new(ROSTER_TAG.to_string())));
    for n in names {
        roster.push(Value::Str(std::sync::Arc::new(n)));
    }

    let slots = &mut vm.frames[frame].slots;
    if slots.len() <= usize::from(slot) {
        slots.resize(usize::from(slot) + 1, Value::Undef);
    }
    slots[base] = Value::array(roster);
    Some(slot)
}

/// `(name, value)` for every run-time local of the activation in `frame` that is
/// set right now. An `unset` leaves the name on the roster with `Value::Undef`
/// in its slot, which is how a body's own slots record the same thing.
pub(crate) fn runtime_locals(vm: &VM, frame: usize) -> Vec<(String, Value)> {
    runtime_names(vm, frame)
        .into_iter()
        .filter_map(|name| {
            let slot = runtime_slot(vm, frame, &name)?;
            match vm.frames[frame].slots.get(usize::from(slot)) {
                Some(v) if *v != Value::Undef => Some((name, v.clone())),
                _ => None,
            }
        })
        .collect()
}

// ── the link descriptor ──────────────────────────────────────────────────

/// Where an `upvar` link points, as the value stored in the local's frame slot.
///
/// A `Value::Array` rather than a bespoke `Value` variant, because fusevm's value
/// type belongs to fusevm: an `Array` is already a value a slot can hold, and it
/// is already non-numeric, which is what keeps the JIT from installing a trace
/// over a frame that holds one (`slots_all_numeric`, `fusevm`'s
/// `refresh_slot_buffers`) — the same protection a procedure-local array has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) home: Home,
    /// The element of the array at `home`, when the target was written `a(i)`.
    pub(crate) elem: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Home {
    /// A global, at its index in the running chunk's projection.
    Global(u16),
    /// A frame slot: the frame's index in `vm.frames`, and the slot in it.
    Slot { frame: u16, slot: u16 },
}

/// The tag that says a slot holds a link rather than a value. A string no script
/// can produce as the first element of a list it built, because it holds a NUL.
const LINK_TAG: &str = "\u{0}upvar";

impl Link {
    pub(crate) fn encode(&self) -> Value {
        let mut parts = vec![Value::Str(std::sync::Arc::new(LINK_TAG.to_string()))];
        match self.home {
            Home::Global(idx) => {
                parts.push(Value::Int(0));
                parts.push(Value::Int(i64::from(idx)));
            }
            Home::Slot { frame, slot } => {
                parts.push(Value::Int(1));
                parts.push(Value::Int(i64::from(frame)));
                parts.push(Value::Int(i64::from(slot)));
            }
        }
        if let Some(key) = &self.elem {
            parts.push(Value::Str(std::sync::Arc::new(key.clone())));
        }
        Value::array(parts)
    }

    pub(crate) fn decode(value: &Value) -> Option<Link> {
        let Value::Array(parts) = value else {
            return None;
        };
        if !matches!(parts.first(), Some(v) if to_tcl_string(v) == LINK_TAG) {
            return None;
        }
        let int = |i: usize| match parts.get(i) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        };
        let (home, next) = match int(1)? {
            0 => (Home::Global(int(2)? as u16), 3),
            1 => (
                Home::Slot {
                    frame: int(2)? as u16,
                    slot: int(3)? as u16,
                },
                4,
            ),
            _ => return None,
        };
        Some(Link {
            home,
            elem: parts.get(next).map(to_tcl_string),
        })
    }
}

/// The link the frame slot `slot` of the innermost frame holds, if it holds one.
pub(crate) fn link_at(vm: &VM, slot: u16) -> Option<Link> {
    let frame = vm.frames.last()?;
    Link::decode(frame.slots.get(usize::from(slot))?)
}

// ── compiling ────────────────────────────────────────────────────────────

impl Compiler {
    /// Publish the slot names of the body just lowered, so a live frame running
    /// it can be addressed by name. Called where the body's scope is discarded —
    /// by `crate::procs::cmd_proc` and by [`Compiler::emit_lambda`] below.
    ///
    /// A name `upvar` bound is published too, at the slot its [`Link`] descriptor
    /// lives in. It is a name of the frame — `upvar 1 $v y` makes `y` as much a
    /// local as `set y` does, and tclsh's `varTablePtr` holds both the same way —
    /// so a nested `upvar 1 y q`, a `dict with` key `y` and a computed `set $n`
    /// with `n` holding `y` must all find it. Each of those follows the
    /// descriptor rather than reading the slot, which is what [`link_of_home`]
    /// is for; publishing without following would hand them the descriptor
    /// itself.
    ///
    /// Not the same table `crate::procs::slot_names_of` builds: that one is the
    /// *projection* an `eval` inside the body runs against, and a slot holding a
    /// descriptor has no value to project.
    pub(crate) fn publish_slot_names(&mut self, scope: &Scope, entry: usize, end: usize) {
        let mut names: Vec<(String, u16)> = scope
            .locals
            .iter()
            .chain(scope.links.iter())
            .map(|(name, slot)| (name.clone(), *slot))
            .collect();
        names.sort_by_key(|(_, slot)| *slot);
        self.slot_names.push(BodySlots { entry, end, names });
    }

    /// `upvar ?level? otherVar localVar ?otherVar localVar ...?`.
    ///
    /// Whether the first word is a level is decided the way `Tcl_UpvarObjCmd`
    /// decides it (`generic/tclVar.c:5212-5227`): an *even* number of arguments
    /// means no level word and the default level 1, an odd number means the
    /// first word is the level. The count is a property of the text, so that
    /// much is settled here however the words are spelled.
    pub(crate) fn cmd_upvar(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.len() < 2 {
            return self.error(
                "wrong # args: should be \"upvar ?level? otherVar localVar ?otherVar localVar ...?\"",
            );
        }
        let has_level = !args.len().is_multiple_of(2);
        let (level, pairs) = if has_level {
            (Some(&args[0]), &args[1..])
        } else {
            (None, args)
        };
        if pairs.is_empty() {
            return self.error(
                "wrong # args: should be \"upvar ?level? otherVar localVar ?otherVar localVar ...?\"",
            );
        }

        // The one shape that needs no run-time indirection at all, and the one
        // the whole body used to be restricted to: an absolute level 0 with both
        // names written out. `local` becomes another spelling of the global
        // `other` for the rest of the body, through `Scope::aliases`.
        let literal_global = level
            .and_then(|w| w.as_literal())
            .is_some_and(|text| parse_level(text) == Some(Level::Absolute(0)));
        if literal_global {
            let literal_pairs: Option<Vec<(String, String)>> = pairs
                .chunks(2)
                .map(|p| {
                    let other = crate::assoc::target_of(&p[0])?;
                    let local = crate::assoc::target_of(&p[1])?;
                    match (other, local) {
                        (
                            crate::assoc::Target::Scalar(other),
                            crate::assoc::Target::Scalar(local),
                        ) => Some((other, local)),
                        _ => None,
                    }
                })
                .collect();
            if let Some(bound) = literal_pairs {
                for (other, local) in &bound {
                    self.bind_alias(local, other)?;
                }
                if self.scope.is_some() {
                    self.push_empty();
                    return Ok(());
                }
                // Outside a procedure the binding is between two *globals*, and a
                // global outlives the chunk that named it: `uplevel #0 [list
                // upvar #0 a b]` makes the pair in a chunk of its own, and every
                // later script has to see it. So the compile-time binding — which
                // is what keeps the two coherent inside *this* chunk — is paired
                // with a run-time registration on the interpreter, and the two
                // agree because they say the same thing.
                for (other, local) in &bound {
                    self.emit(Op::LoadInt(NO_SLOT), 1);
                    self.push_str(local);
                    self.push_str("#0");
                    self.push_str(other);
                    self.emit(Op::Extended(ext::UPVAR, 4), -3);
                    self.emit(Op::Pop, -1);
                }
                self.push_empty();
                return Ok(());
            }
        }

        // Otherwise the target is resolved when the command runs. The *local*
        // name still has to be known now — it is the name the rest of the body
        // spells, and the slot the descriptor lives in is chosen for it here.
        let mut locals = Vec::new();
        for pair in pairs.chunks(2) {
            let local = self.var_name_of(&pair[1])?;
            if local.contains("::") {
                // `ObjMakeUpvar` refuses a qualified local name
                // (`generic/tclVar.c:4544-4558`), because the link would outlive
                // the frame it points into.
                return self.error(format!(
                    "bad variable name \"{local}\": can't create namespace variable that refers \
                     to procedure variable"
                ));
            }
            // Outside a procedure there is no slot for a descriptor to live in,
            // and none is needed: both names are then globals, and the link is
            // an alias between two entries of the interpreter's own variable
            // table — see [`crate::runtime::alias_global`]. `NO_SLOT` is what
            // says so.
            let slot = match self.scope {
                Some(_) => i64::from(self.link_slot(&local)?),
                None => NO_SLOT,
            };
            locals.push((slot, local));
        }

        // `[slot, local]` per pair, then the level, then the `other` words. The
        // slots and names go first so the handler can pop the computed words off
        // the top of the stack.
        for (slot, local) in &locals {
            self.emit(Op::LoadInt(*slot), 1);
            self.push_str(local);
        }
        match level {
            Some(w) => self.word(w)?,
            // The absent level word is the default 1, and is pushed as the empty
            // string so the handler can tell "no level given" from a level the
            // script computed — which is the `hasLevel` flag `Tcl_UpvarObjCmd`
            // carries alongside the value.
            None => self.push_empty(),
        }
        for pair in pairs.chunks(2) {
            self.word(&pair[0])?;
        }
        let pushed = locals.len() * 3 + 1;
        let count = u8::try_from(pushed)
            .map_err(|_| self.err("too many arguments for \"upvar\"".to_string()))?;
        self.emit(Op::Extended(ext::UPVAR, count), 1 - pushed as i32);
        Ok(())
    }

    /// The slot a link descriptor for the local `name` lives in.
    ///
    /// A name that is already a local is refused, as `TclPtrObjMakeUpvarIdx`
    /// refuses it (`variable "x" already exists`); a name already linked is
    /// *rebound*, which is what `upvar` in a loop does.
    fn link_slot(&mut self, name: &str) -> Result<u16, CompileError> {
        let Some(scope) = self.scope.as_mut() else {
            return self.error("\"upvar\" outside a procedure has no local scope");
        };
        if let Some(slot) = scope.links.get(name) {
            return Ok(*slot);
        }
        if scope.locals.contains_key(name) || scope.aliases.contains_key(name) {
            return self.error(format!("variable \"{name}\" already exists"));
        }
        let slot = scope.next_slot;
        scope.next_slot += 1;
        scope.links.insert(name.to_string(), slot);
        Ok(slot)
    }

    // `variable` was lowered here too, for a frontend that had no namespaces
    // and could treat every namespace variable as a global. It now lives in
    // `crate::cmd_namespace`, which is the same command with the namespace case
    // filled in — the link a procedure body makes to `::foo::v`, the refusal of
    // a name carrying `::`, and a trailing name with no value. Keeping this
    // copy would have been two `cmd_variable` methods on one `impl`, and the
    // one the dispatcher reached would have depended on arm order.

    /// Bind `local` to the global `other` for the rest of the body — or, outside
    /// a procedure, for the rest of the script.
    ///
    /// Two maps for the two scopes, because the *local* name means different
    /// things in them: inside a body it is a name that would otherwise have been
    /// a frame slot, and outside one it is a global that is now another spelling
    /// of a second global. Both are followed by [`Compiler::var_place`].
    fn bind_alias(&mut self, local: &str, other: &str) -> Result<(), CompileError> {
        let Some(scope) = self.scope.as_mut() else {
            // Both sides are stored under the table key: the pair is between two
            // variables, and `::a` and `a` are one variable.
            let local = crate::cmd_namespace::global_key(self, local);
            let other = crate::cmd_namespace::global_key(self, other);
            let local = crate::cmd_namespace::store_key(&local).to_string();
            let other = crate::cmd_namespace::store_key(&other).to_string();
            if local == other {
                return self.error("can't upvar from variable to itself");
            }
            self.top_aliases.insert(local, other);
            return Ok(());
        };
        if scope.locals.contains_key(local) || scope.links.contains_key(local) {
            return Err(CompileError {
                msg: format!("variable \"{local}\" already exists"),
                line: self.command_line,
            });
        }
        scope.aliases.insert(local.to_string(), other.to_string());
        Ok(())
    }
}

/// What a level word names, if it names one at all.
///
/// `TclObjGetFrame` (`generic/tclProc.c:772-862`): `#N` is an absolute level and
/// a bare integer is a number of levels *up* from the current one. Anything else
/// is not a level, which is what makes `uplevel {set x 1}` a script and not a
/// level followed by nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Absolute(i64),
    Up(i64),
}

fn parse_level(text: &str) -> Option<Level> {
    if let Some(digits) = text.strip_prefix('#') {
        return digits.parse::<i64>().ok().map(Level::Absolute);
    }
    text.parse::<i64>().ok().map(Level::Up)
}

/// The absolute level a level word names, given the level the command is running
/// at. `None` when the word is not a level at all, which is the `-1` result
/// `TclObjGetFrame` returns.
fn absolute_level(text: &str, current: i64) -> Option<i64> {
    match parse_level(text)? {
        Level::Absolute(n) if n < 0 => None,
        Level::Absolute(n) => Some(n),
        Level::Up(n) => Some(current - n),
    }
}

// ── running ──────────────────────────────────────────────────────────────

/// `upvar` ([`ext::UPVAR`]) with a target the script computed: `[slot …,
/// level, other …]`, the inline operand counting the stack values.
///
/// The level is resolved against the call stack exactly as `uplevel`'s is, and
/// then each `other` name is resolved *in that level* — a `::`-qualified name in
/// the interpreter's variables whatever the level, a bare name in the frame's
/// slots when the level is a procedure activation and in the globals when it is
/// not, and an `a(i)` spelling as one element of an array. The result is a
/// [`Link`] stored in the local's slot.
pub(crate) fn upvar_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let pairs = (usize::from(argc) - 1) / 3;
    let mut others: Vec<String> = (0..pairs).map(|_| to_tcl_string(&vm.pop())).collect();
    others.reverse();
    let level_word = to_tcl_string(&vm.pop());
    let mut locals: Vec<(i64, String)> = (0..pairs)
        .map(|_| {
            let local = to_tcl_string(&vm.pop());
            (pop_int(vm), local)
        })
        .collect();
    locals.reverse();

    let current = crate::runtime::current_level(vm);
    // An absent level word is the default 1 and cannot be a bad level; a word
    // the script wrote and that names no level is `bad level "…"`, which is the
    // message `Tcl_UpvarObjCmd` synthesises for `result == 0 && hasLevel`.
    let target = if level_word.is_empty() {
        current - 1
    } else {
        match absolute_level(&level_word, current) {
            Some(n) => n,
            None => return Err(TclError::plain(format!("bad level \"{level_word}\""))),
        }
    };
    if target < 0 || target > current {
        let named = if level_word.is_empty() {
            "1".to_string()
        } else {
            level_word
        };
        return Err(TclError::plain(format!("bad level \"{named}\"")));
    }

    for ((slot, local), other) in locals.iter().zip(others.iter()) {
        if *slot == NO_SLOT {
            // Outside a procedure. `upvar #0 other local` there makes two
            // entries of the interpreter's variable table one variable, which is
            // how `tk.tcl` binds `::tk::Priv` to the per-display array it keeps
            // the real values in (`uplevel #0 [list upvar #0 ::tk::Priv.$disp
            // ::tk::Priv]`, `library/tk.tcl:257`).
            let (base, elem) = split_element(other);
            if elem.is_some() {
                return Err(TclError::plain(format!(
                    "\"upvar\" outside a procedure to the array element \"{other}\" is not \
                     supported: an alias between two entries of the interpreter's variable table \
                     cannot name one element of one of them"
                )));
            }
            crate::runtime::alias_global(interp, local, &base).map_err(TclError::plain)?;
            continue;
        }
        let link = resolve_target(interp, vm, target, other)?;
        let cell = crate::runtime::var_cell(vm, Place::Slot(*slot as u16))
            .ok_or_else(|| TclError::plain("\"upvar\" outside a procedure"))?;
        *cell = link.encode();
    }
    // Whatever the alias displaced, the running chunk's projection was taken
    // before it existed, so it is taken again — the same exchange `eval` makes
    // around a nested script.
    if locals.iter().any(|(slot, _)| *slot == NO_SLOT) {
        crate::runtime::flush_globals(vm, interp);
        crate::runtime::reseed_globals(vm, interp);
    }
    vm.push(Value::Str(std::sync::Arc::new(String::new())));
    Ok(())
}

/// The slot operand that means "there is no frame slot": an `upvar` outside a
/// procedure, whose link is an alias in the interpreter's variable table
/// instead. Not a `u16` value, so it cannot collide with a real slot.
const NO_SLOT: i64 = -1;

fn pop_int(vm: &mut VM) -> i64 {
    match vm.pop() {
        Value::Int(n) => n,
        other => to_tcl_string(&other).parse().unwrap_or(0),
    }
}

/// Where the name `other`, read at level `target`, lives.
fn resolve_target(
    interp: &Shared,
    vm: &mut VM,
    target: i64,
    other: &str,
) -> Result<Link, TclError> {
    // `a(i)` is an array element wherever the name came from: `TclObjLookupVar`
    // splits on the first `(` with the name ending at the final `)`, which is the
    // same split `crate::assoc::target_of` makes for a literal.
    let (base, elem) = split_element(other);
    let qualified = base.contains("::");
    let home = if target == 0 || qualified {
        Home::Global(global_home(interp, vm, &base)?)
    } else {
        frame_home(vm, target, &base)?
    };
    // The slot may hold a descriptor rather than a value, when the target is
    // itself a name the *target's* frame bound with `upvar`. Linking to the
    // descriptor's slot would make the new link point at the descriptor; tclsh
    // links to what the chain ends at, so `upvar 1 y q` in a body whose caller
    // did `upvar 1 $v y` reaches the caller's caller's variable.
    let link = link_of_home(
        vm,
        &Link {
            home,
            elem: elem.clone(),
        },
    )
    .unwrap_or(Link { home, elem });
    if link.elem.is_some() {
        materialize_array(vm, &link, other)?;
    }
    Ok(link)
}

/// `a(i)` → `("a", Some("i"))`, and anything else → `(name, None)`.
fn split_element(name: &str) -> (String, Option<String>) {
    match name.find('(') {
        Some(open) if name.ends_with(')') => (
            name[..open].to_string(),
            Some(name[open + 1..name.len() - 1].to_string()),
        ),
        _ => (name.to_string(), None),
    }
}

/// The index in the running chunk's global projection that holds `name`,
/// interning it past the end of the chunk's own name table when the table does
/// not carry it — which is the whole point of a *computed* target name.
fn global_home(interp: &Shared, vm: &mut VM, name: &str) -> Result<u16, TclError> {
    let key = crate::cmd_namespace::store_key(name).to_string();
    // The chunk may hold the name under either spelling — `::x` when the code it
    // was compiled from wrote the prefix — and both are the one variable, so the
    // table is searched by the key rather than by the spelling. Missing this
    // would intern a *second* projection entry for a variable the chunk already
    // carries, and the two would disagree.
    if let Some(idx) = vm
        .chunk
        .names
        .iter()
        .position(|n| crate::cmd_namespace::store_key(n) == key)
    {
        return u16::try_from(idx).map_err(|_| {
            TclError::plain(format!("\"upvar\" cannot reach the variable \"{name}\""))
        });
    }
    crate::runtime::intern_overflow(interp, vm, &key)
        .map_err(|msg| TclError::plain(format!("\"upvar\" cannot reach \"{name}\": {msg}")))
}

/// The frame slot that holds the local `name` of the procedure activation at
/// absolute level `target`.
///
/// A Tcl level is not a frame index. fusevm pushes frames that are not
/// activations — the base frame, a scope frame, one materialized after a JIT side
/// exit — and `crate::runtime::current_level` counts only the ones that are, so
/// the level has to be turned back into a frame the same way. Treating them as
/// the same number was right only while every frame on the stack was a call.
fn frame_home(vm: &mut VM, target: i64, name: &str) -> Result<Home, TclError> {
    if let Some(home) = frame_home_opt(vm, target, name)? {
        return Ok(home);
    }
    // A name with no slot in the compiled body gets one at run time, which is
    // what tclsh's own frame does with a name its compiled body never mentioned.
    // No op in the already-built body can address it — but a nested script in
    // that frame can, and `info locals` there lists it, which is the whole of
    // what the reference implementation gives such a name.
    let Some(frame) = crate::runtime::frame_of_level(vm, target) else {
        return Err(TclError::plain(format!("bad level \"{target}\"")));
    };
    match runtime_slot_alloc(vm, frame, name) {
        Some(slot) => Ok(Home::Slot {
            frame: u16::try_from(frame).map_err(|_| TclError::plain("call stack too deep"))?,
            slot,
        }),
        None => Err(TclError::plain(format!(
            "\"upvar\" to the variable \"{name}\" at level {target} is not supported: there is \
             no procedure activation at that level to hold it"
        ))),
    }
}

/// [`frame_home`] with the missing-slot case handed back rather than raised.
///
/// Both callers answer the `None` the same way — by growing the name a slot,
/// [`runtime_slot_alloc`] — but they reach it differently, so the "not there"
/// answer stays separate from the "here it is" one: `upvar` is asked for a
/// single name and raises whatever went wrong, and `dict with` is handed every
/// key of a dictionary at once.
///
/// Everything that is not "the body never names it" — a level that is no frame,
/// a frame whose body recorded no slot names at all — stays an error here,
/// because neither caller can do anything with those.
fn frame_home_opt(vm: &VM, target: i64, name: &str) -> Result<Option<Home>, TclError> {
    let Some(frame) = crate::runtime::frame_of_level(vm, target) else {
        return Err(TclError::plain(format!("bad level \"{target}\"")));
    };
    let Some(body) = frame_slots(vm, frame) else {
        return Err(TclError::plain(format!(
            "\"upvar\" to level {target} is not supported here: no slot names were recorded for \
             the procedure running at that level"
        )));
    };
    let slot = body
        .names
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, slot)| *slot);
    match slot {
        Some(slot) => Ok(Some(Home::Slot {
            frame: u16::try_from(frame).map_err(|_| TclError::plain("call stack too deep"))?,
            slot,
        })),
        None => Ok(None),
    }
}

/// Where the variable a `dict with` key names lives, in the frame the command is
/// running in — or `None` when that frame is a procedure activation whose body
/// never names it.
///
/// The key is a *value*, so this is the same resolution [`resolve_target`] does
/// for a computed `upvar` target, at the current level: a `::`-qualified name is
/// the interpreter's, a bare name at a script's own top level is a global —
/// interned past the chunk's table when the table does not carry it — and a bare
/// name inside a procedure is a frame slot. An `a(i)` spelling is one element of
/// an array, which is what `Tcl_ObjSetVar2(keyPtr, NULL, …)` makes of a key
/// written that way (`generic/tclDictObj.c:3810`): tclsh 9.0.4 turns the key
/// `a(1)` into the local *array* `a` with element `1`, measured.
pub(crate) fn dict_with_home(
    interp: &Shared,
    vm: &mut VM,
    key: &str,
) -> Result<Option<Link>, TclError> {
    let (base, elem) = split_element(key);
    let level = crate::runtime::current_level(vm);
    if level == 0 || base.contains("::") {
        let home = Home::Global(global_home(interp, vm, &base)?);
        return Ok(Some(Link { home, elem }));
    }
    if let Some(home) = frame_home_opt(vm, level, &base)? {
        // A key naming a slot that holds an `upvar` descriptor binds what the
        // descriptor points at, for the reason [`resolve_target`] states.
        let link = Link { home, elem };
        return Ok(Some(link_of_home(vm, &link).unwrap_or(link)));
    }
    // A key the body never wrote as a variable still becomes one — `dict with`
    // assigns *every* key of the dictionary (`generic/tclDictObj.c:3808-3816`),
    // and tclsh puts a name the compiled body has no slot for in the frame's own
    // variable table. That is what a run-time slot is, and it is what makes two
    // `dict with` commands over the same unmentioned key in one frame bind the
    // *same* variable, as they do in tclsh: the second finds the roster entry
    // the first created.
    let Some(frame) = crate::runtime::frame_of_level(vm, level) else {
        return Ok(None);
    };
    Ok(runtime_slot_alloc(vm, frame, &base).map(|slot| Link {
        home: Home::Slot {
            frame: u16::try_from(frame).unwrap_or(u16::MAX),
            slot,
        },
        elem,
    }))
}

/// [`ext::LINK_GET`]: `[name, slot]` → what the link points at.
pub(crate) fn link_get(vm: &mut VM, tolerant: bool) -> Result<(), TclError> {
    let slot = pop_slot(vm);
    let name = to_tcl_string(&vm.pop());
    let Some(link) = link_at(vm, slot) else {
        return Err(TclError::plain(format!(
            "can't read \"{name}\": no such variable"
        )));
    };
    match read_link(vm, &link).cloned() {
        Some(v) if v != Value::Undef => vm.push(v),
        // `incr` on a variable that does not exist creates it at zero, and a
        // linked name is no different: the read the compiler marked tolerant
        // answers with the zero rather than refusing. See `Compiler::cmd_incr`.
        _ if tolerant => vm.push(Value::Int(0)),
        _ => {
            return Err(TclError::plain(format!(
                "can't read \"{name}\": no such variable"
            )))
        }
    }
    Ok(())
}

/// [`ext::LINK_SET`]: `[value, name, slot]` → nothing, having stored the value
/// where the link points. The assignment's *value* is the caller's business —
/// [`Compiler::emit_set_var`] is a store and leaves the stack as it found it.
pub(crate) fn link_set(vm: &mut VM) -> Result<(), TclError> {
    let slot = pop_slot(vm);
    let name = to_tcl_string(&vm.pop());
    let value = vm.pop();
    let Some(link) = link_at(vm, slot) else {
        return Err(TclError::plain(format!(
            "can't set \"{name}\": no such variable"
        )));
    };
    let Some(cell) = write_link(vm, &link) else {
        return Err(TclError::plain(format!(
            "can't set \"{name}\": variable isn't array"
        )));
    };
    *cell = value;
    Ok(())
}

fn pop_slot(vm: &mut VM) -> u16 {
    match vm.pop() {
        Value::Int(n) => n as u16,
        other => to_tcl_string(&other).parse().unwrap_or(0),
    }
}

// ── variables whose name the script computes ─────────────────────────────
//
// `set $n 1`, `incr $n`, `unset $n`, `info exists $n`. The name is a value, so
// none of the compile-time resolution [`Compiler::var_place`] does is available
// — which is what the compiler used to refuse with `variable name must be a
// literal in this phase`. The resolution happens here instead, and it is the
// same one a computed `upvar` target and a `dict with` key already get: a
// `::`-qualified name and a name at a script's own top level are the
// interpreter's, a bare name inside a procedure is that activation's, and an
// `a(i)` spelling is one element of an array.

/// Where the variable that `name` spells lives, resolved in the frame the
/// command is running in.
///
/// `declared` is the enclosing body's `global` and `variable` declarations as a
/// Tcl list, pushed by the compiler because only it knows them — `global g`
/// leaves no trace in the frame, and a bare `$g` after it reads the *global*
/// rather than a local. Without this, `global g; set $n 1` with `n` holding `g`
/// would create a procedure-local `g` and leave the global untouched.
///
/// An `upvar`'d name resolves to the slot holding its [`Link`] descriptor, so
/// the descriptor is followed here: after `upvar 1 $other y`, `set $n 5` with
/// `n` holding `y` must write the caller's variable, not overwrite the link.
pub(crate) fn dynamic_link(
    interp: &Shared,
    vm: &mut VM,
    name: &str,
    declared: &str,
) -> Result<Link, TclError> {
    let (base, elem) = split_element(name);
    let level = crate::runtime::current_level(vm);
    let is_global = level == 0
        || base.contains("::")
        || crate::list::split(declared).is_ok_and(|names| names.contains(&base));
    if is_global {
        let home = Home::Global(global_home(interp, vm, &base)?);
        return Ok(Link { home, elem });
    }
    let home = match frame_home_opt(vm, level, &base)? {
        Some(home) => home,
        // A name the compiled body never mentioned becomes a run-time local of
        // this activation, which is what tclsh's own frame does with one.
        None => {
            let Some(frame) = crate::runtime::frame_of_level(vm, level) else {
                return Err(TclError::plain(format!(
                    "can't resolve \"{name}\": no variable context here"
                )));
            };
            let slot = runtime_slot_alloc(vm, frame, &base).ok_or_else(|| {
                TclError::plain(format!(
                    "can't resolve \"{name}\": there is no procedure activation to hold it"
                ))
            })?;
            Home::Slot {
                frame: u16::try_from(frame).map_err(|_| TclError::plain("call stack too deep"))?,
                slot,
            }
        }
    };
    // The slot may itself hold an `upvar` descriptor, in which case the variable
    // is wherever that points.
    let link = Link { home, elem };
    Ok(link_of_home(vm, &link).unwrap_or(link))
}

/// [`ext::DYN_GET`]: `[declared, name]` → what that variable holds.
///
/// The three ways a read can fail are told apart the way `$a` and `$a(i)` tell
/// them apart, because they are the same three: the variable is unset, it is an
/// array and was read as a scalar, or it is a scalar and was read as an element.
pub(crate) fn dyn_get_op(interp: &Shared, vm: &mut VM, absent: u8) -> Result<(), TclError> {
    let name = to_tcl_string(&vm.pop());
    let declared = to_tcl_string(&vm.pop());
    let link = dynamic_link(interp, vm, &name, &declared)?;
    // What an unset variable answers is the reading command's business: `incr`
    // creates a counter at zero and `append`/`lappend` create the variable by
    // extending nothing. See `crate::compiler::Absent`.
    let missing = |absent: u8| match absent {
        1 => Ok(Value::Int(0)),
        2 => Ok(Value::Str(std::sync::Arc::new(String::new()))),
        _ => Err(TclError::plain(format!(
            "can't read \"{name}\": no such variable"
        ))),
    };
    // The base cell, read without the element applied — the three ways a read
    // can fail are told apart by what it holds, and `read_link` collapses all
    // three into `None`. The same three [`ext::ELEM_GET`] tells apart for a name
    // the script wrote out, in the same words, because they are the same
    // failures reached by a different route.
    let base = read_link(
        vm,
        &Link {
            home: link.home,
            elem: None,
        },
    )
    .cloned();
    let value = match (&link.elem, base) {
        // A scalar read of a variable that holds an array is its own refusal,
        // and the one a bare `read_link` cannot give: it would answer with the
        // array's internal shape.
        (None, Some(Value::Hash(_))) => {
            return Err(TclError::plain(format!(
                "can't read \"{name}\": variable is array"
            )))
        }
        (None, Some(v)) if v != Value::Undef => v,
        (None, _) => missing(absent)?,
        (Some(key), Some(Value::Hash(map))) => match map.get(key) {
            Some(v) if *v != Value::Undef => v.clone(),
            _ if absent != 0 => missing(absent)?,
            _ => {
                return Err(TclError::plain(format!(
                    "can't read \"{name}\": no such element in array"
                )))
            }
        },
        (Some(_), Some(Value::Undef) | None) => missing(absent)?,
        // The variable exists and is not an array, so the element names nothing
        // that could ever be there. Which command says so depends on how far it
        // gets, and tclsh is measurably not uniform about it: `incr b(1)` on a
        // scalar `b` answers `can't read`, because `TclIncrObjCmd` reads before
        // it writes, while `append b(1) x` and `lappend b(1) x` answer `can't
        // set` — their read is the fully tolerant one and the refusal comes from
        // the store. So the empty-answering read passes this through and lets
        // [`dyn_set_op`] give it its own words.
        (Some(_), Some(_)) if absent == 2 => Value::Str(std::sync::Arc::new(String::new())),
        (Some(_), Some(_)) => {
            return Err(TclError::plain(format!(
                "can't read \"{name}\": variable isn't array"
            )))
        }
    };
    vm.push(value);
    Ok(())
}

/// [`ext::DYN_SET`]: `[value, declared, name]` → nothing, having stored it.
///
/// The name on top rather than under the value, for the reason the op's own
/// documentation gives: it is what lets `append` and `incr` keep the name they
/// already evaluated on the stack across the read and the store.
pub(crate) fn dyn_set_op(interp: &Shared, vm: &mut VM) -> Result<(), TclError> {
    let name = to_tcl_string(&vm.pop());
    let declared = to_tcl_string(&vm.pop());
    let value = vm.pop();
    let link = dynamic_link(interp, vm, &name, &declared)?;
    if link.elem.is_none() {
        if let Some(Value::Hash(_)) = read_link(vm, &link) {
            return Err(TclError::plain(format!(
                "can't set \"{name}\": variable is array"
            )));
        }
    }
    let Some(cell) = write_link(vm, &link) else {
        return Err(TclError::plain(format!(
            "can't set \"{name}\": variable isn't array"
        )));
    };
    *cell = value;
    Ok(())
}

/// [`ext::DYN_UNSET`]: `[declared, name]` → nothing.
///
/// `complain` is `unset` without `-nocomplain`, whose refusal names the variable
/// exactly as the literal path's [`ext::UNSET_VAR`] does.
pub(crate) fn dyn_unset_op(interp: &Shared, vm: &mut VM, complain: bool) -> Result<(), TclError> {
    let name = to_tcl_string(&vm.pop());
    let declared = to_tcl_string(&vm.pop());
    let link = dynamic_link(interp, vm, &name, &declared)?;
    let existed = match &link.elem {
        None => match write_link(vm, &link) {
            Some(cell) if *cell != Value::Undef => {
                *cell = Value::Undef;
                true
            }
            _ => false,
        },
        // Removed rather than emptied, for the reason [`unset_link`] states: a
        // key left holding nothing would still be listed by `array names`.
        //
        // An element of a variable that is *not* an array is its own refusal,
        // in the words [`ext::UNSET_ELEM`] gives it — and `-nocomplain` silences
        // that one too, which is what `TclObjUnsetVar2` does with it.
        Some(key) => {
            let key = key.clone();
            match base_cell(vm, &link) {
                // A key the array does not hold is "no such element in array",
                // not "no such variable" — the array itself is right there.
                Some(Value::Hash(map)) => {
                    if map.remove(&key).is_some() {
                        true
                    } else {
                        return if complain {
                            Err(TclError::plain(format!(
                                "can't unset \"{name}\": no such element in array"
                            )))
                        } else {
                            Ok(())
                        };
                    }
                }
                Some(Value::Undef) | None => false,
                Some(_) => {
                    return if complain {
                        Err(TclError::plain(format!(
                            "can't unset \"{name}\": variable isn't array"
                        )))
                    } else {
                        Ok(())
                    }
                }
            }
        }
    };
    if complain && !existed {
        return Err(TclError::plain(format!(
            "can't unset \"{name}\": no such variable"
        )));
    }
    Ok(())
}

/// [`ext::DYN_EXISTS`]: `[declared, name]` → 1 or 0.
///
/// Nothing is created: resolving through [`dynamic_link`] would grow a run-time
/// slot for a name the body never mentioned, and `info exists $n` asking about a
/// variable must not be what brings it into being. So a name with no slot and no
/// global entry is simply absent, which is the answer either way.
pub(crate) fn dyn_exists_op(vm: &mut VM, interp: &Shared) -> Result<(), TclError> {
    let name = to_tcl_string(&vm.pop());
    let declared = to_tcl_string(&vm.pop());
    let (base, elem) = split_element(&name);
    let level = crate::runtime::current_level(vm);
    let is_global = level == 0
        || base.contains("::")
        || crate::list::split(&declared).is_ok_and(|names| names.contains(&base));

    let home = if is_global {
        let key = crate::cmd_namespace::store_key(&base).to_string();
        match vm
            .chunk
            .names
            .iter()
            .position(|n| crate::cmd_namespace::store_key(n) == key)
            .and_then(|idx| u16::try_from(idx).ok())
        {
            Some(idx) => Some(Home::Global(idx)),
            // Not in this chunk's projection: the interpreter may still carry
            // it, and asking there costs nothing and creates nothing.
            None => {
                let held = crate::runtime::global_value(interp, &key);
                let set = match (&held, &elem) {
                    (None, _) | (Some(Value::Undef), _) => false,
                    (Some(Value::Hash(map)), Some(key)) => {
                        matches!(map.get(key), Some(v) if *v != Value::Undef)
                    }
                    (Some(_), Some(_)) => false,
                    (Some(_), None) => true,
                };
                vm.push(Value::Int(i64::from(set)));
                return Ok(());
            }
        }
    } else {
        match frame_home_opt(vm, level, &base)? {
            Some(home) => Some(home),
            None => crate::runtime::frame_of_level(vm, level)
                .and_then(|frame| runtime_slot(vm, frame, &base).map(|slot| (frame, slot)))
                .and_then(|(frame, slot)| {
                    Some(Home::Slot {
                        frame: u16::try_from(frame).ok()?,
                        slot,
                    })
                }),
        }
    };
    let set = match home {
        None => false,
        Some(home) => {
            let link = Link { home, elem };
            // An `upvar`'d name is whatever it points at, and a link that was
            // never made is nothing at all.
            let link = match link_of_home(vm, &link) {
                Some(followed) => followed,
                None => link,
            };
            matches!(read_link(vm, &link), Some(v) if *v != Value::Undef)
        }
    };
    vm.push(Value::Int(i64::from(set)));
    Ok(())
}

/// The link a resolved slot itself holds, if it holds one, with this link's own
/// element applied to it.
///
/// Every path that resolves a *name* to a frame slot ends here, because a slot
/// may hold an `upvar` descriptor rather than a value and the variable is then
/// wherever that points. An element spelled on this name wins over the
/// descriptor's own key: `a(i)` where `a` was bound to an array names element
/// `i` of that array.
///
/// `None` when the slot holds an ordinary value, which is the common case and
/// leaves the caller's own link standing.
fn link_of_home(vm: &VM, link: &Link) -> Option<Link> {
    let Home::Slot { frame, slot } = link.home else {
        return None;
    };
    let inner = vm
        .frames
        .get(usize::from(frame))
        .and_then(|f| f.slots.get(usize::from(slot)))
        .and_then(Link::decode)?;
    Some(Link {
        home: inner.home,
        elem: link.elem.clone().or(inner.elem),
    })
}

/// The place a link's *home* is, for the ops that take a place operand. The
/// element key, if any, is applied by [`read_link`] / [`write_link`].
fn home_place(link: &Link) -> Place {
    match link.home {
        Home::Global(idx) => Place::Global(idx),
        // A frame other than the innermost is not a `Place`, which only ever
        // means "the current frame". Those are reached directly below.
        Home::Slot { slot, .. } => Place::Slot(slot),
    }
}

/// What the link points at, without creating anything.
///
/// A borrow rather than a clone: the storage belongs to the VM either way, so
/// reading `$data(k)` through a link costs exactly what reading it through the
/// variable's own name costs.
pub(crate) fn read_link<'v>(vm: &'v VM, link: &Link) -> Option<&'v Value> {
    let base = match link.home {
        Home::Global(idx) => vm.globals.get(usize::from(idx)),
        Home::Slot { frame, slot } => vm
            .frames
            .get(usize::from(frame))
            .and_then(|f| f.slots.get(usize::from(slot))),
    }?;
    match &link.elem {
        None => Some(base),
        Some(key) => match base {
            Value::Hash(map) => map.get(key),
            _ => None,
        },
    }
}

/// Unset what the link in `slot` points at, answering whether it was set.
///
/// An element is *removed* rather than emptied: a key left holding nothing would
/// still be listed by `array names`, and `info exists a(k)` would still be true —
/// which is the one thing `unset` is for.
pub(crate) fn unset_link(vm: &mut VM, slot: u16) -> bool {
    let Some(link) = link_at(vm, slot) else {
        return false;
    };
    let Some(key) = link.elem.clone() else {
        return match write_link(vm, &link) {
            Some(cell) if *cell != Value::Undef => {
                *cell = Value::Undef;
                true
            }
            _ => false,
        };
    };
    let base = match link.home {
        Home::Global(idx) => vm.globals.get_mut(usize::from(idx)),
        Home::Slot { frame, slot } => vm
            .frames
            .get_mut(usize::from(frame))
            .and_then(|f| f.slots.get_mut(usize::from(slot))),
    };
    match base {
        Some(Value::Hash(map)) => map.remove(&key).is_some(),
        _ => false,
    }
}

/// The cell holding the *variable* a link points into — the array itself for an
/// `a(i)` target, and the target itself otherwise — growing the frame's slot
/// vector to reach it.
///
/// Separate from [`write_link`] because the variable has to be reached at two
/// different moments: when something is written through the link, and when the
/// link is *made*. See [`materialize_array`] for why the second one exists.
fn base_cell<'v>(vm: &'v mut VM, link: &Link) -> Option<&'v mut Value> {
    match link.home {
        Home::Global(_) => crate::runtime::var_cell(vm, home_place(link)),
        Home::Slot { frame, slot } => {
            let f = vm.frames.get_mut(usize::from(frame))?;
            let slot = usize::from(slot);
            if slot >= f.slots.len() {
                f.slots.resize(slot + 1, Value::Undef);
            }
            Some(&mut f.slots[slot])
        }
    }
}

/// Make the variable an `a(i)` link points into an array, before anything is
/// written through the link.
///
/// `Tcl_UpvarObjCmd` reaches its target through `TclObjLookupVar` with
/// `createPart1` set (`generic/tclVar.c`), so naming an element *creates the
/// array*: after `upvar 1 arr(k) e` the caller has an `arr`, `info exists arr`
/// is 1 and `array exists arr` is 1, whether or not `e` is ever assigned. Only
/// the array is created — `createPart2` is 0, so the element itself stays
/// absent and `array names arr` is empty.
///
/// The same lookup is what refuses an element of a variable that is already a
/// scalar, in these words, rather than quietly making a link that could never be
/// written through.
fn materialize_array(vm: &mut VM, link: &Link, spelled: &str) -> Result<(), TclError> {
    let Some(base) = base_cell(vm, link) else {
        return Ok(());
    };
    match base {
        Value::Undef => {
            *base = Value::Hash(HashMap::new());
            Ok(())
        }
        Value::Hash(_) => Ok(()),
        _ => Err(TclError::plain(format!(
            "can't access \"{spelled}\": variable isn't array"
        ))),
    }
}

/// The cell the link points at, growing the storage to reach it. `None` when the
/// target is an element of something that is not an array.
pub(crate) fn write_link<'v>(vm: &'v mut VM, link: &Link) -> Option<&'v mut Value> {
    let base = base_cell(vm, link)?;
    let Some(key) = &link.elem else {
        return Some(base);
    };
    if *base == Value::Undef {
        *base = Value::Hash(HashMap::new());
    }
    match base {
        Value::Hash(map) => Some(map.entry(key.clone()).or_insert(Value::Undef)),
        _ => None,
    }
}

/// The aliases a scope carries, for [`Compiler::var_place`] to consult. A
/// separate type so the map's meaning — a *local* name bound to a *global* one —
/// is stated where it is declared rather than at every use.
pub(crate) type Aliases = HashMap<String, String>;

/// The links a scope carries: a local name, and the frame slot its [`Link`]
/// descriptor lives in.
pub(crate) type Links = HashMap<String, u16>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_word_is_told_apart_from_a_script() {
        assert_eq!(parse_level("#0"), Some(Level::Absolute(0)));
        assert_eq!(parse_level("#3"), Some(Level::Absolute(3)));
        assert_eq!(parse_level("1"), Some(Level::Up(1)));
        assert_eq!(parse_level("-1"), Some(Level::Up(-1)));
        // A script is not a level, however it is spelled.
        assert_eq!(parse_level("set x 1"), None);
        assert_eq!(parse_level("#"), None);
        assert_eq!(parse_level(""), None);
    }

    /// `TclObjGetFrame`'s two forms, resolved against the same current level.
    #[test]
    fn a_level_word_resolves_to_an_absolute_level() {
        assert_eq!(absolute_level("#0", 3), Some(0));
        assert_eq!(absolute_level("#2", 3), Some(2));
        assert_eq!(absolute_level("1", 3), Some(2));
        assert_eq!(absolute_level("0", 3), Some(3));
        assert_eq!(absolute_level("3", 3), Some(0));
        // `#-1` is refused by `TclObjGetFrame` before any frame is looked for.
        assert_eq!(absolute_level("#-1", 3), None);
        assert_eq!(absolute_level("nope", 3), None);
    }

    #[test]
    fn an_element_target_splits_on_the_first_paren() {
        assert_eq!(
            split_element("a(i)"),
            ("a".to_string(), Some("i".to_string()))
        );
        assert_eq!(
            split_element("p(a(b))"),
            ("p".to_string(), Some("a(b)".to_string()))
        );
        assert_eq!(split_element("q(x)y"), ("q(x)y".to_string(), None));
        assert_eq!(split_element("plain"), ("plain".to_string(), None));
    }

    /// Every link shape survives the round trip through the value a frame slot
    /// holds, and nothing a script could put in a slot decodes as one.
    #[test]
    fn a_link_round_trips_through_the_slot_it_lives_in() {
        for link in [
            Link {
                home: Home::Global(7),
                elem: None,
            },
            Link {
                home: Home::Global(7),
                elem: Some("k".to_string()),
            },
            Link {
                home: Home::Slot { frame: 2, slot: 5 },
                elem: None,
            },
            Link {
                home: Home::Slot { frame: 2, slot: 5 },
                elem: Some("i j".to_string()),
            },
        ] {
            assert_eq!(Link::decode(&link.encode()), Some(link));
        }
        assert_eq!(Link::decode(&Value::Int(3)), None);
        assert_eq!(Link::decode(&Value::array(vec![Value::Int(0)])), None);
    }

    #[test]
    fn a_place_round_trips_through_its_operand() {
        for place in [
            Place::Global(0),
            Place::Global(65535),
            Place::Slot(0),
            Place::Slot(65535),
            Place::Link(0),
            Place::Link(65535),
        ] {
            assert_eq!(Place::decode(place.encode()), place);
        }
    }

    /// A lambda gets one frame slot per formal, in declaration order.
    ///
    /// It used to assert that of `lambda_scope`, when `apply` lowered a
    /// written-out lambda to a sub of the enclosing chunk. `apply` runs a lambda
    /// as the procedure it is instead — `crate::runtime::apply_op` synthesises
    /// `proc` and calls it — because that also runs a lambda the script
    /// *computed*, which the lowering could not. So the assertion is made of
    /// `crate::procs::scope_for`, which is the scope a procedure body is lowered
    /// in and therefore the one a lambda body is now lowered in.
    #[test]
    fn a_lambda_scope_matches_the_prologue_it_is_emitted_with() {
        let sig = crate::procs::parse_signature("l", "a {b 2} args").expect("a valid signature");
        let scope = crate::procs::scope_for(&sig);
        assert_eq!(scope.locals.get("a"), Some(&0));
        assert_eq!(scope.locals.get("b"), Some(&1));
        assert_eq!(scope.locals.get("args"), Some(&2));
        assert_eq!(scope.next_slot, 3);
    }

    /// A parameter list the lambda shares with `proc`, so the two cannot drift.
    #[test]
    fn a_lambda_uses_the_procedure_signature_parser() {
        let sig: crate::procs::Signature =
            crate::procs::parse_signature("l", "x").expect("a valid signature");
        assert_eq!(sig.params.len(), 1);
        let first = &sig.params[0];
        assert_eq!(first.name, "x");
        assert_eq!(first.default, None);
    }
}
