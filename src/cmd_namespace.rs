//! `namespace`, `variable` and `rename` — Tcl's second name space.
//!
//! # What a namespace is here
//!
//! Tcl's namespaces are two lookup tables per namespace — one for commands, one
//! for variables — arranged in a tree rooted at `::`, with a resolution rule
//! that walks from the current namespace outward (`generic/tclNamesp.c`,
//! `TclGetNamespaceForQualName`). Nothing about that rule is dynamic in the
//! cases a script actually writes: `namespace eval` names its namespace
//! literally, `proc` names its procedure literally, and `$v` names its variable
//! literally. So this frontend resolves namespaces where it resolves everything
//! else — while compiling — and the runtime carries only what a *query* needs.
//!
//! Concretely:
//!
//! * a **variable** `v` in namespace `::foo` is the entry `foo::v` of the
//!   interpreter's variable map. `::` is the empty prefix, so a global variable
//!   keeps the name it always had and every script that predates this module
//!   compiles to the same bytecode ([`store_key`]);
//! * a **procedure** `p` defined in `::foo` is registered under `foo::p`, and a
//!   call written `p` from inside `::foo` resolves to it before it resolves to
//!   a global `p` — the two-step rule of `TclGetNamespaceForQualName`, measured
//!   against tclsh 9.0.4 in `tests/namespace_differential.rs`;
//! * a **query** — `namespace exists`, `children`, `which`, `origin`, `parent`,
//!   `import`, `delete` — is answered from a [`Registry`] the interpreter holds,
//!   which the compiled code populates as it runs.
//!
//! # The name grammar
//!
//! [`qualifiers`] and [`tail`] are ports of `NamespaceQualifiersCmd` and
//! `NamespaceTailCmd` (`generic/tclNamesp.c`), backwards scans for the last
//! `::` that then step back over *every* adjacent colon. That is why
//! `namespace qualifiers ::foo::` is `::foo` and not `::foo::`, and why
//! `namespace tail ::` is empty rather than `:`. They are pure functions of the
//! text — no namespace has to exist for either to answer — which is why both
//! are constant-folded when their argument is a literal.
//!
//! # What is refused
//!
//! A namespace name or a body this compiler cannot read while compiling is
//! refused rather than approximated: `namespace eval $n {…}` names a namespace
//! that is not known until the command runs, and every resolution above depends
//! on knowing it. `namespace path` and `namespace unknown` change
//! resolution at run time and are refused for the same reason. See
//! `refuse_dynamic`. `namespace upvar` with its names written out is the
//! `upvar #0` to the fully qualified name, and is lowered as that.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fusevm::{Op, Value};

use crate::compiler::{CompileError, Compiler};
use crate::parser::{Part, Script, Word};
use crate::runtime::{to_tcl_string, TclError};

/// Extension opcode ids owned by this module, at the bottom of the block
/// `compiler::ext::NS_BASE` names. [`crate::cmd_source`] takes the rest of the
/// same block; the two are one feature.
pub mod ext {
    use crate::compiler::ext::NS_BASE;

    /// `[sub, current, arg …]` with the count in the inline operand — one of
    /// the runtime namespace operations, named by `sub`. One value out.
    pub const NS: u16 = NS_BASE;
    /// `[name]` → the same name, having refused it when the command it names
    /// has been renamed away. Emitted only at a call site whose name the same
    /// chunk also passes to `rename`; see `Compiler::ns_guard`.
    pub const RENAME_GUARD: u16 = NS_BASE + 1;
}

/// Whether `id` is one of this module's runtime ops.
pub fn is_op(id: u16) -> bool {
    id == ext::NS || id == ext::RENAME_GUARD
}

// ── the name grammar ─────────────────────────────────────────────────────

/// `namespace qualifiers name` — everything before the last `::`, with the
/// separator's colons removed.
///
/// Port of `NamespaceQualifiersCmd` (`generic/tclNamesp.c`): scan backwards for
/// a `::`, then step back over every further colon, and answer the prefix that
/// remains. An empty answer means the name had no qualifier at all.
pub fn qualifiers(name: &str) -> &str {
    let b = name.as_bytes();
    let mut p = b.len();
    // `--p` before the test, exactly as the C loop does, so the final character
    // is never itself the start of a separator.
    while p > 0 {
        p -= 1;
        if b[p] == b':' && p > 0 && b[p - 1] == b':' {
            // Step over the second colon, then over any run of further colons.
            if p < 2 {
                return "";
            }
            p -= 2;
            while b[p] == b':' {
                if p == 0 {
                    return "";
                }
                p -= 1;
            }
            return &name[..p + 1];
        }
    }
    ""
}

/// `namespace tail name` — everything after the last `::`.
///
/// Port of `NamespaceTailCmd` (`generic/tclNamesp.c`). The C loop's condition is
/// `--p > name`, not `>=`, so a leading `::` at position 0 is not a separator
/// and `namespace tail ::` is the empty string rather than `:`.
pub fn tail(name: &str) -> &str {
    let b = name.as_bytes();
    let mut p = b.len();
    while p > 1 {
        p -= 1;
        if b[p] == b':' && b[p - 1] == b':' {
            return &name[p + 1..];
        }
    }
    name
}

/// Split a name into its components, treating any run of two or more colons as
/// one separator, and say whether it was absolute.
fn components(name: &str) -> (bool, Vec<&str>) {
    let absolute = name.starts_with("::");
    let mut parts = Vec::new();
    let mut rest = name;
    while !rest.is_empty() {
        match rest.find("::") {
            Some(0) => {
                // Skip the whole run of colons.
                rest = rest.trim_start_matches(':');
            }
            Some(i) => {
                parts.push(&rest[..i]);
                rest = &rest[i..];
            }
            None => {
                parts.push(rest);
                break;
            }
        }
    }
    (absolute, parts)
}

/// The fully-qualified form of `name` as seen from `current`, which is itself
/// fully qualified and begins with `::`.
///
/// This is `TclGetNamespaceForQualName`'s first step: a name beginning with
/// `::` is absolute, anything else is relative to the current namespace.
pub fn resolve(current: &str, name: &str) -> String {
    let (absolute, parts) = components(name);
    let mut out = String::new();
    if !absolute {
        out.push_str(current.trim_end_matches(':'));
    }
    for part in parts {
        out.push_str("::");
        out.push_str(part);
    }
    if out.is_empty() {
        out.push_str("::");
    }
    out
}

/// The parent of a fully-qualified namespace: `::foo::bar` → `::foo`, `::foo` →
/// `::`, and `::` → the empty string, which is what tclsh answers for the root.
pub fn parent_of(fqn: &str) -> String {
    if fqn == "::" {
        return String::new();
    }
    let q = qualifiers(fqn);
    if q.is_empty() {
        "::".to_string()
    } else {
        q.to_string()
    }
}

/// The key a namespace variable takes in the interpreter's variable map: its
/// fully-qualified name without the leading `::`.
///
/// A global keeps its bare name, so nothing a script written before namespaces
/// existed compiles to changes.
pub fn store_key(fqn: &str) -> &str {
    fqn.strip_prefix("::").unwrap_or(fqn)
}

/// The spelling a name a script wrote with a leading `::` takes in a *chunk's*
/// name table — the [`store_key`] with the prefix put back.
///
/// The two are different on purpose. The table key is what the variable is
/// stored under, and `::g` and a global `g` are one variable, so both store as
/// `g`. The chunk name is what a *projected frame* is asked about, and there the
/// two spellings mean opposite things: inside a procedure `$g` is the frame's
/// own local and `$::g` is the interpreter's variable, so a projection that
/// answered both from the frame's view would either hide the global or leak the
/// local. Keeping the prefix in the chunk is what lets the projection tell them
/// apart; [`crate::runtime`] strips it again at every point the name reaches the
/// variable table.
///
/// A name that resolves *into* a namespace — `nsx::v`, or a bare `v` written
/// inside `namespace eval nsx` — needs no marker, because its key already
/// carries `::` and no frame slot's name ever can.
pub(crate) fn chunk_key(fqn: &str) -> String {
    format!("::{}", store_key(fqn))
}

/// Whether `name`, as a chunk's name table spells it, is a namespace variable
/// rather than something a frame could hold — the test a projection makes.
pub(crate) fn is_namespaced(name: &str) -> bool {
    name.contains("::")
}

// ── the compile-time context ─────────────────────────────────────────────

/// What the compiler knows about namespaces while it lowers a script.
pub struct NsCtx {
    /// The namespace whose body is being lowered, fully qualified. `::` at the
    /// script's own level.
    pub current: String,
    /// `variable`'s links inside the procedure body being lowered:
    /// `(namespace, local name)` → the fully-qualified namespace variable the
    /// name stands for.
    ///
    /// Saved and restored around every body by `Compiler::ns_proc`, so a
    /// declaration is scoped to the procedure that made it — one procedure of a
    /// namespace may say `variable v` and another `global v` without either
    /// reaching the other's variable. The namespace is still part of the key,
    /// because a body may be lowered while a namespace-level declaration of the
    /// same name is in scope.
    pub links: HashMap<(String, String), Link>,
    /// Names this chunk passes to `rename`, so a call site for one of them can
    /// be guarded. Only literal first arguments land here; a computed one is
    /// refused where it is written.
    pub renames: BTreeSet<String>,
    /// `namespace export`'s patterns per namespace, as the script wrote them.
    /// The runtime registry keeps the same record; this copy is what
    /// `namespace import` consults while compiling, since an import has to
    /// resolve a *call* and a call is resolved here.
    pub exports: HashMap<String, Vec<String>>,
    /// What `namespace import` brought in: the local name of the imported
    /// command, as a variable-map key, and the key of the command it stands for.
    pub imports: HashMap<String, String>,
}

