//! The record of what was executing when an error was raised: the data behind
//! `-errorinfo`, `-errorstack`, `-errorline`, `$::errorInfo` and
//! `$::errorCode`.
//!
//! tclsh builds `errorInfo` as the error unwinds. Each time the error leaves a
//! command it logs the command's source text, and each time it leaves a
//! container — a procedure body, an `eval`, an `uplevel` script, a `foreach`
//! body — it logs where in the container the command sat:
//!
//! ```text
//! inner
//!     while executing
//! "error inner"
//!     (procedure "p" line 1)
//!     invoked from within
//! "p"
//!     ("uplevel" body line 1)
//!     invoked from within
//! "uplevel #0 $script"
//! ```
//!
//! Everything that needs source text has to be recorded while compiling, because
//! the VM runs ops: [`CmdRec`] is that record, one per command, mapping the op
//! range the command lowered to back to its text, its line and — for a command
//! inside the body of `foreach` and its kin, which tclsh reports as a container
//! of its own — the command that owns the body. The records of a chunk are
//! filed by the chunk's `source` field, which this crate otherwise leaves
//! empty, so the VM running a chunk can find them again.
//!
//! When an error is raised [`chain`] walks the running VM's frames and turns the
//! failing op into the list of [`Entry`]s tclsh would have logged. The list
//! travels with the error ([`ErrInfo`]) across nested evaluations — an `eval`,
//! an `uplevel`, a procedure bodied in another chunk — each machine adding its
//! own frames, until a `catch` region absorbs the error and [`ErrInfo::info`]
//! renders it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, VM};

/// What a command's own body is, for the commands tclsh reports as a container
/// between the failing command and the command that owns the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyKind {
    Foreach,
    Lmap,
    DictFor,
    DictMap,
    DictWith,
    DictUpdate,
    NsEval,
    /// `try`: the command itself has no container body; its handlers and its
    /// `finally` script are, and say which with the three kinds below.
    Try,
    TryOn,
    TryTrap,
    TryFinally,
    /// A procedure body: a container of its own, whose lines count from it, but
    /// logged through the frame rather than as a body.
    Proc,
}

impl BodyKind {
    /// The kind of body `name` (and, for `dict`, its subcommand) runs.
    pub(crate) fn of(name: &str, sub: Option<&str>) -> Option<BodyKind> {
        match (name, sub) {
            ("foreach", _) => Some(BodyKind::Foreach),
            ("lmap", _) => Some(BodyKind::Lmap),
            ("dict", Some("for")) => Some(BodyKind::DictFor),
            ("dict", Some("map")) => Some(BodyKind::DictMap),
            ("dict", Some("with")) => Some(BodyKind::DictWith),
            ("dict", Some("update")) => Some(BodyKind::DictUpdate),
            ("namespace", Some("eval")) => Some(BodyKind::NsEval),
            ("try", _) => Some(BodyKind::Try),
            ("proc", _) => Some(BodyKind::Proc),
            _ => None,
        }
    }

    /// The line `Tcl_AppendObjToErrorInfo` adds when an error leaves the body,
    /// given the line of the failing command within it.
    fn context(self, line: usize, owner: &str) -> String {
        match self {
            BodyKind::Foreach => format!("(\"foreach\" body line {line})"),
            BodyKind::Lmap => format!("(\"lmap\" body line {line})"),
            BodyKind::DictFor => format!("(\"dict for\" body line {line})"),
            BodyKind::DictMap => format!("(\"dict map\" body line {line})"),
            BodyKind::DictWith => "(body of \"dict with\")".to_string(),
            BodyKind::DictUpdate => "(body of \"dict update\")".to_string(),
            BodyKind::NsEval => {
                // `namespace eval NS …`: the namespace is the third word.
                let words = crate::list::split(owner.trim_end()).unwrap_or_default();
                let ns = words.get(2).cloned().unwrap_or_default();
                let ns = if ns.starts_with("::") {
                    ns
                } else {
                    format!("::{ns}")
                };
                format!("(in namespace eval \"{ns}\" script line {line})")
            }
            BodyKind::TryOn => format!("(\"try ... on\" handler line {line})"),
            BodyKind::TryTrap => format!("(\"try ... trap\" handler line {line})"),
            BodyKind::TryFinally => format!("(\"try ... finally\" body line {line})"),
            BodyKind::Try | BodyKind::Proc => String::new(),
        }
    }
}

