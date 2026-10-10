//! A backtracking matcher for the parts of Tcl's Advanced Regular Expressions
//! that the `regex` crate cannot be trusted with.
//!
//! [`crate::are`] reads an ARE the way `regcomp` does and writes a `regex`
//! pattern. Two things that pattern cannot carry are matched here instead:
//!
//! * **Constructs `regex` has no syntax for** — back-references `\N` and
//!   look-ahead `(?=…)` / `(?!…)`.
//! * **Constructs whose *meaning* differs.** `regex` is leftmost-first, Perl's
//!   rule: the first alternative that lets the whole pattern match wins. An ARE
//!   is leftmost-*longest* unless its preference says otherwise — `a|ab` matches
//!   `ab` in `ab`, and `a*(ab)?` matches `aab` in `aab`. Every pattern whose
//!   first-match path and longest path can differ (an alternation, a
//!   non-greedy quantifier, a quantified group) is therefore routed here too.
//!   See [`needs_engine`]; a pattern of literals, classes and greedy quantifiers
//!   of single characters stays on the linear-time path.
//!
//! The input is the translated pattern string, which [`crate::are`] has already
//! validated: it is a small closed grammar (`(`, `(?:`, `(?=`, `(?!`, `|`, the
//! quantifiers, `[…]` with `\x{…}` items, `\A`, `\z`, `\b…`, `\k{N}`), so this
//! module parses that, not ARE, and reports no syntax errors of its own.
//!
//! ## Which match is chosen
//!
//! Henry Spencer's engine works in two steps and so does this module.
//!
//! 1. **The overall match.** The match starting earliest wins, and among those
//!    from that start the whole expression's *preference* decides the length:
//!    two or more branches prefer the longest, a quantified atom prefers the
//!    longest when greedy and the shortest when not, `{m}` takes its atom's, and
//!    a branch takes the first preference among its atoms. [`Engine::exec`]
//!    finds that span by following every path.
//! 2. **The dissection** (`cdissect` in `regexec.c`). Given the span, the
//!    sub-matches are assigned top-down: in a concatenation each element's end
//!    is the longest (the shortest, for an element that prefers it) that lets
//!    the rest still match exactly; an alternation takes the first branch that
//!    matches the span exactly; an iteration of a group splits into the
//!    iterations of a star followed by its mandatory copies — which is why
//!    `(a+)+` over `aaa` captures the last `a`, not the whole run.
//!    [`Dissect`] is that procedure.
//!
//! Steps that need a yes/no answer — can this part match exactly `[i, j)` —
//! ask [`Engine::ends`], which follows every path of one sub-program from one
//! position.
//!
//! ## Cost
//!
//! A pattern without a back-reference visits each (instruction, position) pair
//! once per sub-program run, because the pair's future does not depend on how
//! it was reached. A back-reference makes the future depend on the captures, so
//! there the search is exhaustive under a step budget, and a pattern that
//! exhausts it is reported as too complex rather than left to run.

use std::collections::{HashMap, HashSet};

use crate::cmd_string::lower;

/// The largest compiled program, in instructions: `regcomp`'s own limit is on
/// NFA states and reports `REG_ETOOBIG`.
const MAX_PROGRAM: usize = 100_000;
/// Steps one search may take.
const BUDGET: u64 = 20_000_000;
/// Positions up to which the visited set is a bit table; past it, a hash set,
/// so a `-all` loop over a long subject does not clear a table per match.
const DENSE_BITS: usize = 1 << 20;
/// The wording of `REG_ETOOBIG`.
const TOO_COMPLEX: &str = "regular expression is too complex";

/// The length a (sub)expression prefers its match to have.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pref {
    Longest,
    Shortest,
    None,
}

/// A set of characters: ranges, optionally negated.
#[derive(Debug)]
struct CharSet {
    ranges: Vec<(u32, u32)>,
    negated: bool,
}

impl CharSet {
    fn new(mut ranges: Vec<(u32, u32)>, negated: bool) -> CharSet {
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (a, b) in ranges {
            match merged.last_mut() {
                Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        CharSet {
            ranges: merged,
            negated,
        }
    }

    fn contains(&self, c: u32) -> bool {
        let at = self.ranges.partition_point(|&(_, hi)| hi < c);
        let inside = self.ranges.get(at).is_some_and(|&(lo, _)| lo <= c);
        inside != self.negated
    }
}

/// Zero-width conditions.
#[derive(Clone, Copy, Debug)]
enum Assert {
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
    WordBoundary,
    NotWordBoundary,
    WordStart,
    WordEnd,
}

/// One node of the parsed pattern; children are indices into [`Tree::nodes`].
#[derive(Debug)]
enum Node {
    Empty,
    Char(u32),
    Any,
    Set(CharSet),
    Assert(Assert),
    Backref(usize),
    /// A capturing group when numbered.
    Group(Option<usize>, usize),
    Look {
        negated: bool,
        node: usize,
    },
    Concat(Vec<usize>),
    Alt(Vec<usize>),
    Repeat {
        node: usize,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        /// Written `{m}` or `{m}?`, which take their atom's preference.
        bare: bool,
    },
}

#[derive(Default)]
struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    fn add(&mut self, n: Node) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }
}

// ── reading the translated pattern ───────────────────────────────────────

struct Parser {
    s: Vec<char>,
    i: usize,
    groups: usize,
    tree: Tree,
}