/// Which command declared a name inside a procedure body, and where it points.
#[derive(Clone, PartialEq, Eq)]
pub enum Link {
    /// `global v` — the variable of the root namespace.
    Global,
    /// `variable v` — the variable of the namespace holding the procedure.
    Variable(String),
}

impl Default for NsCtx {
    fn default() -> Self {
        NsCtx {
            current: "::".to_string(),
            links: HashMap::new(),
            renames: BTreeSet::new(),
            exports: HashMap::new(),
            imports: HashMap::new(),
        }
    }
}

impl NsCtx {
    /// Whether the code being lowered belongs to the root namespace, where every
    /// name resolution is the one this frontend already performed.
    pub fn at_global(&self) -> bool {
        self.current == "::"
    }
}

/// The variable map key a name refers to from where the compiler now stands.
///
/// The one hook namespaces need in the variable path: `Compiler::var_place`
/// calls it for every name that is not a procedure-local slot. At the root
/// namespace with no `variable` declaration in scope it answers the name
/// unchanged, which is why a script that uses no namespace lowers to exactly the
/// bytecode it lowered to before.
pub(crate) fn global_key(c: &Compiler, name: &str) -> String {
    // The compiler's own hidden loop state is named with a leading NUL. It is
    // not a Tcl variable and must not be qualified.
    if name.starts_with('\u{0}') {
        return name.to_string();
    }
    // Inside a procedure body only a declared name reaches the variable map at
    // all; an undeclared one is a frame slot and never gets here.
    if c.scope.is_some() {
        return match c.ns.links.get(&(c.ns.current.clone(), name.to_string())) {
            Some(Link::Variable(fqn)) => store_key(fqn).to_string(),
            // `global v` names the root namespace's variable even from inside a
            // namespace that has one of its own — which is the whole difference
            // between `global` and `variable`.
            Some(Link::Global) => name.to_string(),
            None => resolve_var(c, name),
        };
    }
    resolve_var(c, name)
}

/// A variable name as written in a namespace body: qualified names are
/// absolute or relative as spelt, bare ones belong to the current namespace.
fn resolve_var(c: &Compiler, name: &str) -> String {
    // Written `::x`: the root namespace's variable, said so explicitly. Inside a
    // projection that is a different variable from a bare `x` — the frame's
    // local — so the prefix is kept and the two take separate names. Everywhere
    // else they are one variable and must share one name, or a chunk that wrote
    // through one spelling would read nothing back through the other. See
    // [`chunk_key`] and [`Compiler::projected`].
    if c.projected && name.starts_with("::") {
        return chunk_key(&resolve(&c.ns.current, name));
    }
    if name.contains("::") {
        return store_key(&resolve(&c.ns.current, name)).to_string();
    }
    if c.ns.at_global() {
        return name.to_string();
    }
    store_key(&resolve(&c.ns.current, name)).to_string()
}

// ── the runtime registry ─────────────────────────────────────────────────

/// `TclGetNamespaceFromObj`'s refusal (`generic/tclNamesp.c`): the name as the
/// script wrote it, and — for a relative one — the namespace it was looked up
/// from, with the `TCL LOOKUP NAMESPACE` errorcode.
pub(crate) fn namespace_not_found(written: &str, here: &str) -> TclError {
    let msg = if written.starts_with("::") {
        format!("namespace \"{written}\" not found")
    } else {
        format!("namespace \"{written}\" not found in \"{here}\"")
    };
    TclError {
        errorcode: Some(crate::list::join(&[
            "TCL".to_string(),
            "LOOKUP".to_string(),
            "NAMESPACE".to_string(),
            written.to_string(),
        ])),
        ..TclError::plain(msg)
    }
}

/// What one command in the registry is.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where the command was originally defined, fully qualified. Equal to the
    /// command's own name unless it arrived through `namespace import`, which is
    /// what `namespace origin` reports.
    pub origin: String,
}

/// Every namespace and command the running script has created, which is what
/// the query subcommands read.
///
/// Held by the interpreter rather than by a process-wide table, because two
/// interpreters in one process — which `cargo test` builds by the dozen — have
/// separate namespaces.
#[derive(Default)]
pub struct Registry {
    /// Every namespace that exists, fully qualified. `::` is created on demand
    /// by [`Registry::ensure`].
    namespaces: BTreeSet<String>,
    /// Commands by fully-qualified name.
    commands: BTreeMap<String, Entry>,
    /// Export patterns per namespace, in the order they were added.
    exports: BTreeMap<String, Vec<String>>,
    /// Ensemble commands by fully-qualified command name, made by `namespace
    /// ensemble create`.
    ensembles: BTreeMap<String, Ensemble>,
    /// Commands `rename` or `namespace delete` took away, which is what tells a
    /// deleted command apart from a name that never existed. Read by
    /// [`ext::RENAME_GUARD`].
    gone: BTreeSet<String>,
}

impl Registry {
    /// Create `fqn` and every namespace above it, as `Tcl_CreateNamespace` does
    /// for a qualified name (`generic/tclNamesp.c`).
    pub fn ensure(&mut self, fqn: &str) {
        self.namespaces.insert("::".to_string());
        let mut here = String::from("::");
        for part in components(fqn).1 {
            if here == "::" {
                here = format!("::{part}");
            } else {
                here = format!("{here}::{part}");
            }
            self.namespaces.insert(here.clone());
        }
    }

    pub fn exists(&self, fqn: &str) -> bool {
        fqn == "::" || self.namespaces.contains(fqn)
    }

    /// Record a command defined in place. Its namespace is created too, which is
    /// what makes `namespace exists` true for a namespace whose only mention was
    /// a qualified `proc`.
    pub fn define(&mut self, fqn: &str) {
        let ns = parent_of(fqn);
        if !ns.is_empty() {
            self.ensure(&ns);
        }
        self.gone.remove(fqn);
        self.commands.insert(
            fqn.to_string(),
            Entry {
                origin: fqn.to_string(),
            },
        );
    }

    pub fn command(&self, fqn: &str) -> Option<&Entry> {
        self.commands.get(fqn)
    }

    /// Every command the registry holds and `rename` or `namespace delete` has not
    /// taken away, fully qualified.
    fn live_commands(&self) -> impl Iterator<Item = &String> {
        self.commands
            .keys()
            .filter(|name| !self.gone.contains(*name))
    }

    /// Every command in `ns`, fully qualified and sorted.
    fn commands_in(&self, ns: &str) -> Vec<String> {
        self.commands
            .keys()
            .filter(|name| parent_of(name) == ns)
            .cloned()
            .collect()
    }
}

// ── compiling the commands ───────────────────────────────────────────────

/// Refuse a construct whose namespace this compiler cannot know while lowering.
///
/// Every resolution in this module happens at compile time, so a namespace named
/// by a value is not something to guess at: it decides which variable a `$v`
/// reads and which procedure a call reaches.
fn refuse_dynamic(c: &Compiler, what: &str) -> CompileError {
    // The wording carries "is not supported yet" because that is the phrase the
    // reference-page generator recognises as a refusal (`gen_docs::is_refusal`).
    // A refusal it does not recognise is rendered as *implemented*, which would
    // put a claim on `docs/reference.html` that the compiler contradicts.
    c.err(format!(
        "{what} is not supported yet: this frontend resolves namespaces while compiling, \
         so the name has to be written out"
    ))
}

/// The `namespace` subcommands, in the order tclsh lists them in its
/// `unknown or ambiguous subcommand` message — which is the order this reports
/// too, so the two agree byte for byte.
pub const SUBCOMMANDS: &[&str] = &[
    "children",
    "code",
    "current",
    "delete",
    "ensemble",
    "eval",
    "exists",
    "export",
    "forget",
    "import",
    "inscope",
    "origin",
    "parent",
    "path",
    "qualifiers",
    "tail",
    "unknown",
    "upvar",
    "which",
];

/// tclsh's wording for a subcommand it does not have: the name, then every
/// subcommand it does have, comma separated with `or` before the last
/// (`Tcl_GetIndexFromObj`, `generic/tclIndexObj.c`).
fn bad_subcommand(name: &str) -> String {
    let mut list = String::new();
    for (i, s) in SUBCOMMANDS.iter().enumerate() {
        if i > 0 {
            list.push_str(", ");
        }
        if i + 1 == SUBCOMMANDS.len() {
            list.push_str("or ");
        }
        list.push_str(s);
    }
    format!("unknown or ambiguous subcommand \"{name}\": must be {list}")
}