/// One command of a chunk, as the compiler lowered it.
#[derive(Clone, Debug)]
pub(crate) struct CmdRec {
    /// The ops the command lowered to: `start..end`.
    pub start: usize,
    pub end: usize,
    /// The 1-based line the command began on, within the text it was parsed
    /// from — a procedure's body counts from its own first line.
    pub line: usize,
    /// Index of the text in [`CmdMap::sources`] and the command's byte range
    /// in it.
    pub src: usize,
    pub span: (usize, usize),
    /// The kind of body this command's own bodies are.
    pub kind: Option<BodyKind>,
    /// The command whose body this one is directly inside, when that command is
    /// a container of its own ([`BodyKind`]).
    pub outer: Option<(usize, BodyKind)>,
}

/// Every command of one chunk.
#[derive(Debug, Default)]
pub(crate) struct CmdMap {
    pub recs: Vec<CmdRec>,
    pub sources: Vec<Arc<str>>,
}

impl CmdMap {
    fn text(&self, rec: &CmdRec) -> &str {
        self.sources[rec.src]
            .get(rec.span.0..rec.span.1)
            .unwrap_or("")
    }

    /// The innermost command whose ops include `ip`.
    fn innermost(&self, ip: usize) -> Option<usize> {
        self.recs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.start <= ip && ip < r.end)
            .min_by_key(|(_, r)| r.end - r.start)
            .map(|(i, _)| i)
    }
}

/// How many chunks' maps are kept before the table starts over. A script that
/// generates fresh text every pass would otherwise grow it without bound; a map
/// that has gone only costs the trace of an error raised by a chunk compiled
/// that long ago.
const CAPACITY: usize = 4096;

