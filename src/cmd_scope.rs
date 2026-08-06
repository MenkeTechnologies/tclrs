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
//!    [`BodySlots`] for the exact walk, and why the range rather than the entry
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
//! locals are slots. The table is compile-time metadata — [`SlotNames`], keyed
//! by chunk identity exactly as `crate::runtime`'s tolerant-read set is — so a
//! chunk that never says `upvar` carries it, never reads it, and runs the same
//! ops it ran before.
//!
//! ## What a link is at run time
//!
//! `upvar #0 other local` where both names and the level are literal is still
//! bound *while the script is read*: `Scope::aliases` records it and
//! [`Compiler::var_place`] answers with the global's own place, so the body pays
//! nothing. That covers the whole of what this module used to allow.
//!
//! Everything else — a computed level, a computed name, an array element as the
//! target — becomes a **link**: the local gets a frame slot, the slot holds a
//! [`Link`] descriptor rather than a value, and [`Compiler::var_place`] answers
//! [`Place::Link`] so that every read, write, `unset`, `incr`, `lappend` and
//! `info exists` follows the descriptor. A [`Link`] names one of three homes:
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
//! are read back into the frame's slots afterwards. What that cannot do is
//! *create* a variable in the target frame — a name with no slot in the callee's
//! table has no home to be written back to, because no op in the already-built
//! body could address it — so a name the script assigns that the target
//! procedure never mentions is reported rather than silently dropped.

use std::collections::HashMap;
use std::sync::Mutex;

use fusevm::{Op, Value, VM};