/// Which subcommand a possibly-abbreviated name selects. Tcl accepts any unique
/// prefix (`Tcl_GetIndexFromObj`), and `namespace ev` appears in real scripts.
fn subcommand_of(name: &str) -> Option<&'static str> {
    if let Some(exact) = SUBCOMMANDS.iter().find(|s| **s == name) {
        return Some(exact);
    }
    let mut hits = SUBCOMMANDS.iter().filter(|s| s.starts_with(name));
    match (hits.next(), hits.next()) {
        (Some(only), None) if !name.is_empty() => Some(only),
        _ => None,
    }
}

impl Compiler {
    /// `namespace subcommand ?arg ...?`.
    pub(crate) fn cmd_namespace(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let Some(first) = args.first() else {
            return self.error("wrong # args: should be \"namespace subcommand ?arg ...?\"");
        };
        let sub = self.literal_of(first, "namespace subcommand")?.to_string();
        let Some(sub) = subcommand_of(&sub) else {
            return self.error(bad_subcommand(&sub));
        };
        let rest = &args[1..];
        match sub {
            "eval" => self.ns_eval(rest),
            // Static by construction: the namespace a command belongs to is
            // decided when the command is lowered, not when it runs.
            "current" => {
                if !rest.is_empty() {
                    return self.error("wrong # args: should be \"namespace current\"");
                }
                let here = self.ns.current.clone();
                self.push_str(&here);
                Ok(())
            }
            "qualifiers" | "tail" => self.ns_name_op(sub, rest),
            "code" => self.ns_runtime(sub, rest, 1..=1, "namespace code arg"),
            "exists" => self.ns_runtime(sub, rest, 1..=1, "namespace exists name"),
            "parent" => self.ns_runtime(sub, rest, 0..=1, "namespace parent ?name?"),
            "children" => self.ns_runtime(sub, rest, 0..=2, "namespace children ?name? ?pattern?"),
            "delete" => {
                self.ns_runtime(sub, rest, 0..=usize::MAX, "namespace delete ?name name...?")
            }
            "export" => {
                self.ns_note_export(rest);
                self.ns_runtime(
                    sub,
                    rest,
                    0..=usize::MAX,
                    "namespace export ?-clear? ?pattern pattern...?",
                )
            }
            "import" => {
                self.ns_note_import(rest)?;
                self.ns_runtime(
                    sub,
                    rest,
                    0..=usize::MAX,
                    "namespace import ?-force? ?pattern pattern...?",
                )
            }
            "forget" => {
                self.ns_note_forget(rest);
                self.ns_runtime(
                    sub,
                    rest,
                    0..=usize::MAX,
                    "namespace forget ?pattern pattern...?",
                )
            }
            "origin" => self.ns_runtime(sub, rest, 1..=1, "namespace origin name"),
            "which" => self.ns_runtime(
                sub,
                rest,
                1..=2,
                "namespace which ?-command? ?-variable? name",
            ),
            "ensemble" => self.ns_runtime(
                sub,
                rest,
                1..=usize::MAX,
                "namespace ensemble subcommand ?arg ...?",
            ),
            "inscope" => self.ns_runtime(
                sub,
                rest,
                2..=usize::MAX,
                "namespace inscope name arg ?arg...?",
            ),
            "upvar" => self.ns_upvar(rest),
            // These two change how a *later* name resolves, which this
            // frontend decided while compiling. Nothing here could honour them.
            "path" | "unknown" => Err(refuse_dynamic(self, &format!("\"namespace {sub}\""))),
            _ => unreachable!("every subcommand above is one of SUBCOMMANDS"),
        }
    }

    /// `namespace qualifiers` and `namespace tail`, folded when the argument is
    /// written out and left to the runtime op when it is not.
    fn ns_name_op(&mut self, sub: &str, rest: &[Word]) -> Result<(), CompileError> {
        let [arg] = rest else {
            return self.error(format!(
                "wrong # args: should be \"namespace {sub} string\""
            ));
        };
        if let Some(text) = arg.as_literal() {
            let answer = if sub == "tail" {
                tail(text)
            } else {
                qualifiers(text)
            };
            self.push_str(answer);
            return Ok(());
        }
        self.ns_runtime(sub, rest, 1..=1, &format!("namespace {sub} string"))
    }

    /// Emit one of the runtime subcommands: the subcommand name, the namespace
    /// the command was written in, then its arguments.
    fn ns_runtime(
        &mut self,
        sub: &str,
        rest: &[Word],
        arity: std::ops::RangeInclusive<usize>,
        usage: &str,
    ) -> Result<(), CompileError> {
        if !arity.contains(&rest.len()) {
            return self.error(format!("wrong # args: should be \"{usage}\""));
        }
        let count = u8::try_from(rest.len() + 2)
            .map_err(|_| self.err(format!("too many arguments for \"namespace {sub}\"")))?;
        self.push_str(sub);
        let here = self.ns.current.clone();
        self.push_str(&here);
        for w in rest {
            self.word(w)?;
        }
        self.emit(Op::Extended(ext::NS, count), 1 - count as i32);
        Ok(())
    }

    /// `namespace upvar ns ?otherVar myVar ...?`.
    ///
    /// `NamespaceUpvarCmd` (`generic/tclNamesp.c:4673`) looks each `otherVar`
    /// up in `ns` alone and links `myVar` to it with `TclPtrMakeUpvar` — the link
    /// `upvar #0` makes to a namespace variable spelt out in full. So the names
    /// are resolved here, `ns` against the current namespace and each
    /// `otherVar` against `ns`, and the command is lowered as that `upvar #0`.
    /// Both have to be written out, as every namespace name here does.
    ///
    /// `ns` must exist when the command runs, and tclsh says so as `namespace
    /// "x" not found in "::"` — `namespace parent ns` raises exactly that, so
    /// it runs first and its value is dropped.
    fn ns_upvar(&mut self, rest: &[Word]) -> Result<(), CompileError> {
        let [ns_w, pairs @ ..] = rest else {
            return self
                .error("wrong # args: should be \"namespace upvar ns ?otherVar myVar ...?\"");
        };
        if !pairs.len().is_multiple_of(2) {
            return self
                .error("wrong # args: should be \"namespace upvar ns ?otherVar myVar ...?\"");
        }
        let Some(ns) = ns_w.as_literal() else {
            return Err(refuse_dynamic(
                self,
                "a computed \"namespace upvar\" namespace",
            ));
        };
        let target = resolve(&self.ns.current, ns);
        let mut words = vec![literal_word("#0")];
        for pair in pairs.chunks(2) {
            let Some(other) = pair[0].as_literal() else {
                return Err(refuse_dynamic(
                    self,
                    "a computed \"namespace upvar\" variable",
                ));
            };
            words.push(literal_word(&resolve(&target, other)));
            words.push(pair[1].clone());
        }
        self.ns_runtime(
            "parent",
            std::slice::from_ref(ns_w),
            0..=1,
            "namespace parent ?name?",
        )?;
        self.emit(Op::Pop, -1);
        if pairs.is_empty() {
            self.push_empty();
            return Ok(());
        }
        self.cmd_upvar(&words)
    }

    /// `namespace eval name arg ?arg ...?` — the one subcommand that is a
    /// compilation rather than a call.
    ///
    /// The body is lowered into the enclosing chunk with the compiler's current
    /// namespace switched, which is what gives every `proc`, `variable` and `$v`
    /// inside it the namespace's names. It is *not* lowered as a nested body:
    /// `namespace eval` at a script's top level runs exactly once, so a `proc`
    /// inside one is as static as a `proc` beside it.
    fn ns_eval(&mut self, rest: &[Word]) -> Result<(), CompileError> {
        let [name_w, body @ ..] = rest else {
            return self.error("wrong # args: should be \"namespace eval name arg ?arg...?\"");
        };
        if body.is_empty() {
            return self.error("wrong # args: should be \"namespace eval name arg ?arg...?\"");
        }
        // Outside a procedure a computed name or body is a computed word like
        // any other: the whole command runs as the list its words make, and
        // there every word is written out (`Compiler::eval_rebuilt`). Inside
        // one the nested script would run against the frame's projection,
        // which is not where a namespace's variables live, so it stays refused.
        if self.scope.is_none() {
            self.literal_of(name_w, "\"namespace eval\" name")?;
            for w in body {
                self.literal_of(w, "\"namespace eval\" body")?;
            }
        }
        let Some(name) = name_w.as_literal() else {
            return Err(refuse_dynamic(self, "a computed \"namespace eval\" name"));
        };
        if self.scope.is_some() {
            // Inside a procedure body an unqualified name is a frame slot, and
            // the body of a `namespace eval` would keep taking slots — so
            // `namespace eval foo {set x 1}` would set a local rather than
            // `::foo::x`. Measured against tclsh: it sets `::foo::x`. Refused
            // rather than answered with the wrong variable.
            return self.error(
                "\"namespace eval\" inside a procedure is not supported yet: an unqualified \
                 name in its body would take a frame slot rather than the namespace's variable",
            );
        }
        let target = resolve(&self.ns.current, name);

        // Several arguments are concatenated with a space and the result is the
        // script, which is what tclsh does (`NamespaceEvalCmd` calls
        // `Tcl_ConcatObj`). Each has to be readable now, for the same reason the
        // name does.
        let mut text = String::new();
        for (i, w) in body.iter().enumerate() {
            let Some(piece) = w.as_literal() else {
                return Err(refuse_dynamic(self, "a computed \"namespace eval\" body"));
            };
            if i > 0 {
                text.push(' ');
            }
            text.push_str(piece);
        }
        let mut script = crate::parser::parse(&text).map_err(|e| self.deferrable_err(e.msg))?;
        // The body is a container of its own: its lines count from it.
        script.base = crate::parser::Base(Some(0));
        // Every `proc` the body defines belongs to the namespace, so the
        // signatures are collected under their qualified names before anything
        // is lowered — which is what lets a procedure defined later in the body
        // be called earlier, as Tcl allows.
        prescan(&mut self.procs, &script, &target);
        // A `namespace eval` nested inside this one defines procedures too, and
        // a call to one of them may be written before the nested block.
        prescan_script(&mut self.procs, &script, &target);

        // Create the namespace before the body runs, so `namespace exists` and
        // `namespace children` see it even when the body defines nothing.
        self.push_str("\u{0}create");
        let here = self.ns.current.clone();
        self.push_str(&here);
        self.push_str(&target);
        self.emit(Op::Extended(ext::NS, 3), -2);
        self.emit(Op::Pop, -1);

        let outer = std::mem::replace(&mut self.ns.current, target);
        // The body is parsed from its own text, so its commands are numbered
        // from 1 and must not move the line a failure is reported at.
        self.body_depth += 1;
        let container = self.emap.enter_body(false);
        let result = self.script_value(&script);
        self.emap.leave_body(container);
        self.body_depth -= 1;
        self.ns.current = outer;
        result
    }

