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
//! on knowing it. `namespace path`, `namespace unknown` and `namespace upvar`
//! change resolution at run time and are refused for the same reason. See
//! [`refuse_dynamic`].

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
    /// chunk also passes to `rename`; see [`Compiler::ns_guard`].
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
    /// Saved and restored around every body by [`Compiler::ns_proc`], so a
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
/// The one hook namespaces need in the variable path: [`Compiler::var_place`]
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
    if name.contains("::") {
        return store_key(&resolve(&c.ns.current, name)).to_string();
    }
    if c.ns.at_global() {
        return name.to_string();
    }
    store_key(&resolve(&c.ns.current, name)).to_string()
}

// ── the runtime registry ─────────────────────────────────────────────────

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
    /// Namespaces made into an ensemble by `namespace ensemble create`.
    ensembles: BTreeSet<String>,
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
                "namespace inscope ns script ?arg...?",
            ),
            // These three change how a *later* name resolves, which this
            // frontend decided while compiling. Nothing here could honour them.
            "path" | "unknown" | "upvar" => {
                Err(refuse_dynamic(self, &format!("\"namespace {sub}\"")))
            }
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
        let script = crate::parser::parse(&text).map_err(|e| self.deferrable_err(e.msg))?;
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
        let result = self.script_value(&script);
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
                if self.procs.contains_key(&from) {
                    let to = store_key(&resolve(&self.ns.current, to)).to_string();
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
        if let Ok(sig) = crate::procs::parse_signature(&fqn, spec) {
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
        "\u{0}rename" => {
            let old = resolve(here, &args[0]);
            let Some(entry) = reg.commands.remove(&old) else {
                return Err(TclError::plain(format!(
                    "can't rename \"{}\": command doesn't exist",
                    args[0]
                )));
            };
            reg.gone.insert(old);
            if args[1].is_empty() {
                return Ok(String::new());
            }
            let new = resolve(here, &args[1]);
            if reg.commands.contains_key(&new) {
                return Err(TclError::plain(format!(
                    "can't rename to \"{}\": command already exists",
                    args[1]
                )));
            }
            let ns = parent_of(&new);
            if !ns.is_empty() && !reg.exists(&ns) {
                return Err(TclError::plain(format!(
                    "can't rename to \"{}\": unknown namespace",
                    args[1]
                )));
            }
            reg.gone.remove(&new);
            reg.commands.insert(new, entry);
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
                return Err(TclError::plain(format!("namespace \"{fqn}\" not found")));
            }
            Ok(parent_of(&fqn))
        }
        "children" => {
            let fqn = match args.first() {
                Some(name) => resolve(here, name),
                None => here.to_string(),
            };
            if !reg.exists(&fqn) {
                return Err(TclError::plain(format!("namespace \"{fqn}\" not found")));
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
                        "unknown namespace \"{fqn}\" in namespace delete command"
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
                reg.ensembles.remove(&fqn);
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
                let fqn = resolve(here, &p);
                let from = parent_of(&fqn);
                let pat = tail(&fqn).to_string();
                if !reg.exists(&from) {
                    return Err(TclError::plain(format!(
                        "unknown namespace in import pattern \"{p}\""
                    )));
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
    let (kind, name) = match args {
        [name] => ("-command", name.as_str()),
        [flag, name] => (flag.as_str(), name.as_str()),
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
        return Err(TclError::plain(format!(
            "unknown namespace \"{ns}\" in inscope namespace command"
        )));
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

/// `namespace ensemble subcommand ?arg ...?`.
///
/// `exists` and `configure` read the registry. `create` records that the
/// namespace is one, so those two answer truthfully — but nothing dispatches
/// through the ensemble, because a call reaches its command while this frontend
/// is compiling and an ensemble decides its callee when it runs. Calling one is
/// therefore a refusal, not a guess.
fn ensemble(reg: &mut Registry, here: &str, args: &[String]) -> Result<String, TclError> {
    const OPTIONS: &str =
        "must be -command, -map, -parameters, -prefixes, -subcommands, or -unknown";
    match args[0].as_str() {
        "create" => {
            let mut ns = here.to_string();
            let mut i = 1;
            while i < args.len() {
                let opt = &args[i];
                if !matches!(
                    opt.as_str(),
                    "-command" | "-map" | "-parameters" | "-prefixes" | "-subcommands" | "-unknown"
                ) {
                    return Err(TclError::plain(format!("bad option \"{opt}\": {OPTIONS}")));
                }
                if opt == "-command" {
                    ns = resolve(here, args.get(i + 1).map(String::as_str).unwrap_or(""));
                }
                i += 2;
            }
            reg.ensembles.insert(here.to_string());
            reg.define(&ns);
            Ok(ns)
        }
        "exists" => {
            let fqn = resolve(here, args.get(1).map(String::as_str).unwrap_or(""));
            Ok(u8::from(reg.ensembles.contains(&fqn)).to_string())
        }
        "configure" => {
            let fqn = resolve(here, args.get(1).map(String::as_str).unwrap_or(""));
            if !reg.ensembles.contains(&fqn) {
                return Err(TclError::plain(format!("unknown command \"{fqn}\"")));
            }
            let exports = reg.exports.get(&fqn).cloned().unwrap_or_default();
            Ok(format!(
                "-map {{}} -namespace {fqn} -parameters {{}} -prefixes 1 -subcommands {{{}}} \
                 -unknown {{}}",
                crate::list::join(&exports)
            ))
        }
        other => Err(TclError::plain(format!(
            "unknown or ambiguous subcommand \"{other}\": must be configure, create, or exists"
        ))),
    }
}