thread_local! {
    /// The lambda terms of the `apply` calls running, innermost last: what
    /// `(lambda term "…")` names. A lambda is run as a procedure of a script this
    /// crate writes, so the term the script wrote is not in it.
    static LAMBDAS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Note that a lambda written `term` is about to run.
pub(crate) fn enter_lambda(term: &str) {
    LAMBDAS.with(|l| l.borrow_mut().push(term.to_string()));
}

/// The lambda that was noted last has ended.
pub(crate) fn leave_lambda() {
    LAMBDAS.with(|l| {
        l.borrow_mut().pop();
    });
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static MAPS: Mutex<Option<HashMap<String, Arc<CmdMap>>>> = Mutex::new(None);

/// File `map` under a fresh key and stamp the key on the chunk.
pub(crate) fn note(chunk: &mut Chunk, map: CmdMap) {
    if map.recs.is_empty() {
        return;
    }
    let key = format!("tclrs:{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let mut guard = MAPS.lock().expect("command map lock");
    let table = guard.get_or_insert_with(HashMap::new);
    if table.len() >= CAPACITY {
        table.clear();
    }
    table.insert(key.clone(), Arc::new(map));
    chunk.source = key;
}

fn lookup(chunk: &Chunk) -> Option<Arc<CmdMap>> {
    let guard = MAPS.lock().expect("command map lock");
    guard.as_ref()?.get(&chunk.source).cloned()
}

// ── what an error carries ────────────────────────────────────────────────

/// What a logged command was a part of, which is the line tclsh adds after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ctx {
    /// Nothing: the command is the outermost one of a chain that stopped
    /// at a `catch`, or whose container was not recorded.
    None,
    /// The body of a `foreach` and its kin; the entry after this one is the
    /// command that owns the body.
    Body(BodyKind),
    /// A procedure body. `name` is what `(procedure "…")` names.
    Proc(String),
    /// A lambda's body, named by the lambda's text.
    Lambda(String),
    /// The outermost command of a script a command ran as a nested evaluation;
    /// which line says so depends on that command, and is filled in by the
    /// machine that runs it ([`ErrInfo::resolve`]).
    Pending,
    /// The same, once resolved: the line to print and the `-errorstack` element
    /// it stands for.
    Done { line: Option<String>, stack: String },
}

/// One command an error left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub text: String,
    pub line: usize,
    pub ctx: Ctx,
    /// How many procedure activations were on the frame stack when the command
    /// ran, and the ops it lowered to: what tells a command inside a `catch`
    /// region from one outside it.
    pub depth: usize,
    pub range: (usize, usize),
    /// What the command's own bodies are, if it has one that is a container.
    pub kind: Option<BodyKind>,
    /// The command is not logged — tclsh does not log `try` — and only what
    /// contained it is.
    pub hidden: bool,
}

/// The trace an error carries: the commands it left, innermost first, and how it
/// began.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct ErrInfo {
    pub entries: Vec<Entry>,
    /// `error msg info`: the error's `errorInfo` already says something, so the
    /// command that raised it is not logged and what follows is "invoked from
    /// within".
    pub preset: Option<String>,
    /// The `INNER` element of `-errorstack`.
    pub inner: String,
    /// The first entry is the command that stated `preset`, which is not logged
    /// again; only what contained it is.
    pub suppress_first: bool,
    /// The `-errorline` and `-errorstack` of the record this one continues, when
    /// it was rebuilt from an options dictionary (`return -options`, a `finally`
    /// re-raising what its region took).
    pub line_hint: usize,
    pub stack_prefix: String,
    /// Whether the first entry is introduced as "invoked from within" rather
    /// than "while executing": errors raised by an expression's arithmetic are,
    /// because their record was begun by the expression.
    pub invoked: bool,
}

/// The word `text` begins with, for the commands whose behavior is decided by
/// the name.
pub(crate) fn first_word(text: &str) -> &str {
    text.split(|c: char| c.is_whitespace() || c == ';')
        .next()
        .unwrap_or("")
}

/// The single line `Tcl_LogCommandInfo` allows a command's text: the first
/// 150 characters, then an ellipsis.
fn clipped(text: &str) -> String {
    const LIMIT: usize = 150;
    let mut it = text.char_indices();
    match it.nth(LIMIT) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_string(),
    }
}

impl ErrInfo {
    /// The frames this error crossed, for the machine that raised it, appended
    /// to what the machines below it already logged.
    ///
    /// `failing` is the op that raised. The chain is every container on the VM's
    /// frame stack from there down, innermost first.
    pub(crate) fn extend(
        &mut self,
        vm: &VM,
        chunk: &Chunk,
        failing: usize,
        started_inside: bool,
        reraise: bool,
        inline: bool,
    ) {
        let before = self.entries.len();
        let mut tail = chain(vm, chunk, failing, started_inside);
        // A `finally` region took the error and is handing it on: what it took is
        // the preset, and the command that owns the region is the first entry.
        // `subst` is compiled in tclsh, so a command substitution failing inside
        // it is the innermost command and `subst` itself is not logged.
        if inline && before > 0 && !tail.is_empty() {
            let dropped = tail.remove(0);
            let last = &mut self.entries[before - 1];
            last.ctx = dropped.ctx;
            last.line = dropped.line;
            last.depth = dropped.depth;
            last.range = dropped.range;
        }
        if reraise {
            if let (Some(owner), Some(preset)) = (tail.first(), self.preset.as_mut()) {
                match owner.kind {
                    Some(kind)
                        if !matches!(kind, BodyKind::Proc | BodyKind::Try) && owner.depth == 0 =>
                    {
                        preset.push_str(&format!(
                            "\n    {}",
                            kind.context(self.line_hint, &owner.text)
                        ));
                    }
                    // `try`, or a body tclsh compiled in a procedure: the command is
                    // not logged, only what held it.
                    _ => self.suppress_first = true,
                }
            }
        }
        if tail.is_empty() {
            return;
        }
        // Everything already logged ran *inside* the frame this machine is in, so
        // it is deeper than every container this chain adds.
        let shift = tail[0].depth + 1;
        for e in &mut self.entries[..before] {
            e.depth += shift;
        }
        // The nested script the previous machine ran ended in an entry that does
        // not know what contained it. It is the command this chain opens with.
        if let Some(last) = self.entries[..before].last_mut() {
            if last.ctx == Ctx::Pending {
                last.ctx = unit_context(&tail[0].text, last.line);
            }
        }
        self.entries.extend(tail);
    }