    /// `variable ?name value ...? name ?value?`.
    ///
    /// Two shapes, and they are different operations. In a namespace body it
    /// creates and optionally initialises the namespace's variable. In a
    /// procedure body it *links* the local name to that variable, which is what
    /// [`global_key`] then reads.
    pub(crate) fn cmd_variable(&mut self, args: &[Word]) -> Result<(), CompileError> {
        // `variable` with no arguments is legal and does nothing — measured
        // against tclsh 9.0.4, which answers with the empty string. The loop
        // below already answers that way, so there is nothing to refuse.
        let here = self.ns.current.clone();
        let mut i = 0;
        while i < args.len() {
            let name = self.var_name_of(&args[i])?;
            if name.contains("::") {
                return self.error(format!(
                    "bad variable name \"{name}\": can't create a local variable with a \
                     namespace separator"
                ));
            }
            let fqn = resolve(&here, &name);
            if self.scope.is_some() {
                self.ns
                    .links
                    .insert((here.clone(), name.clone()), Link::Variable(fqn.clone()));
                if let Some(scope) = self.scope.as_mut() {
                    if scope.locals.contains_key(&name) {
                        return self.error(format!("variable \"{name}\" already exists"));
                    }
                    scope.globals.insert(name.clone());
                }
            }
            // A value makes this an assignment; the last name may have none, in
            // which case the variable is only declared and stays unset.
            if let Some(value) = args.get(i + 1) {
                self.scalar_set_guard(&name);
                self.word(value)?;
                self.emit_set_var(&name);
                i += 2;
            } else {
                i += 1;
            }
        }
        self.push_empty();
        Ok(())
    }

    /// `global ?varname ...?`, recording that the names belong to the root
    /// namespace rather than to the enclosing one.
    ///
    /// A thin wrapper over `procs::cmd_global`, which does the declaring. What
    /// it adds is the [`Link::Global`] record, which is what makes `global v`
    /// inside a procedure of `::foo` reach `::v` even when `::foo` has a `v` of
    /// its own — the whole difference between `global` and `variable`. The
    /// record is scoped to the body by [`Compiler::ns_proc`], so two procedures
    /// of one namespace may declare the same name each way.
    pub(crate) fn ns_global(&mut self, args: &[Word]) -> Result<(), CompileError> {
        if self.scope.is_some() && !self.ns.at_global() {
            let here = self.ns.current.clone();
            for w in args {
                let Ok(name) = self.var_name_of(w) else {
                    continue;
                };
                self.ns.links.insert((here.clone(), name), Link::Global);
            }
        }
        self.cmd_global(args)
    }

    /// `proc name args body`, in whichever namespace is current.
    ///
    /// The definition is registered under its qualified name and announced to
    /// the runtime registry, which is what `namespace which`, `namespace origin`
    /// and `rename` then answer from.
    pub(crate) fn ns_proc(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let qualified;
        let args = if self.ns.at_global() {
            args
        } else {
            let [name_w, rest @ ..] = args else {
                return self.cmd_proc(args);
            };
            let Some(name) = name_w.as_literal() else {
                return self.cmd_proc(args);
            };
            let fqn = resolve(&self.ns.current, name);
            qualified = std::iter::once(literal_word(store_key(&fqn)))
                .chain(rest.iter().cloned())
                .collect::<Vec<Word>>();
            &qualified
        };
        // A declaration inside the body belongs to the body. Saving the table
        // here and restoring it after is what scopes `variable` and `global` to
        // one procedure, without the body's own lowering having to know.
        let outer_links = self.ns.links.clone();
        let compiled = self.cmd_proc(args);
        self.ns.links = outer_links;
        compiled?;
        // `proc` leaves the empty string; the announcement leaves it too, so the
        // command's value is unchanged and the depth arithmetic stays exact.
        if let Some(name) = args.first().and_then(|w| w.as_literal()) {
            let fqn = resolve("::", name);
            self.emit(Op::Pop, -1);
            self.push_str("\u{0}define");
            let here = self.ns.current.clone();
            self.push_str(&here);
            self.push_str(&fqn);
            self.emit(Op::Extended(ext::NS, 3), -2);
        }
        Ok(())
    }

    /// `rename oldName newName`.
    ///
    /// The registry is updated when this runs, so every query answers what the
    /// script did. Call *dispatch* is not: this frontend binds a call to a
    /// procedure while compiling, and the chunk holding the `rename` was lowered
    /// before it ran. [`Compiler::ns_guard`] closes the gap that matters —
    /// calling a command the same chunk deleted — and BUGS.md records the rest.
    pub(crate) fn cmd_rename(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [old, new] = args else {
            return self.error("wrong # args: should be \"rename oldName newName\"");
        };
        if let Some(name) = old.as_literal() {
            self.ns.renames.insert(name.to_string());
            self.ns.renames.insert(tail(name).to_string());
            // A rename to a new name is a second name for the same procedure,
            // which is what an import already is — so it is recorded the same
            // way and a call written after the `rename` reaches the body the
            // old name reached. A call written *before* it still reaches the
            // procedure under the new name; BUGS.md records that.
            let from = store_key(&resolve(&self.ns.current, name)).to_string();
            if let Some(to) = new.as_literal().filter(|t| !t.is_empty()) {
                let to = store_key(&resolve(&self.ns.current, to)).to_string();
                // Onto a name that is already a procedure the `rename` is
                // refused when it runs, and nothing becomes a second name.
                if self.procs.contains_key(&from) && !self.procs.contains_key(&to) {
                    self.ns.imports.insert(to, from);
                }
            }
        }
        self.ns_runtime(
            "\u{0}rename",
            &[old.clone(), new.clone()],
            2..=2,
            "rename oldName newName",
        )
    }

    /// Record `namespace export`'s patterns for the namespace being compiled.
    ///
    /// A pattern the script computes is not recorded — the runtime registry
    /// still sees it, so `namespace export` with no arguments still reports it,
    /// but a `namespace import` compiled here cannot match against it and says
    /// so rather than importing nothing quietly.
    fn ns_note_export(&mut self, rest: &[Word]) {
        let here = self.ns.current.clone();
        let list = self.ns.exports.entry(here).or_default();
        for w in rest {
            if let Some(pattern) = w.as_literal() {
                if pattern != "-clear" && !list.contains(&pattern.to_string()) {
                    list.push(pattern.to_string());
                }
            }
        }
    }

    /// Resolve `namespace import`'s patterns against the procedures the script
    /// defines, and record what each import stands for.
    ///
    /// An import is a second name for an existing command, and a call through
    /// either name reaches the same procedure — so here it is a name in the
    /// compiler's own table rather than anything the running code does. A
    /// pattern the compiler cannot read, or one that matches nothing it knows,
    /// is refused: importing a name that then fails to resolve at the call site
    /// would report the wrong command as missing.
    fn ns_note_import(&mut self, rest: &[Word]) -> Result<(), CompileError> {
        let here = self.ns.current.clone();
        for w in rest {
            let Some(pattern) = w.as_literal() else {
                return Err(refuse_dynamic(
                    self,
                    "a computed \"namespace import\" pattern",
                ));
            };
            if pattern == "-force" {
                continue;
            }
            let fqn = resolve(&here, pattern);
            let from = parent_of(&fqn);
            let pat = tail(&fqn).to_string();
            let exported = self.ns.exports.get(&from).cloned().unwrap_or_default();
            let found: Vec<String> = self
                .procs
                .keys()
                .filter(|key| parent_of(&format!("::{key}")) == from)
                .filter(|key| crate::assoc::string_match(tail(key), &pat))
                .filter(|key| {
                    exported
                        .iter()
                        .any(|e| crate::assoc::string_match(tail(key), e))
                })
                .cloned()
                .collect();
            let force = rest.iter().any(|w| w.as_literal() == Some("-force"));
            for origin in found {
                let local = store_key(&resolve(&here, tail(&origin))).to_string();
                if local == origin {
                    continue;
                }
                let held = self.ns.imports.get(&local).cloned();
                if !force
                    && held.as_deref() != Some(origin.as_str())
                    && (held.is_some() || self.procs.contains_key(&local))
                {
                    // Deferred, not refused: tclsh reports this when the
                    // command runs, so `catch {namespace import …}` traps it
                    // there and a script that never reaches the import is not
                    // an error at all.
                    let msg = format!("can't import command \"{}\": already exists", tail(&origin));
                    return Err(self.deferrable_err(msg));
                }
                self.ns.imports.insert(local, origin);
            }
        }
        Ok(())
    }