impl Parser {
    fn new(pattern: &str) -> Parser {
        Parser {
            s: pattern.chars().collect(),
            i: 0,
            groups: 0,
            tree: Tree::default(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, lit: &str) -> bool {
        let want: Vec<char> = lit.chars().collect();
        if self.s[self.i..].starts_with(&want) {
            self.i += want.len();
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> usize {
        let mut branches = vec![self.concat()];
        while self.eat('|') {
            branches.push(self.concat());
        }
        if branches.len() == 1 {
            branches[0]
        } else {
            self.tree.add(Node::Alt(branches))
        }
    }

    fn concat(&mut self) -> usize {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom();
            items.push(self.quantified(atom));
        }
        match items.len() {
            0 => self.tree.add(Node::Empty),
            1 => items[0],
            _ => self.tree.add(Node::Concat(items)),
        }
    }

    fn number(&mut self) -> u32 {
        let mut n = 0u32;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            n = n.saturating_mul(10).saturating_add(d);
            self.i += 1;
        }
        n
    }

    fn quantified(&mut self, atom: usize) -> usize {
        let mut bare = false;
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, None)
            }
            Some('+') => {
                self.i += 1;
                (1, None)
            }
            Some('?') => {
                self.i += 1;
                (0, Some(1))
            }
            Some('{') => {
                self.i += 1;
                let m = self.number();
                let max = if self.eat(',') {
                    if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        Some(self.number())
                    } else {
                        None
                    }
                } else {
                    bare = true;
                    Some(m)
                };
                self.eat('}');
                (m, max)
            }
            _ => return atom,
        };
        let greedy = !self.eat('?');
        self.tree.add(Node::Repeat {
            node: atom,
            min,
            max,
            greedy,
            bare,
        })
    }

    /// One `\x{H…}` code point, or the character after a backslash.
    fn escaped(&mut self) -> u32 {
        match self.peek() {
            Some('x') if self.s.get(self.i + 1) == Some(&'{') => {
                self.i += 2;
                let mut n = 0u32;
                while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
                    n = n.wrapping_mul(16).wrapping_add(d);
                    self.i += 1;
                }
                self.eat('}');
                n
            }
            Some('n') => {
                self.i += 1;
                10
            }
            Some(c) => {
                self.i += 1;
                c as u32
            }
            None => '\\' as u32,
        }
    }

    fn class_item(&mut self) -> u32 {
        match self.peek() {
            Some('\\') => {
                self.i += 1;
                self.escaped()
            }
            Some(c) => {
                self.i += 1;
                c as u32
            }
            None => 0,
        }
    }

    fn class(&mut self) -> usize {
        let negated = self.eat('^');
        let mut ranges = Vec::new();
        while self.peek().is_some_and(|c| c != ']') {
            let a = self.class_item();
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|&c| c != ']') {
                self.i += 1;
                let b = self.class_item();
                ranges.push((a, b));
            } else {
                ranges.push((a, a));
            }
        }
        self.eat(']');
        self.tree.add(Node::Set(CharSet::new(ranges, negated)))
    }

    fn atom(&mut self) -> usize {
        let Some(c) = self.peek() else {
            return self.tree.add(Node::Empty);
        };
        self.i += 1;
        let node = match c {
            '(' => {
                let node = if self.eat_str("?:") {
                    let inner = self.alt();
                    Node::Group(None, inner)
                } else if self.eat_str("?=") || self.eat_str("?!") {
                    let negated = self.s[self.i - 1] == '!';
                    let inner = self.alt();
                    Node::Look {
                        negated,
                        node: inner,
                    }
                } else {
                    self.groups += 1;
                    let n = self.groups;
                    let inner = self.alt();
                    Node::Group(Some(n), inner)
                };
                self.eat(')');
                node
            }
            '[' => return self.class(),
            '.' => Node::Any,
            '^' => Node::Assert(Assert::LineStart),
            '$' => Node::Assert(Assert::LineEnd),
            '\\' => match self.peek() {
                Some('A') => {
                    self.i += 1;
                    Node::Assert(Assert::TextStart)
                }
                Some('z') => {
                    self.i += 1;
                    Node::Assert(Assert::TextEnd)
                }
                Some('B') => {
                    self.i += 1;
                    Node::Assert(Assert::NotWordBoundary)
                }
                Some('b') => {
                    self.i += 1;
                    if self.eat_str("{start}") {
                        Node::Assert(Assert::WordStart)
                    } else if self.eat_str("{end}") {
                        Node::Assert(Assert::WordEnd)
                    } else {
                        Node::Assert(Assert::WordBoundary)
                    }
                }
                Some('k') => {
                    self.i += 2;
                    let n = self.number() as usize;
                    self.eat('}');
                    Node::Backref(n)
                }
                _ => Node::Char(self.escaped()),
            },
            other => Node::Char(other as u32),
        };
        self.tree.add(node)
    }
}

/// Split a translated pattern into its `(?m)` / `(?s)` prefix and the rest.
fn split_flags(pattern: &str) -> (bool, bool, &str) {
    if let Some(rest) = pattern.strip_prefix("(?") {
        if let Some((flags, tail)) = rest.split_once(')') {
            if !flags.is_empty() && flags.chars().all(|c| c == 'm' || c == 's') {
                return (flags.contains('m'), flags.contains('s'), tail);
            }
        }
    }
    (false, false, pattern)
}

// ── analysis ─────────────────────────────────────────────────────────────

impl Tree {
    fn single_char(&self, n: usize) -> bool {
        matches!(self.nodes[n], Node::Char(_) | Node::Any | Node::Set(_))
    }