    /// Settle every entry still waiting for the command that ran its script,
    /// when nothing further out will — the error is being reported.
    pub(crate) fn settle(&mut self) {
        for e in &mut self.entries {
            if e.ctx == Ctx::Pending {
                e.ctx = Ctx::None;
            }
        }
    }

    /// How many entries a `catch` region entered at activation depth `depth`,
    /// whose command lowered to the ops `region`, contains: the leading entries
    /// — innermost first — that ran inside it.
    pub(crate) fn within(&self, depth: usize, region: (usize, usize)) -> usize {
        self.entries
            .iter()
            .take_while(|e| {
                e.depth > depth
                    || (e.depth == depth
                        && e.range.0 >= region.0
                        && e.range.1 <= region.1
                        && e.range != region)
            })
            .count()
    }

    /// `errorInfo` for an error whose message is `msg`, from the first `upto`
    /// entries. The last one's container was not left — a `catch` took the error
    /// inside it — so what contained it is not logged.
    pub(crate) fn info(&self, msg: &str, upto: usize) -> String {
        let upto = upto.min(self.entries.len());
        let mut out = match &self.preset {
            Some(info) => info.clone(),
            None => msg.to_string(),
        };
        let mut first = true;
        for (i, e) in self.entries[..upto].iter().enumerate() {
            if !(i == 0 && self.suppress_first) && !e.hidden {
                let header = if first && !self.invoked && self.preset.is_none() {
                    "while executing"
                } else {
                    "invoked from within"
                };
                first = false;
                out.push_str(&format!("\n    {header}\n\"{}\"", clipped(&e.text)));
            }
            // The last entry's container was not left, unless what it was in is
            // a `try` clause: `try` is not logged, so its clause is the one thing
            // a region cutting here still reports.
            let clause = matches!(
                e.ctx,
                Ctx::Body(BodyKind::TryOn | BodyKind::TryTrap | BodyKind::TryFinally)
            );
            if i + 1 == upto && !clause {
                break;
            }
            let context = match &e.ctx {
                Ctx::Body(kind) => {
                    let owner = self.entries.get(i + 1).map_or("", |n| n.text.as_str());
                    Some(kind.context(e.line, owner))
                }
                Ctx::Proc(name) => Some(format!("(procedure \"{name}\" line {})", e.line)),
                Ctx::Lambda(text) => Some(format!("(lambda term \"{text}\" line {})", e.line)),
                Ctx::Done { line, .. } => line.clone(),
                Ctx::None | Ctx::Pending => None,
            };
            if let Some(context) = context {
                out.push_str(&format!("\n    {context}"));
            }
        }
        out
    }