    /// Undo what `namespace import` recorded, so a call written after the
    /// `namespace forget` no longer resolves through the name it took away.
    ///
    /// The compiler walks a script's commands in order, so an import, a call
    /// and a forget in that order each see the table as the running script
    /// would.
    fn ns_note_forget(&mut self, rest: &[Word]) {
        let here = self.ns.current.clone();
        for w in rest {
            let Some(pattern) = w.as_literal() else {
                continue;
            };
            let fqn = resolve(&here, pattern);
            let from = parent_of(&fqn);
            let pat = tail(&fqn).to_string();
            self.ns.imports.retain(|local, origin| {
                parent_of(&format!("::{local}")) != here
                    || parent_of(&format!("::{origin}")) != from
                    || !crate::assoc::string_match(tail(origin), &pat)
            });
        }
    }

    /// The key of the run-time procedure `name` reaches from the namespace being
    /// compiled, when a `proc` outside the script's top level defines it.
    pub(crate) fn runtime_ns_key(&self, name: &str) -> Option<String> {
        let scoped = store_key(&resolve(&self.ns.current, name)).to_string();
        let target = self.ns.imports.get(&scoped).cloned().unwrap_or(scoped);
        self.runtime.contains(&target).then_some(target)
    }

    /// Whether this module claims the command name `name`, and under what
    /// qualified name.
    ///
    /// Two reasons it might. The name resolves to a procedure of the current
    /// namespace, which has to win over a global of the same name
    /// (`TclGetNamespaceForQualName`'s two-step search); or the chunk renames
    /// the name, so the call needs a guard even though it resolves as before.
    pub(crate) fn ns_resolves(&self, name: &str) -> Option<String> {
        let scoped = store_key(&resolve(&self.ns.current, name)).to_string();
        if let Some(origin) = self.ns.imports.get(&scoped) {
            return Some(origin.clone());
        }
        if name.contains("::") {
            return self.procs.contains_key(&scoped).then_some(scoped);
        }
        if !self.ns.at_global() && self.procs.contains_key(&scoped) {
            return Some(scoped);
        }
        if self.ns.renames.contains(name) && self.procs.contains_key(name) {
            return Some(name.to_string());
        }
        None
    }

    /// Call the procedure `ns_resolves` found, guarding the call when the chunk
    /// also renames the name.
    ///
    /// The guard is on the name **as written**, not on the procedure it reaches:
    /// after `rename f g` a call written `g` must run `f`'s body, and a call
    /// written `f` must refuse. Guarding the resolved name would refuse both.
    pub(crate) fn ns_call(
        &mut self,
        written: &str,
        key: &str,
        args: &[Word],
    ) -> Result<(), CompileError> {
        if self.ns.renames.contains(written) || self.ns.renames.contains(tail(written)) {
            self.ns_guard(written);
        }
        self.call_proc(key, args)
    }

    /// Emit the check that refuses a call to a command `rename` has taken away.
    ///
    /// One op, and only at a call site whose name the same chunk hands to
    /// `rename`. Without it `rename p {}` followed by `p` would still reach the
    /// procedure, because the call was bound while compiling.
    fn ns_guard(&mut self, name: &str) {
        let fqn = resolve(&self.ns.current, name);
        self.push_str(&fqn);
        self.emit(Op::Extended(ext::RENAME_GUARD, 1), 0);
        self.emit(Op::Pop, -1);
    }
}

/// Call the procedure [`Compiler::ns_resolves`] found for `name`.
///
/// A free function because a `match` guard cannot bind what it tested: the
/// dispatch arm asks whether the name resolves and this asks again for the
/// answer.
pub(crate) fn call(c: &mut Compiler, name: &str, args: &[Word]) -> Result<(), CompileError> {
    let key = c.ns_resolves(name).expect("the dispatch arm asked first");
    c.ns_call(name, &key, args)
}

/// A word that is exactly this text, as the parser would have produced for a
/// braced word.
fn literal_word(text: &str) -> Word {
    Word {
        parts: if text.is_empty() {
            Vec::new()
        } else {
            vec![Part::Lit(text.to_string())]
        },
        expand: false,
        braced: true,
        quoted: false,
        pos: Default::default(),
    }
}

/// Collect the signature of every procedure a namespace body defines, under its
/// qualified name, before the body is lowered.
///
/// `procs::prescan` reads the same commands; this adds the qualified spelling so
/// that a call written `p` from inside the namespace resolves before the body
/// reaches the definition.
pub fn prescan(procs: &mut HashMap<String, crate::procs::Signature>, script: &Script, ns: &str) {
    for cmd in &script.commands {
        let [head, name, spec, _body] = cmd.words.as_slice() else {
            continue;
        };
        if head.as_literal() != Some("proc") {
            continue;
        }
        let (Some(name), Some(spec)) = (name.as_literal(), spec.as_literal()) else {
            continue;
        };
        let fqn = store_key(&resolve(ns, name)).to_string();
        if let Ok(sig) = crate::procs::parse_signature(spec) {
            procs.insert(fqn, sig);
        }
    }
}

/// Collect, under their qualified names, every procedure the `namespace eval`
/// blocks below `ns` define. Called once per pass before anything is lowered, so
/// that a call written before the block that defines its procedure still
/// resolves — which is what Tcl allows, since a name is looked up when the call
/// runs.
pub fn prescan_script(
    procs: &mut HashMap<String, crate::procs::Signature>,
    script: &Script,
    ns: &str,
) {
    walk(procs, script, ns);
}

fn walk(procs: &mut HashMap<String, crate::procs::Signature>, script: &Script, ns: &str) {
    for cmd in &script.commands {
        let [head, sub, name, body @ ..] = cmd.words.as_slice() else {
            continue;
        };
        if head.as_literal() != Some("namespace") {
            continue;
        }
        if sub.as_literal().and_then(subcommand_of) != Some("eval") {
            continue;
        }
        let (Some(name), true) = (name.as_literal(), !body.is_empty()) else {
            continue;
        };
        let target = resolve(ns, name);
        let mut text = String::new();
        for (i, w) in body.iter().enumerate() {
            let Some(piece) = w.as_literal() else {
                return;
            };
            if i > 0 {
                text.push(' ');
            }
            text.push_str(piece);
        }
        let Ok(inner) = crate::parser::parse(&text) else {
            continue;
        };
        prescan(procs, &inner, &target);
        walk(procs, &inner, &target);
    }
}

// ── running the commands ─────────────────────────────────────────────────

/// The runtime half: everything a query needs the interpreter's own state for.
pub(crate) fn extension(
    interp: &crate::runtime::Shared,
    vm: &mut fusevm::VM,
    id: u16,
    argc: u8,
) -> Result<(), TclError> {
    let mut values = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        values.push(vm.pop());
    }
    values.reverse();
    if id == ext::RENAME_GUARD {
        let name = to_tcl_string(&values[0]);
        let gone = {
            let state = interp.lock().expect("interpreter lock");
            state.ns.command(&name).is_none() && state.ns.renamed_away(&name)
        };
        if gone {
            return Err(TclError::plain(format!(
                "invalid command name \"{}\"",
                tail(&name)
            )));
        }
        vm.push(values.into_iter().next().expect("the guard takes one name"));
        return Ok(());
    }
    let sub = to_tcl_string(&values[0]);
    let here = to_tcl_string(&values[1]);
    let args: Vec<String> = values[2..].iter().map(to_tcl_string).collect();
    // The running chunk's variables live in its slot vector until it ends, so a
    // query that reads or writes one — `namespace which -variable`, and the
    // nested script `namespace inscope` runs — has to see them written back
    // first and re-read afterwards. That is the same trade the `eval` command
    // makes, and it is why this goes through the interpreter rather than the VM.
    let result =
        crate::runtime::with_written_back(interp, vm, |interp| run(interp, &sub, &here, &args))?;
    // A `rename` or a delete leaves the callbacks of the traces on the command
    // here, to run now that the registry is free.
    crate::cmd_trace::run_events(interp, vm)?;
    vm.push(Value::Str(std::sync::Arc::new(result)));
    Ok(())
}

impl Registry {
    /// Whether a name was ever taken away by `rename` or `namespace delete`,
    /// which is what tells a deleted command apart from one that never existed.
    fn renamed_away(&self, fqn: &str) -> bool {
        self.gone.contains(fqn)
    }
}