    fn children(&self, n: usize) -> Vec<usize> {
        match &self.nodes[n] {
            Node::Group(_, x) | Node::Look { node: x, .. } | Node::Repeat { node: x, .. } => {
                vec![*x]
            }
            Node::Concat(v) | Node::Alt(v) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// Whether the leftmost-first match the `regex` crate would find can differ
    /// from the match an ARE prescribes.
    fn risky(&self, n: usize) -> bool {
        match &self.nodes[n] {
            Node::Alt(_) | Node::Backref(_) | Node::Look { .. } => true,
            Node::Repeat {
                node,
                min,
                max,
                greedy,
                ..
            } => {
                if *max == Some(*min) {
                    self.risky(*node)
                } else {
                    !greedy || !self.single_char(*node)
                }
            }
            _ => self.children(n).into_iter().any(|c| self.risky(c)),
        }
    }

    fn any(&self, n: usize, f: &dyn Fn(&Node) -> bool) -> bool {
        f(&self.nodes[n]) || self.children(n).into_iter().any(|c| self.any(c, f))
    }

    /// [`Tree::any`] over the part of the pattern that can still match: an atom
    /// quantified `{0}` is dropped by `regcomp` before anything reads it, so what
    /// is inside it is no capture and no back-reference.
    fn any_live(&self, n: usize, f: &dyn Fn(&Node) -> bool) -> bool {
        if matches!(self.nodes[n], Node::Repeat { max: Some(0), .. }) {
            return false;
        }
        f(&self.nodes[n]) || self.children(n).into_iter().any(|c| self.any_live(c, f))
    }

    /// Whether the node contains nothing a sub-match could be observed
    /// through: no capturing group and no back-reference.
    fn plain(&self, n: usize) -> bool {
        !self.any_live(n, &|x| {
            matches!(x, Node::Group(Some(_), _) | Node::Backref(_))
        })
    }

    fn pref(&self, n: usize) -> Pref {
        match &self.nodes[n] {
            Node::Concat(v) => v
                .iter()
                .map(|c| self.pref(*c))
                .find(|p| *p != Pref::None)
                .unwrap_or(Pref::None),
            Node::Alt(_) => Pref::Longest,
            Node::Group(_, x) => self.pref(*x),
            // Dropped before anything reads its preference.
            Node::Repeat { max: Some(0), .. } => Pref::None,
            Node::Repeat {
                node, greedy, bare, ..
            } => {
                // `{m}` and `{m}?` take their atom's preference; `{m,m}` does
                // not, and neither does any other quantifier.
                if *bare {
                    self.pref(*node)
                } else if *greedy {
                    Pref::Longest
                } else {
                    Pref::Shortest
                }
            }
            _ => Pref::None,
        }
    }

    fn nullable(&self, n: usize) -> bool {
        match &self.nodes[n] {
            Node::Char(_) | Node::Any | Node::Set(_) => false,
            Node::Empty | Node::Assert(_) | Node::Backref(_) | Node::Look { .. } => true,
            Node::Group(_, x) => self.nullable(*x),
            Node::Concat(v) => v.iter().all(|c| self.nullable(*c)),
            Node::Alt(v) => v.iter().any(|c| self.nullable(*c)),
            Node::Repeat { node, min, .. } => *min == 0 || self.nullable(*node),
        }
    }
}

/// Whether `pattern` — a translated pattern — has to be matched by [`Engine`]
/// rather than by the `regex` crate.
pub(crate) fn needs_engine(pattern: &str) -> bool {
    let (_, _, body) = split_flags(pattern);
    let mut p = Parser::new(body);
    let root = p.alt();
    p.tree.risky(root)
        // A `\A` that matches at every restart and a `^` that does not: only
        // this engine tells the two apart.
        || (p.tree.any(root, &|n| matches!(n, Node::Assert(Assert::TextStart)))
            && p.tree.any(root, &|n| matches!(n, Node::Assert(Assert::LineStart))))
}

// ── the program ──────────────────────────────────────────────────────────

#[derive(Debug)]
enum Inst {
    Char(u32),
    Any,
    Set(usize),
    /// Try the first target, then the second.
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Assert(Assert),
    Backref(usize),
    /// Fail unless the group has been set: a quantified back-reference to a
    /// group that took no part fails even when it could repeat zero times.
    Defined(usize),
    Look {
        negated: bool,
        sub: usize,
    },
    /// Remember where a loop iteration began.
    Mark(usize),
    /// Refuse an iteration that consumed nothing: an ARE never counts one, so
    /// the captures inside it are not set by it.
    Check(usize),
    Match,
}

/// What a program is compiled from: a tree, and the state programs share.
struct Compiler<'a> {
    tree: &'a Tree,
    sets: Vec<CharSet>,
    subs: Vec<Vec<Inst>>,
    marks: usize,
    base: usize,
    size: usize,
}

impl Compiler<'_> {
    fn emit(&mut self, prog: &mut Vec<Inst>, id: usize) -> Result<(), String> {
        self.size += 1;
        if self.size > MAX_PROGRAM {
            return Err(TOO_COMPLEX.to_string());
        }
        let tree = self.tree;
        match &tree.nodes[id] {
            Node::Empty => {}
            Node::Char(c) => prog.push(Inst::Char(*c)),
            Node::Any => prog.push(Inst::Any),
            Node::Set(s) => {
                self.sets.push(CharSet {
                    ranges: s.ranges.clone(),
                    negated: s.negated,
                });
                prog.push(Inst::Set(self.sets.len() - 1));
            }
            Node::Assert(a) => prog.push(Inst::Assert(*a)),
            Node::Backref(n) => prog.push(Inst::Backref(*n)),
            Node::Group(g, x) => {
                if let Some(g) = g {
                    prog.push(Inst::Save(2 * g));
                    self.emit(prog, *x)?;
                    prog.push(Inst::Save(2 * g + 1));
                } else {
                    self.emit(prog, *x)?;
                }
            }
            Node::Look { negated, node } => {
                let mut sub = Vec::new();
                self.emit(&mut sub, *node)?;
                sub.push(Inst::Match);
                self.subs.push(sub);
                prog.push(Inst::Look {
                    negated: *negated,
                    sub: self.subs.len() - 1,
                });
            }
            Node::Concat(v) => {
                for x in v {
                    self.emit(prog, *x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                let last = v.len() - 1;
                for (k, x) in v.iter().enumerate() {
                    if k == last {
                        self.emit(prog, *x)?;
                        break;
                    }
                    let split = prog.len();
                    prog.push(Inst::Split(0, 0));
                    self.emit(prog, *x)?;
                    jumps.push(prog.len());
                    prog.push(Inst::Jmp(0));
                    prog[split] = Inst::Split(split + 1, prog.len());
                }
                let end = prog.len();
                for j in jumps {
                    prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
                ..
            } => {
                if let (Node::Backref(g), true) = (&tree.nodes[*node], *max != Some(0)) {
                    prog.push(Inst::Defined(*g));
                }
                self.repeat(prog, *node, *min, *max, *greedy)?
            }
        }
        Ok(())
    }

    fn repeat(
        &mut self,
        prog: &mut Vec<Inst>,
        x: usize,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    ) -> Result<(), String> {
        let copies = max.unwrap_or(min).max(min) as usize + 1;
        if copies > 1 && self.size.saturating_mul(copies) > MAX_PROGRAM * 8 {
            return Err(TOO_COMPLEX.to_string());
        }
        for _ in 0..min {
            self.emit(prog, x)?;
        }
        match max {
            None => {
                let top = prog.len();
                prog.push(Inst::Split(0, 0));
                if self.tree.nullable(x) {
                    let slot = self.base + self.marks;
                    self.marks += 1;
                    prog.push(Inst::Mark(slot));
                    self.emit(prog, x)?;
                    prog.push(Inst::Check(slot));
                    prog.push(Inst::Jmp(top));
                } else {
                    self.emit(prog, x)?;
                    prog.push(Inst::Jmp(top));
                }
                let end = prog.len();
                prog[top] = if greedy {
                    Inst::Split(top + 1, end)
                } else {
                    Inst::Split(end, top + 1)
                };
            }
            Some(m) => {
                let mut splits = Vec::new();
                for _ in min..m {
                    splits.push(prog.len());
                    prog.push(Inst::Split(0, 0));
                    self.emit(prog, x)?;
                }
                let end = prog.len();
                for at in splits {
                    prog[at] = if greedy {
                        Inst::Split(at + 1, end)
                    } else {
                        Inst::Split(end, at + 1)
                    };
                }
            }
        }
        Ok(())
    }

    /// A program of its own for a run of sibling nodes, ending in `Match`.
    fn program(&mut self, ids: &[usize]) -> Result<Vec<Inst>, String> {
        let mut prog = Vec::new();
        for id in ids {
            self.emit(&mut prog, *id)?;
        }
        prog.push(Inst::Match);
        Ok(prog)
    }
}

// ── the dissection plan ──────────────────────────────────────────────────

/// How a sub-match is assigned once the span it covers is known.
enum Kind {
    /// Matched as a unit; nothing inside it is observable.
    Leaf,
    /// A capturing group.
    Capture(usize, Box<Plan>),
    /// Elements in order, each with whether it prefers its shortest match.
    Seq(Vec<(Plan, bool)>),
    /// Branches, tried in order.
    Choice(Vec<Plan>),
    /// An iteration of something observable: `min..=max` copies.
    Iter {
        x: Box<Plan>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        x_shorter: bool,
        /// The atom contains a back-reference, which makes the iteration a node
        /// of its own whatever its minimum.
        backref: bool,
    },
}

struct Plan {
    /// Index into [`Engine::progs`]: the program that matches this part alone.
    prog: usize,
    kind: Kind,
}

/// Build the dissection plan of one node, compiling the programs it asks
/// about as it goes.
fn plan(
    tree: &Tree,
    c: &mut Compiler,
    progs: &mut Vec<Vec<Inst>>,
    id: usize,
) -> Result<Plan, String> {
    fn compile(
        c: &mut Compiler,
        progs: &mut Vec<Vec<Inst>>,
        ids: &[usize],
    ) -> Result<usize, String> {
        progs.push(c.program(ids)?);
        Ok(progs.len() - 1)
    }
    if tree.plain(id) {
        return Ok(Plan {
            prog: compile(c, progs, &[id])?,
            kind: Kind::Leaf,
        });
    }
    match &tree.nodes[id] {
        Node::Group(None, x) => plan(tree, c, progs, *x),
        Node::Group(Some(n), x) => {
            let inner = plan(tree, c, progs, *x)?;
            Ok(Plan {
                prog: compile(c, progs, &[id])?,
                kind: Kind::Capture(*n, Box::new(inner)),
            })
        }
        Node::Concat(v) => {
            // Adjacent plain elements are one element: nothing between them
            // is observable, so nothing there is a decision.
            let mut elems: Vec<(Plan, bool)> = Vec::new();
            let mut run: Vec<usize> = Vec::new();
            for x in v {
                if tree.plain(*x) {
                    run.push(*x);
                    continue;
                }
                flush_run(tree, c, progs, &mut run, &mut elems)?;
                let p = plan(tree, c, progs, *x)?;
                elems.push((p, tree.pref(*x) == Pref::Shortest));
            }
            flush_run(tree, c, progs, &mut run, &mut elems)?;
            Ok(Plan {
                prog: compile(c, progs, &[id])?,
                kind: Kind::Seq(elems),
            })
        }
        Node::Alt(v) => {
            let mut branches = Vec::new();
            for x in v {
                branches.push(plan(tree, c, progs, *x)?);
            }
            Ok(Plan {
                prog: compile(c, progs, &[id])?,
                kind: Kind::Choice(branches),
            })
        }
        // A quantified back-reference is one node, `cbrdissect`'s, which counts
        // the repetitions itself.
        Node::Repeat { node, .. } if matches!(tree.nodes[*node], Node::Backref(_)) => Ok(Plan {
            prog: compile(c, progs, &[id])?,
            kind: Kind::Leaf,
        }),
        Node::Repeat {
            node,
            min,
            max,
            greedy,
            ..
        } => {
            let x = plan(tree, c, progs, *node)?;
            Ok(Plan {
                prog: compile(c, progs, &[id])?,
                kind: Kind::Iter {
                    x: Box::new(x),
                    min: *min,
                    max: *max,
                    greedy: *greedy,
                    x_shorter: tree.pref(*node) == Pref::Shortest,
                    backref: tree.any_live(*node, &|n| matches!(n, Node::Backref(_))),
                },
            })
        }
        // A back-reference: observable only through what it must equal, which
        // is matched as a unit.
        _ => Ok(Plan {
            prog: compile(c, progs, &[id])?,
            kind: Kind::Leaf,
        }),
    }
}

/// Close a run of adjacent plain elements into one element of a sequence.
fn flush_run(
    tree: &Tree,
    c: &mut Compiler,
    progs: &mut Vec<Vec<Inst>>,
    run: &mut Vec<usize>,
    elems: &mut Vec<(Plan, bool)>,
) -> Result<(), String> {
    if run.is_empty() {
        return Ok(());
    }
    let shorter =
        run.iter().map(|x| tree.pref(*x)).find(|p| *p != Pref::None) == Some(Pref::Shortest);
    progs.push(c.program(run)?);
    elems.push((
        Plan {
            prog: progs.len() - 1,
            kind: Kind::Leaf,
        },
        shorter,
    ));
    run.clear();
    Ok(())
}

// ── running ──────────────────────────────────────────────────────────────

const NONE: usize = usize::MAX;

enum Frame {
    Run(usize, usize),
    Restore(usize, usize),
}

/// The (instruction, position) pairs a run has already followed.
enum Visited {
    Dense { bits: Vec<u64>, width: usize },
    Sparse { seen: HashSet<u64>, width: usize },
}

impl Visited {
    fn new(insts: usize, width: usize) -> Visited {
        if insts.saturating_mul(width) <= DENSE_BITS {
            Visited::Dense {
                bits: vec![0; (insts * width).div_ceil(64)],
                width,
            }
        } else {
            Visited::Sparse {
                seen: HashSet::new(),
                width,
            }
        }
    }

    /// Mark a pair; false when it was marked already.
    fn insert(&mut self, pc: usize, pos: usize) -> bool {
        match self {
            Visited::Dense { bits, width } => {
                let k = pc * *width + pos;
                let (w, b) = (k / 64, 1u64 << (k % 64));
                let fresh = bits[w] & b == 0;
                bits[w] |= b;
                fresh
            }
            Visited::Sparse { seen, width } => seen.insert((pc * *width + pos) as u64),
        }
    }
}

struct Overflow;

/// What a search sees of the subject, and how much work it may still do.
struct Search<'t> {
    text: &'t [char],
    /// `REG_NOTBOL`: the first character is not at the start of a line.
    notbol: bool,
    /// Steps left before the search is abandoned.
    budget: u64,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A compiled pattern.
pub(crate) struct Engine {
    /// The whole pattern, wrapped in group 0's saves.
    root: Vec<Inst>,
    /// The programs [`Plan::prog`] indexes.
    progs: Vec<Vec<Inst>>,
    subs: Vec<Vec<Inst>>,
    sets: Vec<CharSet>,
    plan: Plan,
    groups: usize,
    slots: usize,
    overall: Pref,
    multiline: bool,
    dotall: bool,
    icase: bool,
    backref: bool,
}

/// One match: `(start, end)` per group, group 0 first, in characters.
pub(crate) type Spans = Vec<Option<(usize, usize)>>;

impl Engine {
    /// Compile a translated pattern. `icase` is `REG_ICASE`, which only a
    /// back-reference needs: [`crate::are`] has folded it into every literal.
    pub(crate) fn new(pattern: &str, icase: bool) -> Result<Engine, String> {
        let (multiline, dotall, body) = split_flags(pattern);
        let mut p = Parser::new(body);
        let top = p.alt();
        let groups = p.groups;
        let tree = p.tree;
        let mut c = Compiler {
            tree: &tree,
            sets: Vec::new(),
            subs: Vec::new(),
            marks: 0,
            base: 2 * (groups + 1),
            size: 0,
        };
        let mut root = vec![Inst::Save(0)];
        c.emit(&mut root, top)?;
        root.push(Inst::Save(1));
        root.push(Inst::Match);
        let mut progs = Vec::new();
        let plan = if groups == 0 {
            Plan {
                prog: 0,
                kind: Kind::Leaf,
            }
        } else {
            plan(&tree, &mut c, &mut progs, top)?
        };
        Ok(Engine {
            root,
            progs,
            subs: c.subs,
            sets: c.sets,
            plan,
            groups,
            slots: c.base + c.marks,
            overall: tree.pref(top),
            multiline,
            dotall,
            icase,
            backref: tree.any_live(top, &|n| matches!(n, Node::Backref(_))),
        })
    }

    fn assertion(&self, a: Assert, s: &Search, pos: usize) -> bool {
        let n = s.text.len();
        let before = pos > 0 && is_word(s.text[pos - 1]);
        let after = pos < n && is_word(s.text[pos]);
        match a {
            Assert::LineStart => {
                if pos == 0 {
                    !s.notbol
                } else {
                    self.multiline && s.text[pos - 1] == '\n'
                }
            }
            Assert::LineEnd => pos == n || (self.multiline && s.text[pos] == '\n'),
            Assert::TextStart => pos == 0,
            Assert::TextEnd => pos == n,
            Assert::WordBoundary => before != after,
            Assert::NotWordBoundary => before == after,
            Assert::WordStart => !before && after,
            Assert::WordEnd => before && !after,
        }
    }

    /// Follow every path of `insts` from `start` in priority order, calling
    /// `on_match` with the end position and the slots at each `Match`; it
    /// answers whether to stop.
    fn explore(
        &self,
        insts: &[Inst],
        s: &mut Search,
        start: usize,
        caps: &mut [usize],
        mut visited: Option<&mut Visited>,
        on_match: &mut dyn FnMut(usize, &[usize]) -> bool,
    ) -> Result<(), Overflow> {
        let text = s.text;
        let n = text.len();
        let mut stack = vec![Frame::Run(0, start)];
        while let Some(frame) = stack.pop() {
            let (mut pc, mut pos) = match frame {
                Frame::Restore(slot, v) => {
                    caps[slot] = v;
                    continue;
                }
                Frame::Run(pc, pos) => (pc, pos),
            };
            loop {
                if s.budget == 0 {
                    return Err(Overflow);
                }
                s.budget -= 1;
                if let Some(v) = visited.as_deref_mut() {
                    if !v.insert(pc, pos) {
                        break;
                    }
                }
                match &insts[pc] {
                    Inst::Char(c) => {
                        if pos < n && text[pos] as u32 == *c {
                            pos += 1;
                            pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Any => {
                        if pos < n && (self.dotall || text[pos] != '\n') {
                            pos += 1;
                            pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Set(i) => {
                        if pos < n && self.sets[*i].contains(text[pos] as u32) {
                            pos += 1;
                            pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Split(a, b) => {
                        stack.push(Frame::Run(*b, pos));
                        pc = *a;
                    }
                    Inst::Jmp(t) => pc = *t,
                    Inst::Save(slot) | Inst::Mark(slot) => {
                        stack.push(Frame::Restore(*slot, caps[*slot]));
                        caps[*slot] = pos;
                        pc += 1;
                    }
                    Inst::Check(slot) => {
                        if caps[*slot] == pos {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Assert(a) => {
                        if self.assertion(*a, s, pos) {
                            pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Defined(g) => {
                        if caps[2 * g] == NONE || caps[2 * g + 1] == NONE {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Backref(g) => {
                        let (a, b) = (caps[2 * g], caps[2 * g + 1]);
                        if a == NONE || b == NONE || pos + (b - a) > n {
                            break;
                        }
                        let len = b - a;
                        let same = (0..len).all(|k| {
                            let (x, y) = (text[a + k], text[pos + k]);
                            x == y || (self.icase && lower(x) == lower(y))
                        });
                        if !same {
                            break;
                        }
                        pos += len;
                        pc += 1;
                    }
                    Inst::Look { negated, sub } => {
                        let mut inner = caps.to_vec();
                        let mut found = false;
                        self.explore(&self.subs[*sub], s, pos, &mut inner, None, &mut |_, _| {
                            found = true;
                            true
                        })?;
                        if found == *negated {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Match => {
                        if on_match(pos, caps) {
                            return Ok(());
                        }
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Every position at which `prog` can end when it starts at `from`, given
    /// the captures so far (a back-reference reads them).
    fn ends(
        &self,
        prog: &[Inst],
        s: &mut Search,
        from: usize,
        caps: &[usize],
    ) -> Result<Vec<usize>, Overflow> {
        let n = s.text.len();
        let mut seen = vec![false; n + 1];
        let mut scratch = caps.to_vec();
        let mut visited = (!self.backref).then(|| Visited::new(prog.len(), n + 1));
        self.explore(
            prog,
            s,
            from,
            &mut scratch,
            visited.as_mut(),
            &mut |end, _| {
                seen[end] = true;
                false
            },
        )?;
        Ok(seen
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| b.then_some(i))
            .collect())
    }

    /// The match of `text` an ARE prescribes: leftmost, then by preference.
    pub(crate) fn exec(&self, text: &[char], notbol: bool) -> Result<Option<Spans>, String> {
        let mut s = Search {
            text,
            notbol,
            budget: BUDGET,
        };
        let n = text.len();
        let mut visited = (!self.backref).then(|| Visited::new(self.root.len(), n + 1));
        for start in 0..=n {
            let mut caps = vec![NONE; self.slots];
            let mut reached = vec![false; n + 1];
            let overall = self.overall;
            let mut note = |end: usize, _: &[usize]| {
                reached[end] = true;
                overall == Pref::None
            };
            self.explore(
                &self.root,
                &mut s,
                start,
                &mut caps,
                visited.as_mut(),
                &mut note,
            )
            .map_err(|_| TOO_COMPLEX.to_string())?;
            // The ends the pattern can have from here, in the order its
            // preference tries them. `cfindloop` dissects each in turn and
            // takes the first that dissects, which with a back-reference is not
            // always the first that matches: a dissection never reconsiders the
            // choice it made inside a part once the rest has failed.
            let mut ends: Vec<usize> = (0..=n).filter(|e| reached[*e]).collect();
            if overall != Pref::Shortest {
                ends.reverse();
            }
            if overall == Pref::None {
                ends.truncate(1);
            }
            let mut d = Dissect {
                engine: self,
                s: &mut s,
                cache: HashMap::new(),
            };
            for end in ends {
                let mut caps = vec![NONE; self.slots];
                caps[0] = start;
                caps[1] = end;
                if self.groups == 0
                    || d.go(&self.plan, start, end, &mut caps)
                        .map_err(|_| TOO_COMPLEX.to_string())?
                {
                    return Ok(Some(self.spans(&caps)));
                }
            }
        }
        Ok(None)
    }

    fn spans(&self, caps: &[usize]) -> Spans {
        (0..=self.groups)
            .map(|g| {
                let (a, b) = (caps[2 * g], caps[2 * g + 1]);
                (a != NONE && b != NONE).then_some((a, b))
            })
            .collect()
    }
}

/// Forget what the groups inside `plan` matched: an iteration starts from
/// nothing, so a group the new iteration does not reach is not left holding
/// the previous one's text (`zaptreesubs`).
fn zap(plan: &Plan, caps: &mut [usize]) {
    match &plan.kind {
        Kind::Leaf => {}
        Kind::Capture(n, inner) => {
            caps[2 * n] = NONE;
            caps[2 * n + 1] = NONE;
            zap(inner, caps);
        }
        Kind::Seq(v) => v.iter().for_each(|(p, _)| zap(p, caps)),
        Kind::Choice(v) => v.iter().for_each(|p| zap(p, caps)),
        Kind::Iter { x, .. } => zap(x, caps),
    }
}

/// `cdissect`: assign sub-matches inside a span the whole pattern matched.
struct Dissect<'e, 's, 't> {
    engine: &'e Engine,
    s: &'s mut Search<'t>,
    /// [`Engine::ends`] answers, which depend on the captures only when there
    /// is a back-reference.
    cache: HashMap<(usize, usize), Vec<usize>>,
}

impl Dissect<'_, '_, '_> {
    fn ends(&mut self, prog: usize, from: usize, caps: &[usize]) -> Result<Vec<usize>, Overflow> {
        let engine = self.engine;
        if engine.backref {
            return engine.ends(&engine.progs[prog], self.s, from, caps);
        }
        if let Some(hit) = self.cache.get(&(prog, from)) {
            return Ok(hit.clone());
        }
        let got = engine.ends(&engine.progs[prog], self.s, from, caps)?;
        self.cache.insert((prog, from), got.clone());
        Ok(got)
    }

    /// Candidate ends of a part, ordered by preference.
    fn order(mut v: Vec<usize>, shorter: bool) -> Vec<usize> {
        if !shorter {
            v.reverse();
        }
        v
    }

    /// Whether `plan` can match exactly `[i, j)`.
    fn exact(&mut self, plan: &Plan, i: usize, j: usize, caps: &[usize]) -> Result<bool, Overflow> {
        Ok(self.ends(plan.prog, i, caps)?.contains(&j))
    }

    fn go(
        &mut self,
        plan: &Plan,
        i: usize,
        j: usize,
        caps: &mut Vec<usize>,
    ) -> Result<bool, Overflow> {
        match &plan.kind {
            Kind::Leaf => Ok(true),
            Kind::Capture(n, inner) => {
                let held = (caps[2 * n], caps[2 * n + 1]);
                caps[2 * n] = i;
                caps[2 * n + 1] = j;
                if self.go(inner, i, j, caps)? {
                    return Ok(true);
                }
                (caps[2 * n], caps[2 * n + 1]) = held;
                Ok(false)
            }
            Kind::Seq(elems) => self.seq(elems, i, j, caps),
            Kind::Choice(branches) => {
                for b in branches {
                    if self.exact(b, i, j, caps)? {
                        let held = caps.clone();
                        if self.go(b, i, j, caps)? {
                            return Ok(true);
                        }
                        *caps = held;
                    }
                }
                Ok(false)
            }
            Kind::Iter {
                x,
                min,
                max,
                greedy,
                x_shorter,
                backref,
            } => {
                if *min == 0 || *backref {
                    // A real iteration node.
                    return self.citer(x, *min, *max, *x_shorter, i, j, caps);
                }
                // `x{m,n}` of an atom with no back-reference is `x{m-1,n-1}`
                // matched by the automaton and then one `x` the tree records,
                // which is what makes the *last* iteration the captured one.
                let optional = max.map(|n| n.saturating_sub(*min));
                let before = self.blob_ends(x, optional, *min - 1, i, caps)?;
                for q in Self::order(before, !*greedy) {
                    if q > j || !self.exact(x, q, j, caps)? {
                        continue;
                    }
                    let held = caps.clone();
                    zap(x, caps);
                    if self.go(x, q, j, caps)? {
                        return Ok(true);
                    }
                    *caps = held;
                }
                Ok(false)
            }
        }
    }

    fn seq(
        &mut self,
        elems: &[(Plan, bool)],
        i: usize,
        j: usize,
        caps: &mut Vec<usize>,
    ) -> Result<bool, Overflow> {
        let Some(((first, shorter), rest)) = elems.split_first() else {
            return Ok(i == j);
        };
        if rest.is_empty() {
            return Ok(self.exact(first, i, j, caps)? && self.go(first, i, j, caps)?);
        }
        let candidates = self.ends(first.prog, i, caps)?;
        for p in Self::order(candidates, *shorter) {
            if p > j {
                continue;
            }
            // Without a back-reference the rest can be asked about before
            // committing; with one it depends on what `first` captures.
            if !self.engine.backref && !self.rest_exact(rest, p, j, caps)? {
                continue;
            }
            let held = caps.clone();
            if self.go(first, i, p, caps)? && self.seq(rest, p, j, caps)? {
                return Ok(true);
            }
            *caps = held;
        }
        Ok(false)
    }

    fn rest_exact(
        &mut self,
        rest: &[(Plan, bool)],
        from: usize,
        j: usize,
        caps: &[usize],
    ) -> Result<bool, Overflow> {
        let mut here = vec![from];
        for (e, _) in rest {
            let mut next: Vec<usize> = Vec::new();
            for p in here {
                next.extend(self.ends(e.prog, p, caps)?);
            }
            next.sort_unstable();
            next.dedup();
            here = next;
            if here.is_empty() {
                return Ok(false);
            }
        }
        Ok(here.contains(&j))
    }

    /// Positions reachable from `i` by at most `k` non-empty iterations of `x`
    /// (any number for `None`), `i` itself included.
    fn star_ends(
        &mut self,
        x: &Plan,
        k: Option<u32>,
        i: usize,
        caps: &[usize],
    ) -> Result<Vec<usize>, Overflow> {
        let mut reached: Vec<usize> = vec![i];
        let mut frontier = vec![i];
        let mut round = 0u32;
        while !frontier.is_empty() && k.is_none_or(|k| round < k) {
            let mut fresh = Vec::new();
            for p in &frontier {
                for e in self.ends(x.prog, *p, caps)? {
                    if e > *p && !reached.contains(&e) {
                        reached.push(e);
                        fresh.push(e);
                    }
                }
            }
            frontier = fresh;
            round += 1;
        }
        reached.sort_unstable();
        Ok(reached)
    }

    /// Positions `x{m,n}` can reach from `i` through `copies` plain copies
    /// after the optional ones: the part of an iteration that is matched but
    /// not dissected.
    fn blob_ends(
        &mut self,
        x: &Plan,
        optional: Option<u32>,
        copies: u32,
        i: usize,
        caps: &[usize],
    ) -> Result<Vec<usize>, Overflow> {
        let mut here = self.star_ends(x, optional, i, caps)?;
        for _ in 0..copies {
            let mut next: Vec<usize> = Vec::new();
            for p in here {
                next.extend(self.ends(x.prog, p, caps)?);
            }
            next.sort_unstable();
            next.dedup();
            here = next;
        }
        Ok(here)
    }

    /// The largest end of `x` from `from` that is at most `limit`.
    fn longest(
        &mut self,
        x: &Plan,
        from: usize,
        limit: usize,
        caps: &[usize],
    ) -> Result<Option<usize>, Overflow> {
        Ok(self
            .ends(x.prog, from, caps)?
            .into_iter()
            .rfind(|e| *e <= limit))
    }

    /// The smallest end of `x` from `from` that is at least `limit`.
    fn shortest(
        &mut self,
        x: &Plan,
        from: usize,
        limit: usize,
        caps: &[usize],
    ) -> Result<Option<usize>, Overflow> {
        Ok(self
            .ends(x.prog, from, caps)?
            .into_iter()
            .find(|e| *e >= limit))
    }

    /// `citerdissect` and `creviterdissect`: divide `[begin, end)` into
    /// iterations of `x` by choosing end points that suit the body's DFA and
    /// then dissecting each, backing up over the last one when one fails.
    /// Iterations are non-empty unless the minimum cannot be reached
    /// otherwise.
    #[allow(clippy::too_many_arguments)]
    fn citer(
        &mut self,
        x: &Plan,
        min: u32,
        max: Option<u32>,
        shorter: bool,
        begin: usize,
        end: usize,
        caps: &mut Vec<usize>,
    ) -> Result<bool, Overflow> {
        let mut min_matches = min as usize;
        if min_matches == 0 {
            if begin == end {
                return Ok(true);
            }
            min_matches = 1;
        }
        let mut max_matches = end - begin;
        if let Some(m) = max {
            max_matches = max_matches.min(m as usize);
        }
        max_matches = max_matches.max(min_matches);
        let held = caps.clone();
        let mut endpts = vec![begin; max_matches + 1];
        let mut nverified = 0usize;
        let mut k = 1usize;
        let mut limit = if shorter { begin } else { end };
        while k > 0 {
            // The k'th end point, or a reason to back up.
            let mut backtrack = false;
            if shorter {
                // Disallow a zero-length iteration unless it is needed.
                if limit == endpts[k - 1]
                    && limit != end
                    && (k >= min_matches || min_matches - k < end - limit)
                {
                    limit += 1;
                }
                match self.shortest(x, endpts[k - 1], limit, caps)? {
                    Some(e) if e <= end => endpts[k] = e,
                    _ => {
                        k -= 1;
                        backtrack = true;
                    }
                }
            } else {
                match self.longest(x, endpts[k - 1], limit, caps)? {
                    Some(e) => endpts[k] = e,
                    None => {
                        k -= 1;
                        backtrack = true;
                    }
                }
            }
            if !backtrack {
                nverified = nverified.min(k - 1);
                if endpts[k] != end {
                    if k >= max_matches {
                        k -= 1;
                        backtrack = true;
                    } else if !shorter
                        && endpts[k] == endpts[k - 1]
                        && (k >= min_matches || min_matches - k < end - endpts[k])
                    {
                        backtrack = true;
                    } else {
                        k += 1;
                        limit = if shorter { endpts[k - 1] } else { end };
                        continue;
                    }
                } else if k < min_matches {
                    backtrack = true;
                } else {
                    let mut i = nverified + 1;
                    while i <= k {
                        zap(x, caps);
                        if self.go(x, endpts[i - 1], endpts[i], caps)? {
                            nverified = i;
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    if i > k {
                        return Ok(true);
                    }
                    backtrack = true;
                }
            }
            debug_assert!(backtrack);
            // Consider a shorter (longer, for a body that prefers short) version
            // of the current iteration, a zero-length one only when needed.
            while k > 0 {
                if shorter {
                    if endpts[k] < end {
                        limit = endpts[k] + 1;
                        break;
                    }
                } else {
                    let prev = endpts[k - 1];
                    if endpts[k] > prev {
                        limit = endpts[k] - 1;
                        if limit > prev || (limit == prev && k <= min_matches) {
                            break;
                        }
                    }
                }
                k -= 1;
            }
        }
        *caps = held;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pattern: &str, subject: &str) -> Option<Vec<Option<String>>> {
        let chars: Vec<char> = subject.chars().collect();
        let engine = Engine::new(pattern, false).expect("compiles");
        let spans = engine.exec(&chars, false).expect("runs")?;
        Some(
            spans
                .into_iter()
                .map(|s| s.map(|(a, b)| chars[a..b].iter().collect()))
                .collect(),
        )
    }

    fn whole(pattern: &str, subject: &str) -> Option<String> {
        run(pattern, subject).and_then(|v| v.into_iter().next().flatten())
    }

    fn groups(pattern: &str, subject: &str) -> Vec<String> {
        run(pattern, subject)
            .expect("matches")
            .into_iter()
            .map(|g| g.unwrap_or_else(|| "-".to_string()))
            .collect()
    }

    #[test]
    fn alternation_prefers_the_longest() {
        assert_eq!(whole("a|ab", "ab").as_deref(), Some("ab"));
        assert_eq!(whole("x*|y", "y").as_deref(), Some("y"));
    }

    #[test]
    fn quantified_group_after_greedy_atom_is_longest() {
        assert_eq!(groups("a*(ab)?", "aab"), ["aab", "ab"]);
    }

    #[test]
    fn leading_non_greedy_quantifier_prefers_shortest() {
        assert_eq!(whole("a+?b*", "aabb").as_deref(), Some("a"));
    }

    #[test]
    fn backreference_repeats_the_capture() {
        assert_eq!(groups("(a*)\\k{1}", "aaaaa"), ["aaaa", "aa"]);
        assert_eq!(whole("(a)\\k{1}", "ab"), None);
    }

    #[test]
    fn backreference_to_an_unset_group_fails() {
        assert_eq!(whole("(?:(a)|b)\\k{1}", "b"), None);
    }

    #[test]
    fn lookahead_is_zero_width() {
        assert_eq!(whole("a(?=b)", "ab").as_deref(), Some("a"));
        assert_eq!(whole("a(?!b)", "ab"), None);
        assert_eq!(whole("a(?!b)", "ac").as_deref(), Some("a"));
    }

    #[test]
    fn earlier_groups_take_the_longest_share() {
        assert_eq!(
            groups("(a|ab)(c|bcd)(d*)", "abcd"),
            ["abcd", "ab", "c", "d"]
        );
    }

    #[test]
    fn plus_over_a_group_leaves_the_last_iteration_mandatory() {
        assert_eq!(groups("(a+)+", "aaa"), ["aaa", "a"]);
        assert_eq!(groups("(a+){1,2}", "aa"), ["aa", "a"]);
        assert_eq!(groups("(a+)*", "aaa"), ["aaa", "aaa"]);
        assert_eq!(groups("(a*)+", "aa"), ["aa", ""]);
    }

    #[test]
    fn empty_iterations_terminate() {
        assert_eq!(groups("(a*)*", "aa"), ["aa", "aa"]);
        assert_eq!(whole("(a*)+", "b").as_deref(), Some(""));
    }

    #[test]
    fn runaway_backtracking_is_refused_not_hung() {
        let chars: Vec<char> = "a".repeat(40).chars().collect();
        let engine = Engine::new("(a*)*\\k{1}b", false).expect("compiles");
        assert!(engine.exec(&chars, false).is_err());
    }
}