    /// `-errorstack`: the failing site, then one element per container the error
    /// crossed, innermost first — a procedure call with the command that made it,
    /// or `UP 1` for a nested script.
    pub(crate) fn stack(&self, upto: usize) -> String {
        let mut items: Vec<String> = crate::list::split(&self.stack_prefix).unwrap_or_default();
        if items.is_empty() && !self.inner.is_empty() {
            items.push("INNER".to_string());
            items.push(self.inner.clone());
        }
        let upto = upto.min(self.entries.len());
        for (i, e) in self.entries[..upto].iter().enumerate() {
            let call = self.entries.get(i + 1).map(|n| n.text.clone());
            match &e.ctx {
                Ctx::Body(BodyKind::NsEval) => {
                    if let Some(call) = call {
                        items.extend(["CALL".to_string(), call]);
                    }
                }
                Ctx::Body(_) => items.extend(["UP".to_string(), "1".to_string()]),
                Ctx::Proc(_) | Ctx::Lambda(_) => {
                    if let Some(call) = call {
                        items.extend(["CALL".to_string(), call]);
                    }
                }
                Ctx::Done { stack, .. } => match (stack.as_str(), call) {
                    ("UP 1", _) => items.extend(["UP".to_string(), "1".to_string()]),
                    ("CALL", Some(call)) => items.extend(["CALL".to_string(), call]),
                    _ => {}
                },
                Ctx::None | Ctx::Pending => {}
            }
        }
        crate::list::join(&items)
    }
}

/// What a nested evaluation's outermost command was contained by, from the
/// command that ran it: the line tclsh adds and the `-errorstack` element.
fn unit_context(command: &str, line: usize) -> Ctx {
    let word = first_word(command);
    let (text, stack) = match word {
        "eval" => (Some(format!("(\"eval\" body line {line})")), "UP 1"),
        "uplevel" => (Some(format!("(\"uplevel\" body line {line})")), "UP 1"),
        "namespace" => {
            // `namespace eval NS script`: the namespace is the third word.
            let words = crate::list::split(command.trim_end()).unwrap_or_default();
            match (words.get(1).map(String::as_str), words.get(2)) {
                (Some("eval"), Some(ns)) => {
                    let ns = if ns.starts_with("::") {
                        ns.clone()
                    } else {
                        format!("::{ns}")
                    };
                    (
                        Some(format!("(in namespace eval \"{ns}\" script line {line})")),
                        "CALL",
                    )
                }
                _ => (None, ""),
            }
        }
        "source" => (None, ""),
        "subst" | "time" => (None, ""),
        _ => (None, ""),
    };
    match text {
        Some(line) => Ctx::Done {
            line: Some(line),
            stack: stack.to_string(),
        },
        None => Ctx::None,
    }
}

/// The entries of the VM's frame stack for an error raised by the op at
/// `failing`, innermost first. The last one belongs to the base frame and is
/// [`Ctx::Pending`].
pub(crate) fn chain(vm: &VM, chunk: &Chunk, failing: usize, started_inside: bool) -> Vec<Entry> {
    let Some(map) = lookup(chunk) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let proc_name = |entry: usize| -> Option<String> {
        let (name_idx, _) = chunk.sub_entries.iter().find(|(_, at)| *at == entry)?;
        chunk.names.get(*name_idx as usize).cloned()
    };
    // One container's chain: the failing command, then each command whose body
    // it sits in, ending with the outermost command of the container.
    let container = |ip: usize, depth: usize, ending: Ctx, out: &mut Vec<Entry>| {
        let Some(mut at) = map.innermost(ip) else {
            return;
        };
        let mut hidden = false;
        loop {
            let rec = &map.recs[at];
            let mut entry = Entry {
                text: map.text(rec).to_string(),
                line: rec.line,
                ctx: Ctx::None,
                depth,
                range: (rec.start, rec.end),
                kind: rec.kind,
                hidden,
            };
            match rec.outer {
                Some((parent, kind)) if parent < map.recs.len() => {
                    entry.ctx = Ctx::Body(kind);
                    out.push(entry);
                    at = parent;
                    // `try` is not logged; only what its clause was in.
                    hidden = matches!(
                        kind,
                        BodyKind::TryOn | BodyKind::TryTrap | BodyKind::TryFinally
                    );
                }
                _ => {
                    entry.ctx = ending;
                    out.push(entry);
                    return;
                }
            }
        }
    };
    let mut ip = failing;
    let mut depth = vm.frames.iter().filter(|f| f.entry_ip.is_some()).count();
    for (index, frame) in vm.frames.iter().enumerate().rev() {
        let Some(entry) = frame.entry_ip else {
            continue;
        };
        let ending = match proc_name(entry) {
            Some(name) if name.starts_with('\u{0}') => LAMBDAS
                .with(|l| l.borrow().last().cloned())
                .map_or(Ctx::None, Ctx::Lambda),
            Some(name) => Ctx::Proc(name),
            None => Ctx::None,
        };
        container(ip, depth, ending, &mut out);
        // The frame a machine was started inside of returns past the end of the
        // program; what called it is another machine's to log.
        if started_inside && index == 1 {
            return out;
        }
        ip = frame.return_ip.saturating_sub(1);
        depth -= 1;
    }
    container(ip, depth, Ctx::Pending, &mut out);
    // `apply` runs a lambda as a procedure it defines and calls by a name of its
    // own; that call is not a command of the script.
    out.retain(|e| !e.text.starts_with("\u{0}apply"));
    out
}