/// The names `info commands` (`builtins`) or `info procs` reports from the
/// namespace `here`.
///
/// Built from the registry rather than from the chunk's own procedures, because
/// the registry is what a `rename`, a `namespace delete` and a definition that
/// has not run yet all update: `info commands` after `rename f g` lists `g` and
/// not `f`, and before a `proc` runs it does not list it at all. A qualified
/// pattern is matched against fully qualified names, which are returned as they
/// are; anything else answers the bare names of `here` and — for `commands` —
/// of the global namespace, the two places a command name resolves.
pub(crate) fn visible_commands(
    reg: &Registry,
    here: &str,
    builtins: bool,
    qualified: bool,
) -> Vec<String> {
    let mut fqns: Vec<String> = reg.live_commands().cloned().collect();
    if builtins {
        fqns.extend(
            crate::names::commands()
                .into_iter()
                .map(|name| format!("::{name}"))
                .filter(|fqn| !reg.gone.contains(fqn)),
        );
    }
    if qualified {
        return fqns;
    }
    fqns.into_iter()
        .filter(|fqn| {
            let parent = parent_of(fqn);
            parent == here || (builtins && parent == "::")
        })
        .map(|fqn| tail(&fqn).to_string())
        .collect()
}

/// `rename oldName newName`, when it runs.
///
/// The registry records that the old name is gone and that the new one names what
/// it named, which is what the `namespace` queries answer from. The run-time
/// command table has to follow, because that is what a call *resolves* through: a
/// procedure registered there under the old name would keep answering to it, and
/// in tclsh 9.0.4 `proc f {} {}; rename f g; f` is `invalid command name "f"`
/// (measured). A call written after the rename reaches the body through the new
/// name — the compiler records that as a second name for the procedure — and one
/// written before it reaches it too, which BUGS.md records.
fn rename(
    state: &mut crate::runtime::State,
    here: &str,
    args: &[String],
) -> Result<String, TclError> {
    let old = resolve(here, &args[0]);
    // `TclRenameCommand` (`generic/tclBasic.c`): the old command is found first,
    // an empty new name deletes it, and the destination is checked *before* the
    // old entry is touched — so `rename f f` is `command already exists` and
    // leaves `f` where it was. The destination's namespace is created when it
    // is unknown, as `Tcl_CreateObjCommand` would.
    if !state.ns.commands.contains_key(&old) {
        let verb = if args[1].is_empty() {
            "delete"
        } else {
            "rename"
        };
        return Err(TclError::plain(format!(
            "can't {verb} \"{}\": command doesn't exist",
            args[0]
        )));
    }
    let new = (!args[1].is_empty()).then(|| resolve(here, &args[1]));
    if let Some(new) = &new {
        if state.ns.commands.contains_key(new) {
            return Err(TclError::plain(format!(
                "can't rename to \"{}\": command already exists",
                args[1]
            )));
        }
        let ns = parent_of(new);
        if !ns.is_empty() {
            state.ns.ensure(&ns);
        }
    }
    let entry = state
        .ns
        .commands
        .remove(&old)
        .expect("the old command was found above");
    let defined = state.commands.remove(store_key(&old));
    state.ns.gone.insert(old);
    let Some(new) = new else {
        return Ok(String::new());
    };
    state.ns.gone.remove(&new);
    if let Some(defined) = defined {
        state.commands.insert(store_key(&new).to_string(), defined);
    }
    state.ns.commands.insert(new, entry);
    Ok(String::new())
}

fn run(
    interp: &crate::runtime::Shared,
    sub: &str,
    here: &str,
    args: &[String],
) -> Result<String, TclError> {
    // The one subcommand that runs a script of its own, so it cannot hold the
    // interpreter lock: the script it evaluates needs the same interpreter.
    if sub == "inscope" {
        return inscope(interp, here, args);
    }
    let mut state = interp.lock().expect("interpreter lock");
    state.ns.ensure(here);
    // `which` is the one query that reads the *variables* as well as the
    // registry, so it is answered while both are still reachable.
    if sub == "which" {
        return which(&state, here, args);
    }
    // `rename` moves an entry in the run-time command table as well as in the
    // registry, so it is answered while both halves of the interpreter are
    // reachable rather than through the registry alone.
    if sub == "\u{0}rename" {
        let (old, new) = (resolve(here, &args[0]), args[1].clone());
        let done = rename(&mut state, here, args)?;
        let op = if new.is_empty() { "delete" } else { "rename" };
        let new = if new.is_empty() {
            new
        } else {
            resolve(here, &new)
        };
        crate::cmd_trace::queue_command_events(&mut state.traces, &old, &new, op);
        return Ok(done);
    }
    let reg = &mut state.ns;
    match sub {
        "\u{0}create" => {
            reg.ensure(&args[0]);
            Ok(String::new())
        }
        "\u{0}define" => {
            reg.define(&args[0]);
            Ok(String::new())
        }
        "qualifiers" => Ok(qualifiers(&args[0]).to_string()),
        "tail" => Ok(tail(&args[0]).to_string()),
        "exists" => {
            let fqn = resolve(here, &args[0]);
            Ok(u8::from(reg.exists(&fqn)).to_string())
        }
        "parent" => {
            let fqn = match args.first() {
                Some(name) => resolve(here, name),
                None => here.to_string(),
            };
            if !reg.exists(&fqn) {
                let written = args.first().map_or(here, String::as_str);
                return Err(namespace_not_found(written, here));
            }
            Ok(parent_of(&fqn))
        }
        "children" => {
            let fqn = match args.first() {
                Some(name) => resolve(here, name),
                None => here.to_string(),
            };
            if !reg.exists(&fqn) {
                let written = args.first().map_or(here, String::as_str);
                return Err(namespace_not_found(written, here));
            }
            // A pattern with no `::` is matched against children of `fqn`; one
            // with `::` is qualified first, as `Tcl_GetNamespaceChildren` does.
            let pattern = args.get(1).map(|p| {
                if p.contains("::") {
                    resolve(here, p)
                } else if fqn == "::" {
                    format!("::{p}")
                } else {
                    format!("{fqn}::{p}")
                }
            });
            let kids: Vec<String> = reg
                .namespaces
                .iter()
                .filter(|ns| parent_of(ns) == fqn)
                .filter(|ns| match &pattern {
                    Some(p) => crate::assoc::string_match(ns, p),
                    None => true,
                })
                .cloned()
                .collect();
            Ok(crate::list::join(&kids))
        }
        "delete" => {
            for name in args {
                let fqn = resolve(here, name);
                if !reg.exists(&fqn) {
                    return Err(TclError::plain(format!(
                        "unknown namespace \"{name}\" in namespace delete command"
                    )));
                }
                let prefix = format!("{}::", fqn.trim_end_matches(':'));
                reg.namespaces
                    .retain(|ns| *ns != fqn && !ns.starts_with(&prefix));
                let doomed: Vec<String> = reg
                    .commands
                    .keys()
                    .filter(|c| parent_of(c) == fqn || c.starts_with(&prefix))
                    .cloned()
                    .collect();
                for c in doomed {
                    reg.commands.remove(&c);
                    reg.gone.insert(c);
                }
                reg.exports.remove(&fqn);
                reg.ensembles.retain(|_, e| e.ns != fqn);
            }
            Ok(String::new())
        }
        "code" => {
            // `namespace code script` is the script wrapped so that evaluating
            // it later runs it in this namespace (`NamespaceCodeCmd`). A script
            // that is already such a wrapper is returned unchanged.
            let script = &args[0];
            if script.starts_with("::namespace inscope ")
                || script.starts_with("namespace inscope ")
            {
                return Ok(script.clone());
            }
            Ok(format!(
                "::namespace inscope {here} {}",
                crate::list::join(std::slice::from_ref(script))
            ))
        }
        "export" => {
            let ns = here.to_string();
            let mut patterns = args.to_vec();
            let clear = patterns.first().map(String::as_str) == Some("-clear");
            if clear {
                patterns.remove(0);
                reg.exports.remove(&ns);
            }
            if patterns.is_empty() && !clear {
                return Ok(crate::list::join(
                    reg.exports.get(&ns).map(Vec::as_slice).unwrap_or(&[]),
                ));
            }
            let list = reg.exports.entry(ns).or_default();
            for p in patterns {
                if p.contains("::") {
                    return Err(TclError::plain(format!(
                        "invalid export pattern \"{p}\": pattern can't specify a namespace"
                    )));
                }
                if !list.contains(&p) {
                    list.push(p);
                }
            }
            Ok(String::new())
        }
        "import" => {
            let mut patterns = args.to_vec();
            let force = patterns.first().map(String::as_str) == Some("-force");
            if force {
                patterns.remove(0);
            }
            for p in patterns {
                if p.is_empty() {
                    return Err(TclError::plain("empty import pattern"));
                }
                let fqn = resolve(here, &p);
                let from = parent_of(&fqn);
                let pat = tail(&fqn).to_string();
                if !reg.exists(&from) {
                    return Err(TclError::plain(format!(
                        "unknown namespace in import pattern \"{p}\""
                    )));
                }
                // `Tcl_Import`: a pattern may not name the namespace it is
                // imported into.
                if from == here {
                    return Err(TclError::plain(if p.contains("::") {
                        format!(
                            "import pattern \"{p}\" tries to import from namespace \"{}\" into itself",
                            tail(&from)
                        )
                    } else {
                        format!("no namespace specified in import pattern \"{p}\"")
                    }));
                }
                let exported = reg.exports.get(&from).cloned().unwrap_or_default();
                let matches: Vec<String> = reg
                    .commands_in(&from)
                    .into_iter()
                    .filter(|c| crate::assoc::string_match(tail(c), &pat))
                    .filter(|c| {
                        exported
                            .iter()
                            .any(|e| crate::assoc::string_match(tail(c), e))
                    })
                    .collect();
                // A pattern that matches nothing is not an error: measured
                // against tclsh 9.0.4, `namespace import a::hidden` for an
                // unexported `hidden` and `namespace import a::nothere` for a
                // name that does not exist both answer 0 with an empty result.
                for origin in matches {
                    let local = if here == "::" {
                        format!("::{}", tail(&origin))
                    } else {
                        format!("{here}::{}", tail(&origin))
                    };
                    let root = reg
                        .commands
                        .get(&origin)
                        .map(|e| e.origin.clone())
                        .unwrap_or_else(|| origin.clone());
                    // Importing the same command twice is a no-op, and only a
                    // *different* command under a name already taken is the
                    // error — measured: re-importing `b::p` answers 0, while
                    // importing `a::p` over it is `can't import command "p":
                    // already exists`.
                    if !force {
                        if let Some(held) = reg.commands.get(&local) {
                            if held.origin != root {
                                return Err(TclError::plain(format!(
                                    "can't import command \"{}\": already exists",
                                    tail(&origin)
                                )));
                            }
                        }
                    }
                    reg.gone.remove(&local);
                    reg.commands.insert(local, Entry { origin: root });
                }
            }
            Ok(String::new())
        }
        "forget" => {
            // `Tcl_ForgetImport` finds the commands the pattern names in the
            // *source* namespace, then deletes the imports of them that the
            // current namespace holds — not the originals, which is what makes
            // `namespace forget a::p` leave `::a::p` alone.
            for p in args {
                let fqn = resolve(here, p);
                let from = parent_of(&fqn);
                let pat = tail(&fqn).to_string();
                if !reg.exists(&from) {
                    return Err(TclError::plain(format!(
                        "unknown namespace in namespace forget pattern \"{p}\""
                    )));
                }
                let sources: Vec<String> = reg
                    .commands_in(&from)
                    .into_iter()
                    .filter(|c| crate::assoc::string_match(tail(c), &pat))
                    .collect();
                let doomed: Vec<String> = reg
                    .commands_in(here)
                    .into_iter()
                    .filter(|local| {
                        !sources.contains(local)
                            && reg
                                .commands
                                .get(local)
                                .is_some_and(|e| sources.contains(&e.origin))
                    })
                    .collect();
                for c in doomed {
                    reg.commands.remove(&c);
                    reg.gone.insert(c);
                }
            }
            Ok(String::new())
        }
        "origin" => {
            let Some(found) = which_command(reg, here, &args[0]) else {
                return Err(TclError::plain(format!(
                    "invalid command name \"{}\"",
                    args[0]
                )));
            };
            Ok(reg
                .commands
                .get(&found)
                .map(|e| e.origin.clone())
                .unwrap_or(found))
        }
        "ensemble" => ensemble(reg, here, args),
        other => Err(TclError::plain(bad_subcommand(other))),
    }
}