use crate::compiler::{ext, CompileError, Compiler, Place, Scope};
use crate::parser::Word;
use crate::procs::{parse_signature, Signature};
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
    fn encode(&self) -> Value {
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
        Value::Array(parts)
    }

    fn decode(value: &Value) -> Option<Link> {
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
    pub(crate) fn publish_slot_names(&mut self, scope: &Scope, entry: usize, end: usize) {
        let mut names: Vec<(String, u16)> = scope
            .locals
            .iter()
            .map(|(name, slot)| (name.clone(), *slot))
            .collect();
        names.sort_by_key(|(_, slot)| *slot);
        self.slot_names.push(BodySlots { entry, end, names });
    }

    /// `uplevel ?level? arg ?arg ...?`.
    ///
    /// Everything is decided when the command runs — which level the first word
    /// names, whether that level is the global one, and what script the
    /// remaining words concatenate to — because all three are properties of the
    /// call stack and of values the script computed.
    pub(crate) fn cmd_uplevel(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if args.is_empty() {
            return self.error("wrong # args: should be \"uplevel ?level? command ?arg ...?\"");
        }
        let count = u8::try_from(args.len())
            .map_err(|_| self.err("too many arguments for \"uplevel\"".to_string()))?;
        for arg in args {
            self.word(arg)?;
        }
        self.emit(Op::Extended(ext::UPLEVEL, count), 1 - args.len() as i32);
        Ok(())
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

    /// `apply lambda ?arg ...?`.
    ///
    /// A lambda whose text is written out is compiled exactly as a procedure
    /// body is: its own frame, its own slots, entered by `Op::Call`. So an
    /// applied lambda costs a call and nothing else — its locals are frame slots
    /// like any procedure's, and a loop inside one is as JIT-eligible as a loop
    /// inside a `proc`.
    ///
    /// A lambda that is a *value* is refused. Its body would have to be compiled
    /// when the command runs, as a chunk of its own, and a chunk of its own
    /// reaches only globals — so its parameters could not be its own. That is
    /// the same wall `eval` inside a procedure hits, and it is refused for the
    /// same reason rather than run against the wrong variables.
    pub(crate) fn cmd_apply(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some((lambda_w, actuals)) = args.split_first() else {
            return self.error("wrong # args: should be \"apply lambdaExpr ?arg ...?\"");
        };
        let Some(text) = lambda_w.as_literal() else {
            return self.error(
                "\"apply\" of a computed lambda is not supported: the body would be compiled as \
                 a chunk of its own, which cannot reach frame slots",
            );
        };
        let text = text.to_string();
        // A lambda that is not a two- or three-element list is reported by
        // tclsh when `apply` runs, not while the script is read, so the refusal
        // is marked deferrable and becomes a raise standing where the command's
        // code would have stood.
        let parts = crate::list::split(&text).map_err(|_| {
            self.deferrable_err(format!("can't interpret \"{text}\" as a lambda expression"))
        })?;
        let (spec, body) = match parts.as_slice() {
            [spec, body] => (spec, body),
            // The third element names the namespace the body runs in. `::` is
            // the only namespace here, so it is accepted and any other is not.
            [spec, body, ns] if ns == "::" || ns.is_empty() => (spec, body),
            [_, _, ns] => {
                return self.error(format!(
                    "\"apply\" into the namespace \"{ns}\" is not supported: this frontend has \
                     only the global namespace"
                ))
            }
            _ => {
                return Err(self
                    .deferrable_err(format!("can't interpret \"{text}\" as a lambda expression")))
            }
        };
        let name = format!("\u{0}apply\u{0}{}", self.b.current_pos());
        let sig = match parse_signature(&name, spec) {
            Ok(sig) => sig,
            Err(msg) => return self.error(msg),
        };
        self.emit_lambda(&name, &sig, body)?;
        self.procs.insert(name.clone(), sig);
        // The call site adapts to the signature exactly as a `proc` call does:
        // defaults are pushed here and surplus actuals collected into `args`.
        self.call_proc(&name, actuals)
    }

    /// Emit a lambda body as a sub of the enclosing chunk, behind a jump that
    /// steps over it — the shape `crate::procs::cmd_proc` gives a procedure, so
    /// an applied lambda is entered by the same `Op::Call` a procedure is.
    fn emit_lambda(&mut self, name: &str, sig: &Signature, body: &str) -> Result<(), CompileError> {
        let slots = u8::try_from(sig.params.len())
            .map_err(|_| self.err("a lambda with more than 255 formal parameters".to_string()))?
            as usize;
        let script = crate::parser::parse(body).map_err(|e| self.deferrable_err(e.msg))?;

        let skip = self.emit(Op::Jump(usize::MAX), 0);
        let entry = self.b.current_pos();

        let outer_depth = std::mem::replace(&mut self.depth, slots);
        let outer_loops = std::mem::take(&mut self.loops);
        let outer_catch = std::mem::replace(&mut self.catch_depth, 0);
        let outer_scope = self.scope.replace(lambda_scope(sig));
        let outer_top = std::mem::replace(&mut self.top_level, false);
        let outer_static = std::mem::replace(&mut self.static_ctx, false);

        for slot in (0..slots).rev() {
            self.emit(Op::SetSlot(slot as u16), -1);
        }
        let compiled = self.script_value(&script);
        self.emit(Op::ReturnValue, -1);

        self.depth = outer_depth;
        self.loops = outer_loops;
        self.catch_depth = outer_catch;
        let body_scope = std::mem::replace(&mut self.scope, outer_scope);
        self.top_level = outer_top;
        self.static_ctx = outer_static;
        compiled?;

        let after = self.b.current_pos();
        self.b.patch_jump(skip, after);
        if let Some(scope) = body_scope {
            self.publish_slot_names(&scope, entry, after);
        }
        let name_idx = self.b.add_name(name);
        self.b.add_sub_entry(name_idx, entry);
        Ok(())
    }

    /// Bind `local` to the global `other` for the rest of the body — or, outside
    /// a procedure, for the rest of the script.
    ///
    /// Two maps for the two scopes, because the *local* name means different
    /// things in them: inside a body it is a name that would otherwise have been
    /// a frame slot, and outside one it is a global that is now another spelling
    /// of a second global. Both are followed by [`Compiler::var_place`].
    fn bind_alias(&mut self, local: &str, other: &str) -> Result<(), CompileError> {
        let Some(scope) = self.scope.as_mut() else {
            let local = crate::cmd_namespace::global_key(self, local);
            let other = crate::cmd_namespace::global_key(self, other);
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

/// The slot scope a lambda body starts with: one slot per formal parameter, in
/// declaration order, matching the prologue emitted above.
fn lambda_scope(sig: &Signature) -> Scope {
    let mut scope = Scope::default();
    for (i, p) in sig.params.iter().enumerate() {
        scope.locals.insert(p.name.clone(), i as u16);
    }
    scope.next_slot = sig.params.len() as u16;
    scope
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

    let current = crate::cmd_info::current_level(vm);
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
    Ok(Link { home, elem })
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
    if let Some(idx) = vm.chunk.names.iter().position(|n| *n == key) {
        return u16::try_from(idx).map_err(|_| {
            TclError::plain(format!("\"upvar\" cannot reach the variable \"{name}\""))
        });
    }
    crate::runtime::intern_overflow(interp, vm, &key)
        .map_err(|msg| TclError::plain(format!("\"upvar\" cannot reach \"{name}\": {msg}")))
}

/// The frame slot that holds the local `name` of the procedure activation at
/// absolute level `target`.
fn frame_home(vm: &VM, target: i64, name: &str) -> Result<Home, TclError> {
    let frame =
        usize::try_from(target).map_err(|_| TclError::plain(format!("bad level \"{target}\"")))?;
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
        Some(slot) => Ok(Home::Slot {
            frame: u16::try_from(frame).map_err(|_| TclError::plain("call stack too deep"))?,
            slot,
        }),
        // A name with no slot in the callee's table has no home: no op in the
        // already-built body could address it, so writing one would be writing
        // into a variable that procedure can never read. tclsh creates it; here
        // that is reported rather than silently lost.
        None => Err(TclError::plain(format!(
            "\"upvar\" to the variable \"{name}\" at level {target} is not supported: the \
             procedure running there never names it, so it has no frame slot"
        ))),
    }
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

/// The cell the link points at, growing the storage to reach it. `None` when the
/// target is an element of something that is not an array.
pub(crate) fn write_link<'v>(vm: &'v mut VM, link: &Link) -> Option<&'v mut Value> {
    let base = match link.home {
        Home::Global(_) => crate::runtime::var_cell(vm, home_place(link))?,
        Home::Slot { frame, slot } => {
            let f = vm.frames.get_mut(usize::from(frame))?;
            let slot = usize::from(slot);
            if slot >= f.slots.len() {
                f.slots.resize(slot + 1, Value::Undef);
            }
            &mut f.slots[slot]
        }
    };
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

/// `uplevel` ([`ext::UPLEVEL`]): `[arg …]` with the count in the inline operand.
pub(crate) fn uplevel_op(interp: &Shared, vm: &mut VM, argc: u8) -> Result<(), TclError> {
    let mut args: Vec<String> = (0..argc).map(|_| to_tcl_string(&vm.pop())).collect();
    args.reverse();

    // Level 0 is the script's own and each procedure activation is one more;
    // `crate::cmd_info::current_level` is the same count `info level` answers
    // with, so `uplevel` and `info level` cannot disagree about where the code
    // calling them is.
    let current = crate::cmd_info::current_level(vm);
    // A level word is only a level when a script follows it: `uplevel {set x 1}`
    // is a script, and so is `uplevel 1` on its own — which is why tclsh reports
    // the argument count for the latter rather than a missing script.
    let has_script = args.len() > 1;
    let explicit = args
        .first()
        .filter(|_| has_script)
        .and_then(|w| absolute_level(w, current));
    let (target, script_from) = match explicit {
        Some(n) => (n, 1),
        None => (current - 1, 0),
    };
    if script_from >= args.len() {
        return Err(TclError::plain(
            "wrong # args: should be \"uplevel ?level? command ?arg ...?\"",
        ));
    }
    if target < 0 || target > current {
        // The level tclsh quotes is the word the script wrote, or `1` when the
        // level was left out and defaulted.
        let named = if script_from == 1 {
            args[0].clone()
        } else {
            "1".to_string()
        };
        return Err(TclError::plain(format!("bad level \"{named}\"")));
    }

    let rest = &args[script_from..];
    let src = match rest {
        [one] => one.clone(),
        many => crate::cmd_list::concat(many),
    };
    if target == 0 {
        crate::runtime::flush_globals(vm, interp);
        let result = crate::runtime::run_source(interp, &src);
        crate::runtime::reseed_globals(vm, interp);
        vm.push(result?);
        return Ok(());
    }
    let value = in_frame_context(interp, vm, target as usize, &src)?;
    vm.push(value);
    Ok(())
}

/// Run `src` against the variables of the procedure activation at `frame`.
///
/// The frame's named locals are projected into the interpreter's variables, the
/// script runs as a chunk of its own — where a bare name is a global, which is
/// what makes the projection *be* the frame's scope — and the values are read
/// back into the frame's slots afterwards. Whatever the interpreter held under
/// one of those names is put back, so a genuine global shadowed by a local of
/// the same name survives the call.
fn in_frame_context(
    interp: &Shared,
    vm: &mut VM,
    frame: usize,
    src: &str,
) -> Result<Value, TclError> {
    let Some(body) = frame_slots(vm, frame) else {
        return Err(TclError::plain(format!(
            "\"uplevel\" to level {frame} is not supported here: no slot names were recorded for \
             the procedure running at that level"
        )));
    };
    crate::runtime::flush_globals(vm, interp);

    // Everything the projection displaces, so it can be put back.
    let mut shadowed: Vec<(String, Option<Value>)> = Vec::with_capacity(body.names.len());
    {
        let mut state = interp.lock().expect("interpreter lock");
        for (name, slot) in &body.names {
            let held = state.globals.get(name).cloned();
            shadowed.push((name.clone(), held));
            let local = vm
                .frames
                .get(frame)
                .and_then(|f| f.slots.get(usize::from(*slot)))
                .cloned()
                .unwrap_or(Value::Undef);
            match local {
                Value::Undef => {
                    state.globals.remove(name);
                }
                value => {
                    state.globals.insert(name.clone(), value);
                }
            }
        }
    }

    let before: std::collections::HashSet<String> =
        interp.lock().expect("interpreter lock").global_names();
    let result = crate::runtime::run_source(interp, src);

    // Read the frame's locals back, then undo the projection. Both happen
    // however the script returned: what it assigned before failing is assigned,
    // which is the rule `eval` already follows.
    let mut created = Vec::new();
    {
        let mut state = interp.lock().expect("interpreter lock");
        // A name the script assigned that the target procedure has no slot for.
        // The projection is the frame's scope, so a bare name the script sets
        // *is* a variable of that frame — and there is nowhere in the frame to
        // put it, because no op in the already-built body could address one. It
        // is reported rather than left behind as a global, which is what it would
        // otherwise silently become.
        for name in state.global_names() {
            if !before.contains(&name) && !body.names.iter().any(|(n, _)| *n == name) {
                created.push(name);
            }
        }
        for name in &created {
            state.globals.remove(name);
        }
        for (name, slot) in &body.names {
            let value = state.globals.get(name).cloned().unwrap_or(Value::Undef);
            if let Some(f) = vm.frames.get_mut(frame) {
                let slot = usize::from(*slot);
                if slot >= f.slots.len() {
                    f.slots.resize(slot + 1, Value::Undef);
                }
                f.slots[slot] = value;
            }
        }
        for (name, held) in shadowed {
            match held {
                Some(value) => {
                    state.globals.insert(name, value);
                }
                None => {
                    state.globals.remove(&name);
                }
            }
        }
    }
    crate::runtime::reseed_globals(vm, interp);
    if let Some(name) = created.first() {
        return Err(TclError::plain(format!(
            "\"uplevel\" to level {frame} created the variable \"{name}\", which is not supported: \
             the procedure running there never names it, so it has no frame slot. A global the \
             script means to write is refused here too, because a chunk of its own cannot tell \
             \"set {name} 1\" from \"set ::{name} 1\""
        )));
    }
    result
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
        assert_eq!(Link::decode(&Value::Array(vec![Value::Int(0)])), None);
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

    #[test]
    fn a_lambda_scope_matches_the_prologue_it_is_emitted_with() {
        let sig = parse_signature("l", "a {b 2} args").expect("a valid signature");
        let scope = lambda_scope(&sig);
        assert_eq!(scope.locals.get("a"), Some(&0));
        assert_eq!(scope.locals.get("b"), Some(&1));
        assert_eq!(scope.locals.get("args"), Some(&2));
        assert_eq!(scope.next_slot, 3);
    }

    /// A parameter list the lambda shares with `proc`, so the two cannot drift.
    #[test]
    fn a_lambda_uses_the_procedure_signature_parser() {
        let sig: Signature = parse_signature("l", "x").expect("a valid signature");
        assert_eq!(sig.params.len(), 1);
        let first = &sig.params[0];
        assert_eq!(first.name, "x");
        assert_eq!(first.default, None);
    }
}