/// The `INNER` element for an error whose failing command is `text` and whose
/// message is `msg`.
pub(crate) fn inner_of(text: &str, msg: &str, errorcode: Option<&str>) -> String {
    match first_word(text) {
        "error" | "throw" => {
            let opts = match errorcode {
                Some(ec) if first_word(text) == "throw" => {
                    format!("-errorcode {}", crate::list::quote(ec, false))
                }
                _ => String::new(),
            };
            crate::list::join(&["returnImm".to_string(), msg.to_string(), opts])
        }
        _ => {
            let words = crate::list::split(text.trim_end()).unwrap_or_default();
            let mut all = vec!["invokeStk1".to_string()];
            all.extend(words);
            crate::list::join(&all)
        }
    }
}

// ── the compiler's half ──────────────────────────────────────────────────

/// What the compiler accumulates for one chunk.
#[derive(Debug, Default)]
pub(crate) struct Builder {
    pub recs: Vec<CmdRec>,
    pub sources: Vec<Arc<str>>,
    /// The commands being lowered, innermost last.
    open: Vec<usize>,
    /// The containers whose body is being lowered, innermost last.
    bodies: Vec<(usize, BodyKind)>,
    /// The text of the script being lowered, innermost last.
    texts: Vec<(Option<usize>, usize)>,
    /// Which clause of the `try` being lowered the next body is.
    pub pending_try: Option<BodyKind>,
}

impl Builder {
    /// A script is about to be lowered.
    pub(crate) fn enter_script(&mut self, text: Option<&Arc<str>>, base: Option<usize>) {
        let at = text.map(
            |t| match self.sources.iter().position(|s| Arc::ptr_eq(s, t)) {
                Some(i) => i,
                None => {
                    self.sources.push(Arc::clone(t));
                    self.sources.len() - 1
                }
            },
        );
        let base = base.unwrap_or_else(|| self.texts.last().map_or(0, |t| t.1));
        self.texts.push((at, base));
    }

    pub(crate) fn leave_script(&mut self) {
        self.texts.pop();
    }

    /// A command is about to be lowered, at op `start`.
    pub(crate) fn begin(
        &mut self,
        start: usize,
        line: usize,
        span: (usize, usize),
        kind: Option<BodyKind>,
    ) {
        let Some((Some(src), base)) = self.texts.last().copied() else {
            self.open.push(usize::MAX);
            return;
        };
        self.recs.push(CmdRec {
            start,
            end: usize::MAX,
            line: base + line,
            src,
            span,
            kind,
            outer: self.bodies.last().copied(),
        });
        self.open.push(self.recs.len() - 1);
    }