/// `namespace which ?-command? ?-variable? name`.
///
/// A command is looked for in the registry, a variable in the interpreter's
/// variable map; both take the two-step search — the current namespace, then the
/// root. tclsh answers the empty string rather than an error for a name that
/// resolves to nothing, which is what makes `namespace which` the way a script
/// asks whether a command exists.
fn which(state: &crate::runtime::State, here: &str, args: &[String]) -> Result<String, TclError> {
    // `Tcl_GetIndexFromObj` over the two options, unique prefixes included; a
    // word that names neither is the usage error, not a bad option.
    let (kind, name) =
        match args {
            [name] => ("-command", name.as_str()),
            [flag, name] => {
                let hits: Vec<&str> = ["-command", "-variable"]
                    .into_iter()
                    .filter(|o| !flag.is_empty() && o.starts_with(flag.as_str()))
                    .collect();
                match hits.as_slice() {
                    [one] => (*one, name.as_str()),
                    _ => return Err(TclError::plain(
                        "wrong # args: should be \"namespace which ?-command? ?-variable? name\"",
                    )),
                }
            }
            _ => unreachable!("the arity was checked while compiling"),
        };
    match kind {
        "-command" => Ok(which_command(&state.ns, here, name).unwrap_or_default()),
        "-variable" => {
            let scoped = resolve(here, name);
            if state.globals.contains_key(store_key(&scoped)) {
                return Ok(scoped);
            }
            if !name.starts_with("::") {
                let global = resolve("::", name);
                if state.globals.contains_key(store_key(&global)) {
                    return Ok(global);
                }
            }
            Ok(String::new())
        }
        other => Err(TclError::plain(format!(
            "bad option \"{other}\": must be -command or -variable"
        ))),
    }
}

/// `namespace inscope ns script ?arg ...?` — evaluate `script` in `ns`, with the
/// extra arguments appended as list elements (`NamespaceInscopeCmd`).
///
/// The appending is what makes a `namespace code` callback take arguments, and
/// it quotes each one, so a value with a space in it stays one word.
fn inscope(
    interp: &crate::runtime::Shared,
    here: &str,
    args: &[String],
) -> Result<String, TclError> {
    let ns = resolve(here, &args[0]);
    if !interp.lock().expect("interpreter lock").ns.exists(&ns) {
        return Err(namespace_not_found(&args[0], here));
    }
    let mut script = args[1].clone();
    for extra in &args[2..] {
        script.push(' ');
        script.push_str(&crate::list::join(std::slice::from_ref(extra)));
    }
    // Evaluated through the ordinary compile path, so the script sees the
    // namespace's variables and procedures exactly as a `namespace eval` body
    // does. What is *not* modelled is `uplevel`'s view of the extra call frame
    // Tcl pushes; nothing in this frontend can observe it, since `uplevel` and
    // `info level` are not implemented.
    let source = format!(
        "namespace eval {} {{\n{script}\n}}",
        crate::list::join(std::slice::from_ref(&ns))
    );
    crate::runtime::run_source(interp, &source).map(|v| to_tcl_string(&v))
}

/// `namespace which -command` — the two-step search: the current namespace,
/// then the root (`TclGetNamespaceForQualName`).
fn which_command(reg: &Registry, here: &str, name: &str) -> Option<String> {
    let scoped = resolve(here, name);
    if reg.commands.contains_key(&scoped) {
        return Some(scoped);
    }
    if !name.starts_with("::") {
        let global = resolve("::", name);
        if reg.commands.contains_key(&global) {
            return Some(global);
        }
    }
    // A command this frontend implements itself lives in the root namespace and
    // is in no registry, because nothing created it: `crate::names::commands` is
    // the same list the compiler dispatches on, so asking it here cannot drift
    // from what a call would actually reach.
    let bare = tail(&scoped);
    if parent_of(&scoped) == "::" && crate::names::commands().contains(&bare) {
        return Some(scoped);
    }
    None
}

/// One ensemble command: the configuration `namespace ensemble create` gave it
/// (`generic/tclEnsemble.c`).
pub struct Ensemble {
    /// The namespace the ensemble belongs to, `-namespace`.
    ns: String,
    /// `-map`: subcommand to command prefix, the prefix's first word
    /// qualified against `ns` as `create` does. `None` when not given.
    map: Option<Vec<(String, Vec<String>)>>,
    /// `-subcommands`. `None` (or empty) when not given.
    subcommands: Vec<String>,
    /// `-prefixes`.
    prefixes: bool,
    /// `-parameters` and `-unknown`, which dispatch refuses when set.
    parameters: String,
    unknown: String,
}

/// `TclListObjGetElements` for an option value, as a `TclError`.
fn words(value: &str) -> Result<Vec<String>, TclError> {
    crate::list::split(value).map_err(TclError::plain)
}

/// `Tcl_GetBooleanFromObj`'s reading of `-prefixes`.
fn boolean(value: &str) -> Result<bool, TclError> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "t" | "y" | "tr" | "tru" | "ye" => Ok(true),
        "0" | "false" | "no" | "off" | "f" | "n" | "fa" | "fal" | "fals" | "of" => Ok(false),
        _ => value
            .trim()
            .parse::<i64>()
            .map(|n| n != 0)
            .map_err(|_| TclError::plain(format!("expected boolean value but got \"{value}\""))),
    }
}

/// `namespace ensemble subcommand ?arg ...?`.
///
/// `create` records the ensemble's configuration, `exists` and `configure`
/// read it, and [`ensemble_call`] dispatches a call to the command when it
/// runs.
fn ensemble(reg: &mut Registry, here: &str, args: &[String]) -> Result<String, TclError> {
    const CREATE_OPTIONS: &[&str] = &[
        "-command",
        "-map",
        "-parameters",
        "-prefixes",
        "-subcommands",
        "-unknown",
    ];
    const CONFIGURE_OPTIONS: &[&str] = &[
        "-map",
        "-namespace",
        "-parameters",
        "-prefixes",
        "-subcommands",
        "-unknown",
    ];
    // `Tcl_GetIndexFromObj(… "subcommand" …)`: a unique prefix names one.
    const SUBS: [&str; 3] = ["configure", "create", "exists"];
    let word = args[0].as_str();
    let hits: Vec<&str> = SUBS
        .into_iter()
        .filter(|s| *s == word || (!word.is_empty() && s.starts_with(word)))
        .collect();
    let sub = match hits.as_slice() {
        [one] => *one,
        _ if SUBS.contains(&word) => word,
        [] => {
            return Err(TclError::plain(format!(
                "bad subcommand \"{word}\": must be configure, create, or exists"
            )))
        }
        _ => {
            return Err(TclError::plain(format!(
                "ambiguous subcommand \"{word}\": must be configure, create, or exists"
            )))
        }
    };
    let usage = |u: &str| {
        TclError::plain(format!(
            "wrong # args: should be \"namespace ensemble {u}\""
        ))
    };
    match (sub, args.len()) {
        ("exists", n) if n != 2 => return Err(usage("exists cmdname")),
        ("configure", n) if n == 1 || (n > 3 && n % 2 == 1) => {
            return Err(usage("configure cmdname ?-option value ...? ?arg ...?"))
        }
        ("create", n) if n % 2 == 0 => return Err(usage("create ?option value ...?")),
        _ => {}
    }
    match sub {
        "create" => {
            let mut cmd = here.to_string();
            let mut e = Ensemble {
                ns: here.to_string(),
                map: None,
                subcommands: Vec::new(),
                prefixes: true,
                parameters: String::new(),
                unknown: String::new(),
            };
            for pair in args[1..].chunks(2) {
                let value = pair.get(1).map(String::as_str).unwrap_or("");
                match option(&pair[0], CREATE_OPTIONS)? {
                    "-command" => cmd = resolve(here, value),
                    opt => set_option(&mut e, here, opt, value)?,
                }
            }
            reg.define(&cmd);
            reg.ensembles.insert(cmd.clone(), e);
            Ok(cmd)
        }
        "exists" => {
            let fqn = resolve(here, args.get(1).map(String::as_str).unwrap_or(""));
            Ok(u8::from(reg.ensembles.contains_key(&fqn)).to_string())
        }
        "configure" => {
            let fqn = resolve(here, args.get(1).map(String::as_str).unwrap_or(""));
            let Some(e) = reg.ensembles.get_mut(&fqn) else {
                let written = args.get(1).map(String::as_str).unwrap_or("");
                return Err(TclError::plain(
                    if which_command(reg, here, written).is_some() {
                        format!("\"{written}\" is not an ensemble command")
                    } else {
                        format!("unknown command \"{written}\"")
                    },
                ));
            };
            // `configure cmd -option` reads one; `-option value ...` sets them.
            if args.len() == 3 {
                return Ok(option_value(e, option(&args[2], CONFIGURE_OPTIONS)?));
            }
            if args.len() > 3 {
                for pair in args[2..].chunks(2) {
                    match option(&pair[0], CONFIGURE_OPTIONS)? {
                        "-namespace" => {
                            return Err(TclError::plain("option -namespace is read-only"))
                        }
                        opt => set_option(e, here, opt, &pair[1])?,
                    }
                }
                return Ok(String::new());
            }
            let e = &*e;
            let listing: Vec<String> = CONFIGURE_OPTIONS
                .iter()
                .flat_map(|o| [o.to_string(), option_value(e, o)])
                .collect();
            Ok(crate::list::join(&listing))
        }
        _ => unreachable!("resolved against SUBS above"),
    }
}

/// The command a call to the ensemble command `name` runs, as words, when
/// `name` is one — `TclEnsembleImplementationCmd` (`generic/tclEnsemble.c`):
/// the subcommand table is `-subcommands`, else the keys of `-map`, else the
/// commands the namespace exports; the subcommand is looked up exactly and
/// then, under `-prefixes`, as a unique prefix; and it runs as its `-map`
/// prefix, or as the namespace's command of that name, followed by the rest of
/// the arguments. `Ok(None)` when `name` is not an ensemble.
pub(crate) fn ensemble_call(
    reg: &Registry,
    name: &str,
    args: &[String],
) -> Result<Option<Vec<String>>, String> {
    let Some(e) = reg.ensembles.get(&resolve("::", name)) else {
        return Ok(None);
    };
    if !e.parameters.is_empty() || !e.unknown.is_empty() {
        return Err(format!(
            "ensemble \"{name}\": -parameters and -unknown are not supported yet"
        ));
    }
    let Some((sub, rest)) = args.split_first() else {
        return Err(format!(
            "wrong # args: should be \"{name} subcommand ?arg ...?\""
        ));
    };
    let mut table: Vec<String> = if !e.subcommands.is_empty() {
        e.subcommands.clone()
    } else if let Some(map) = &e.map {
        map.iter().map(|(k, _)| k.clone()).collect()
    } else {
        let exported = reg.exports.get(&e.ns).cloned().unwrap_or_default();
        reg.commands_in(&e.ns)
            .iter()
            .map(|c| tail(c).to_string())
            .filter(|t| exported.iter().any(|p| crate::assoc::string_match(t, p)))
            .collect()
    };
    table.sort();
    table.dedup();
    let chosen = if table.contains(sub) {
        Some(sub.clone())
    } else if e.prefixes {
        let mut hits = table.iter().filter(|t| t.starts_with(sub.as_str()));
        match (hits.next(), hits.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        }
    } else {
        None
    };
    let Some(chosen) = chosen else {
        if table.is_empty() {
            return Err(format!(
                "unknown subcommand \"{sub}\": namespace {} does not export any commands",
                e.ns
            ));
        }
        let what = if e.prefixes {
            "unknown or ambiguous subcommand"
        } else {
            "unknown subcommand"
        };
        let listed = match table.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [init @ .., last] => format!("{}, or {last}", init.join(", ")),
        };
        return Err(format!("{what} \"{sub}\": must be {listed}"));
    };
    let mut target = e
        .map
        .iter()
        .flatten()
        .find(|(k, _)| *k == chosen)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| vec![resolve(&e.ns, &chosen)]);
    // A command of the global namespace is reached by its plain name, which is
    // how this frontend's own commands are known.
    if parent_of(&target[0]) == "::" {
        target[0] = tail(&target[0]).to_string();
    }
    target.extend(rest.iter().cloned());
    Ok(Some(target))
}

/// `Tcl_GetIndexFromObj(… "option" …)` over an ensemble's option names, unique
/// prefixes included.
fn option<'a>(word: &str, table: &[&'a str]) -> Result<&'a str, TclError> {
    if let Some(exact) = table.iter().find(|o| **o == word) {
        return Ok(exact);
    }
    let hits: Vec<&str> = table
        .iter()
        .copied()
        .filter(|o| !word.is_empty() && o.starts_with(word))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        _ => {
            let listed = match table {
                [init @ .., last] => format!("{}, or {last}", init.join(", ")),
                [] => String::new(),
            };
            let what = if hits.is_empty() { "bad" } else { "ambiguous" };
            Err(TclError::plain(format!(
                "{what} option \"{word}\": must be {listed}"
            )))
        }
    }
}

/// Set one of an ensemble's writable options (`create` and `configure`).
fn set_option(e: &mut Ensemble, here: &str, opt: &str, value: &str) -> Result<(), TclError> {
    match opt {
        "-map" => {
            let flat = words(value)?;
            if flat.len() % 2 != 0 {
                return Err(TclError::plain("missing value to go with key"));
            }
            let mut map = Vec::new();
            for pair in flat.chunks(2) {
                let mut prefix = words(&pair[1])?;
                if prefix.is_empty() {
                    return Err(TclError::plain(
                        "ensemble subcommand implementations must be non-empty lists",
                    ));
                }
                prefix[0] = resolve(here, &prefix[0]);
                map.push((pair[0].clone(), prefix));
            }
            e.map = (!map.is_empty()).then_some(map);
        }
        "-parameters" => e.parameters = value.to_string(),
        "-prefixes" => e.prefixes = boolean(value)?,
        "-subcommands" => e.subcommands = words(value)?,
        "-unknown" => e.unknown = value.to_string(),
        _ => unreachable!("options are resolved against the table first"),
    }
    Ok(())
}

/// One option's value, as `namespace ensemble configure` reports it.
fn option_value(e: &Ensemble, opt: &str) -> String {
    match opt {
        "-map" => {
            let map: Vec<String> = e
                .map
                .iter()
                .flatten()
                .flat_map(|(k, v)| [k.clone(), crate::list::join(v)])
                .collect();
            crate::list::join(&map)
        }
        "-namespace" => e.ns.clone(),
        "-parameters" => e.parameters.clone(),
        "-prefixes" => u8::from(e.prefixes).to_string(),
        "-subcommands" => crate::list::join(&e.subcommands),
        _ => e.unknown.clone(),
    }
}