    /// The command is lowered; its ops end before `end`.
    pub(crate) fn finish(&mut self, end: usize) {
        if let Some(at) = self.open.pop() {
            if let Some(rec) = self.recs.get_mut(at) {
                rec.end = end;
            }
        }
    }

    /// The line of the enclosing container a body whose word began on `word_line`
    /// of the script being lowered starts at: the container's own first line when
    /// the command owning the body makes one of it.
    pub(crate) fn body_base(&self, word_line: usize) -> usize {
        let opens = self
            .open
            .last()
            .and_then(|&at| self.recs.get(at))
            .is_some_and(|r| r.kind.is_some_and(|k| k != BodyKind::Try));
        if opens {
            0
        } else {
            self.texts.last().map_or(0, |t| t.1) + word_line.saturating_sub(1)
        }
    }

    /// A body of the command being lowered is about to be. Answers whether it
    /// is a container of its own, which [`Builder::leave_body`] must be told.
    ///
    /// Inside a procedure body tclsh compiles these commands, and a compiled
    /// body is not logged as a container; `in_proc` says so.
    pub(crate) fn enter_body(&mut self, in_proc: bool) -> bool {
        let Some(&at) = self.open.last() else {
            return false;
        };
        match self.recs.get(at).and_then(|r| r.kind) {
            // A procedure body is a container of its own: what encloses the
            // `proc` command is not what encloses its commands.
            Some(BodyKind::Proc) => {
                self.bodies.push((usize::MAX, BodyKind::Proc));
                true
            }
            None => false,
            Some(BodyKind::NsEval) => {
                self.bodies.push((at, BodyKind::NsEval));
                true
            }
            Some(BodyKind::Try) => match self.pending_try.take() {
                Some(kind) if !in_proc => {
                    self.bodies.push((at, kind));
                    true
                }
                _ => false,
            },
            Some(_) if in_proc => false,
            Some(kind) => {
                self.bodies.push((at, kind));
                true
            }
        }
    }

    pub(crate) fn leave_body(&mut self, pushed: bool) {
        if pushed {
            self.bodies.pop();
        }
    }

    pub(crate) fn finish_map(self) -> CmdMap {
        let mut recs = self.recs;
        // A record whose command never finished (the lowering failed and the
        // pass is being discarded) has no range.
        for r in &mut recs {
            if r.end == usize::MAX {
                r.end = r.start;
            }
        }
        CmdMap {
            recs,
            sources: self.sources,
        }
    }
}

/// The ops of the command containing `ip` — what a `catch` region's handler is
/// part of, and so the extent of the region.
pub(crate) fn region_of(chunk: &Chunk, ip: usize) -> (usize, usize) {
    let Some(map) = lookup(chunk) else {
        return (0, 0);
    };
    map.innermost(ip)
        .map(|at| (map.recs[at].start, map.recs[at].end))
        .unwrap_or((0, 0))
}

/// Whether an error with this message was begun by an expression or a value
/// conversion rather than by a command's own failure, which makes tclsh's first
/// log entry "invoked from within" and not "while executing".
pub(crate) fn begun_by_expression(msg: &str, command: &str) -> bool {
    msg == "divide by zero"
        || msg.starts_with("cannot use ")
        || parsed_expression(msg).is_some()
        || (command == "format" && msg.starts_with("expected integer but got"))
}

/// The expression an `expr` parse failure names: the message's second line says
/// `in expression "…"`, with the offset marked `_`, which is not part of the
/// text.
pub(crate) fn parsed_expression(msg: &str) -> Option<String> {
    let (_, rest) = msg.split_once("\nin expression \"")?;
    // `invalid bareword` follows the expression with `";` and a hint; the
    // others end with it.
    let end = rest.find("\";\n").or_else(|| rest.rfind('"'))?;
    Some(rest[..end].replace("_@_", ""))
}
