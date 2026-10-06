//! `for`, `switch`, `catch` and `error`.
//!
//! Every command here obeys the compiler's invariant that a command leaves
//! exactly one value on the stack, and each branch of one is compiled at the
//! same entry depth, so `break` and `continue` can still discard a statically
//! known number of values before jumping.
//!
//! `catch` is the exception to "no runtime unwinder", and it is deliberately a
//! small one. `Op::ExtendedWide(ext_wide::CATCH, handler_ip)` records the
//! runtime stack and frame depths together with the op index of a handler
//! block that the compiler emits ahead of the guarded script and jumps over.
//! When a chunk stops with an error, the driver in [`crate::runtime`] restores
//! those depths, pushes the message, and resumes the VM at the handler. The
//! handler and the ordinary path meet at the same compile-time depth, so no
//! part of the surrounding code has to know a `catch` is there.

use fusevm::Op;

use crate::compiler::{ext, ext_wide, Body, CompileError, Compiler};
use crate::list;
use crate::parser::Word;

/// `Tcl_SwitchObjCmd`'s option table, in its order: the order a refusal lists
/// them in, and the order a prefix is resolved against.
const SWITCH_OPTIONS: &[&str] = &[
    "-exact",
    "-glob",
    "-indexvar",
    "-matchvar",
    "-nocase",
    "-regexp",
    "--",
];

/// `switch`'s own usage line, quoted by every arity refusal it makes.
const SWITCH_USAGE: &str =
    "wrong # args: should be \"switch ?-option ...? string ?pattern body ...? ?default body?\"";

/// How `switch` compares its subject to a pattern.
#[derive(Clone, Copy, PartialEq, Eq)]
/// How a `switch` clause matches, as the low bit of [`ext::MATCH`]'s operand.
/// The high bit carries `-nocase`, so the four combinations ride in one byte
/// and an emitter that knows nothing of case folding still means what it did.
enum Match {
    Exact,
    Glob,
    /// `-regexp`, matched by [`crate::regexp`]. Value 2, so it does not collide
    /// with the `-nocase` bit the operand carries at bit 1.
    Regexp = 4,
}

/// One `pattern body` clause, with `-` fall-through already resolved.
struct Clause {
    /// The pattern's literal text, or `None` when it is a word to evaluate.
    text: Option<String>,
    word: Option<Word>,
    body: String,
}

impl Compiler {
    /// `for start test next command`.
    pub(crate) fn cmd_for(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [start, test, next, body] = args else {
            return self.error("wrong # args: should be \"for start test next command\"");
        };
        // The test is compiled at the *bottom* of the rotated shape, so a
        // computed one would be refused after ops exist and could no longer
        // become code in the branch. Asked here, before anything is emitted,
        // for the reason `Compiler::cmd_while` states.
        self.literal_of(test, "condition")?;
        let start = self.body_of(start)?;
        let body = self.body_of(body)?;
        let next = self.body_of(next)?;

        self.emit_body(&start)?;
        // `continue` skips the rest of the body and runs the step; `break` in
        // the step terminates the loop, as `for(n)` specifies. Both fall out of
        // the rotated shape, where the step precedes the next test.
        self.rotated_loop(
            |c| c.emit_body(&body),
            |c| c.emit_body(&next),
            |c| c.expr_word(test),
        )?;
        self.push_empty();
        Ok(())
    }

    /// `switch ?options? string pattern body ?pattern body ...?` and the form
    /// that groups the patterns and bodies into one braced list.
    pub(crate) fn cmd_switch(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let mut i = 0;
        let mut mode = Match::Exact;
        let mut nocase = false;
        let mut matchvar: Option<String> = None;
        let mut indexvar: Option<String> = None;
        // Whether a mode option was written, which a second one is refused for.
        let mut foundmode = false;
        // `switch(n)`: a leading `-` argument is an option only while at least
        // two arguments follow it — the subject and the patterns. That bound is
        // the interpreter's own (`Tcl_SwitchObjCmd` scans `i < objc-2`), and it
        // is why `switch -exact -9223372036854775807 {a* {…} default {…}}`
        // matches on the number rather than reporting a bad option: after
        // `-exact` only two arguments remain, so the number is the subject.
        while i + 2 < args.len() {
            let Some(text) = args.get(i).and_then(|w| w.as_literal()) else {
                break;
            };
            if !text.starts_with('-') {
                break;
            }
            i += 1;
            // `Tcl_GetIndexFromObj` over the option table, so a unique prefix
            // is the option (`-exa`, `-gl`, `-m`) and `-` alone is ambiguous.
            // A refusal is the command's own, reported when it runs: deferred
            // here for the reason `wrong # args` is.
            let option = crate::cmd_list::option(SWITCH_OPTIONS, text)
                .map_err(|m| self.deferrable_err(m))?;
            match SWITCH_OPTIONS[option] {
                "--" => break,
                "-nocase" => nocase = true,
                // A second mode option names the first one found — `-exact`
                // when none was written yet is impossible, since the first
                // one sets `foundmode`.
                name @ ("-exact" | "-glob" | "-regexp") => {
                    if foundmode {
                        let found = match mode {
                            Match::Exact => "-exact",
                            Match::Glob => "-glob",
                            Match::Regexp => "-regexp",
                        };
                        return Err(self.deferrable_err(format!(
                            "bad option \"{text}\": {found} option already found"
                        )));
                    }
                    foundmode = true;
                    mode = match name {
                        "-exact" => Match::Exact,
                        "-glob" => Match::Glob,
                        _ => Match::Regexp,
                    };
                }
                // Both take the *next* word as a variable name, and that word
                // must leave the subject and a pattern behind it: the
                // `i >= objc-2` re-test `Tcl_SwitchObjCmd` makes after
                // consuming it.
                name => {
                    if i + 2 >= args.len() {
                        return Err(self.deferrable_err(format!(
                            "missing variable name argument to {name} option"
                        )));
                    }
                    let var = self.var_name_of(&args[i])?;
                    i += 1;
                    if name == "-matchvar" {
                        matchvar = Some(var);
                    } else {
                        indexvar = Some(var);
                    }
                }
            }
        }

        // Both variables are filled from the regular expression's capture
        // information, so neither means anything without `-regexp`. tclsh tests
        // `-indexvar` first and stops there, so `-matchvar m -indexvar i -glob`
        // reports the index one — measured, and the order is visible.
        if indexvar.is_some() && mode != Match::Regexp {
            return Err(self.deferrable_err("-indexvar option requires -regexp option"));
        }
        if matchvar.is_some() && mode != Match::Regexp {
            return Err(self.deferrable_err("-matchvar option requires -regexp option"));
        }

        let Some(subject) = args.get(i) else {
            return self.error(SWITCH_USAGE);
        };
        let clauses = self.switch_clauses(&args[i + 1..])?;

        // The capture operand the two variables ride in: which of them was
        // given, and where each lives. Zero when neither was, in which case
        // every clause keeps the plain matcher.
        let captures = match (&matchvar, &indexvar) {
            (None, None) => None,
            (m, x) => Some((
                i64::from(m.is_some()) | i64::from(x.is_some()) << 1,
                m.as_deref().map_or(0, |n| self.place_operand(n)),
                x.as_deref().map_or(0, |n| self.place_operand(n)),
            )),
        };

        self.word(subject)?;
        let entry = self.depth;
        let mut ends = Vec::new();
        let mut defaulted = false;

        for (k, clause) in clauses.iter().enumerate() {
            self.depth = entry;
            let last = k + 1 == clauses.len();
            if last && clause.text.as_deref() == Some("default") {
                // `default` matches anything, but only as the final pattern.
                defaulted = true;
                self.emit(Op::Pop, -1);
                // No regular expression ran, so there is nothing to report:
                // tclsh writes an empty list into both variables before the
                // default body, which is not the same as leaving them alone —
                // a `switch` that matches nothing at all and has no `default`
                // does leave them alone. Measured both ways.
                if let Some((given, m, x)) = captures {
                    self.emit(Op::LoadInt(given), 1);
                    self.emit(Op::LoadInt(m), 1);
                    self.emit(Op::LoadInt(x), 1);
                    self.emit(Op::Extended(crate::regexp::ext::SWITCH_CLEAR, 3), -3);
                }
                self.switch_body(&clause.body)?;
                break;
            }
            self.emit(Op::Dup, 1);
            match (&clause.text, &clause.word) {
                (Some(text), _) => self.push_text(text),
                (None, Some(w)) => self.word(w)?,
                (None, None) => unreachable!("a clause has a pattern"),
            }
            // The string module's matcher, so `-nocase` folds exactly as
            // `string match -nocase` folds; it answers "1"/"0", which the
            // boolean op turns into the 1/0 the branch below tests.
            //
            // `-matchvar`/`-indexvar` need what matched and not only whether
            // something did, so those go to the regular-expression module's own
            // op instead. It answers "1"/"0" too, which is why the two are
            // interchangeable here.
            match captures {
                Some((given, m, x)) => {
                    self.emit(Op::LoadInt(i64::from(nocase)), 1);
                    self.emit(Op::LoadInt(given), 1);
                    self.emit(Op::LoadInt(m), 1);
                    self.emit(Op::LoadInt(x), 1);
                    self.emit(Op::Extended(crate::regexp::ext::SWITCH_VARS, 6), -5);
                }
                None => {
                    self.emit(
                        Op::LoadInt(i64::from(mode as u8 | u8::from(nocase) << 1)),
                        1,
                    );
                    self.emit(Op::Extended(crate::cmd_string::ext::SWITCH_MATCH, 3), -2);
                }
            }
            self.emit(Op::Extended(ext::BOOL, 0), 0);
            let miss = self.emit(Op::JumpIfFalse(usize::MAX), -1);
            self.emit(Op::Pop, -1);
            self.switch_body(&clause.body)?;
            ends.push(self.emit(Op::Jump(usize::MAX), 0));
            let next = self.b.current_pos();
            self.b.patch_jump(miss, next);
        }

        self.depth = entry;
        if !defaulted {
            // Nothing matched: the subject is discarded and `switch` is empty.
            self.emit(Op::Pop, -1);
            self.push_empty();
        }
        let end = self.b.current_pos();
        for j in ends {
            self.b.patch_jump(j, end);
        }
        Ok(())
    }

    /// Flatten a `switch` tail into clauses, resolving the `-` body that means
    /// "share the next pattern's body" by repeating that body per pattern.
    fn switch_clauses(&mut self, tail: &[Word]) -> Result<Vec<Clause>, CompileError> {
        let mut patterns: Vec<(Option<String>, Option<Word>)> = Vec::new();
        let mut bodies: Vec<String> = Vec::new();

        // Every refusal in this function is one `Tcl_SwitchObjCmd` makes while
        // running: an odd number of words, a `-` body with nothing after it, a
        // pattern list that will not parse. tclsh reaches none of them until
        // the command is invoked, so `if {0} {switch -- x {a}}` costs a script
        // nothing there and `catch {switch -- x {a}}` answers 1. Nothing has
        // been emitted yet at this point, which is what lets them defer.
        if tail.is_empty() {
            return Err(self.deferrable_err(SWITCH_USAGE));
        }
        if tail.len() == 1 {
            // The grouped form: the whole tail is one list, and because braces
            // suppress substitution its patterns are literal text.
            let text = self.literal_of(&tail[0], "switch pattern list")?;
            let elements = match list::split(text) {
                Ok(elements) => elements,
                Err(msg) => return Err(self.deferrable_err(msg)),
            };
            if elements.is_empty() {
                return Err(self.deferrable_err(SWITCH_USAGE));
            }
            if !elements.len().is_multiple_of(2) {
                return Err(self.deferrable_err("extra switch pattern with no body"));
            }
            for pair in elements.chunks(2) {
                patterns.push((Some(pair[0].clone()), None));
                bodies.push(pair[1].clone());
            }
        } else {
            if !tail.len().is_multiple_of(2) {
                return Err(self.deferrable_err("extra switch pattern with no body"));
            }
            for pair in tail.chunks(2) {
                bodies.push(self.literal_of(&pair[1], "switch body")?.to_string());
                match pair[0].as_literal() {
                    Some(text) => patterns.push((Some(text.to_string()), None)),
                    None => patterns.push((None, Some(pair[0].clone()))),
                }
            }
        }

        let mut clauses = Vec::with_capacity(patterns.len());
        for (k, (text, word)) in patterns.into_iter().enumerate() {
            let mut at = k;
            while bodies[at] == "-" {
                at += 1;
                if at >= bodies.len() {
                    return Err(self.deferrable_err(format!(
                        "no body specified for pattern \"{}\"",
                        text.as_deref().unwrap_or_default()
                    )));
                }
            }
            clauses.push(Clause {
                text,
                word,
                body: bodies[at].clone(),
            });
        }
        Ok(clauses)
    }

    fn switch_body(&mut self, text: &str) -> Result<(), CompileError> {
        // An arm that is never selected is never parsed by tclsh, so an arm
        // whose text will not parse raises only if it is the arm chosen.
        match crate::parser::parse(text) {
            Ok(script) => self.nested_value(&script),
            Err(e) => self.emit_body_value(&Body::deferred(text, e.msg)),
        }
    }

    /// `catch script ?resultVarName?`.
    pub(crate) fn cmd_catch(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (body, var, opts_var) = match args {
            [b] => (b, None, None),
            [b, v] => (b, Some(self.var_name_of(v)?), None),
            [b, v, o] => (b, Some(self.var_name_of(v)?), Some(self.var_name_of(o)?)),
            _ => {
                return self.error(
                    "wrong # args: should be \"catch script ?resultVarName? ?optionVarName?\"",
                )
            }
        };
        let script = self.body_of(body)?;
        let entry = self.depth;

        // The handler comes first so its op index is known when the region is
        // opened; the ordinary path jumps over it.
        let over = self.emit(Op::Jump(usize::MAX), 0);
        let handler = self.b.current_pos();
        // The driver resumes here having pushed the code, the return options
        // and the result, in that order — so the result is on top, the options
        // under it, and the code under those, which is the value `catch` is.
        self.depth = entry + 3;
        self.store_or_drop(var.as_deref());
        self.store_or_drop(opts_var.as_deref());
        let to_end = self.emit(Op::Jump(usize::MAX), 0);

        let guarded = self.b.current_pos();
        self.b.patch_jump(over, guarded);
        self.depth = entry;
        self.emit(Op::ExtendedWide(ext_wide::CATCH, handler), 0);
        self.catch_depth += 1;
        let compiled = self.emit_body_value(&script);
        self.catch_depth -= 1;
        compiled?;
        self.emit(Op::Extended(ext::CATCH_END, 0), 0);
        self.store_or_drop(var.as_deref());
        // The script completed, so the options say so: code 0, level 0.
        if let Some(name) = opts_var.as_deref() {
            self.push_str("-code 0 -level 0");
            self.store_or_drop(Some(name));
        }
        self.emit(Op::LoadInt(0), 1);

        let end = self.b.current_pos();
        self.b.patch_jump(to_end, end);
        Ok(())
    }

    /// Run `body` with a cleanup that happens *however the body ended* — Tcl's
    /// `finally`, which `dict update` and `dict with` are built out of.
    ///
    /// The reference implementation writes one with NRE: `DictUpdateCmd` pushes
    /// `FinalizeDictUpdate` as a callback and then evaluates the body, so the
    /// callback runs on every path and `Tcl_RestoreInterpState` puts the body's
    /// result — code and message alike — back afterwards
    /// (`generic/tclDictObj.c:3539` and `:3545-3596`). There is no NRE stack
    /// here, so the same shape is built out of the `catch` region: it absorbs
    /// every code, the handler runs the cleanup, and [`ext::RERAISE`] hands the
    /// code back on unchanged.
    ///
    /// The caller has already pushed a *record* — whatever the cleanup needs to
    /// do its work — and that record sits below the region's stack mark, so the
    /// driver's unwind to `frame.stack` leaves it untouched and the handler
    /// finds it exactly where the ordinary path does. `cleanup` is emitted with
    /// the number of values sitting on top of it as its inline operand: one on
    /// the ordinary path (the body's value), three in the handler (the code, the
    /// options and the message the driver pushed). It consumes the record and
    /// nothing else, so the command leaves one value where the record was.
    pub(crate) fn finally_region<F>(&mut self, cleanup: u16, body: F) -> Result<(), CompileError>
    where
        F: FnOnce(&mut Self) -> Result<(), CompileError>,
    {
        let entry = self.depth;

        // The handler comes first so its op index is known when the region is
        // opened, exactly as `catch`'s does; the ordinary path jumps over it.
        let over = self.emit(Op::Jump(usize::MAX), 0);
        let handler = self.b.current_pos();
        self.depth = entry + 3;
        self.emit(Op::Extended(cleanup, 3), -1);
        // Control leaves here on every path, so nothing follows and the depth
        // this arm reached never has to meet the ordinary path's.
        self.emit(Op::Extended(ext::RERAISE, 0), -3);

        let guarded = self.b.current_pos();
        self.b.patch_jump(over, guarded);
        self.depth = entry;
        self.emit(Op::ExtendedWide(ext_wide::CATCH, handler), 0);
        self.catch_depth += 1;
        let compiled = body(self);
        self.catch_depth -= 1;
        compiled?;
        self.emit(Op::Extended(ext::CATCH_END, 0), 0);
        // A cleanup that fails does so *outside* the region, so its own error
        // reaches whatever encloses the command rather than re-entering the
        // handler — which is what `FinalizeDictUpdate` does when the write-back
        // finds the variable is no longer a dictionary (`:3583`).
        self.emit(Op::Extended(cleanup, 1), -1);
        Ok(())
    }

    /// `error message ?errorInfo? ?errorCode?`.
    ///
    /// All three words are evaluated, in the order written — `Tcl_ErrorObjCmd`
    /// (`generic/tclCmdAH.c:596-628`) receives them already substituted, so a
    /// command substitution in the second or the third has run whatever the
    /// first does. What the two extras *set* is `-errorinfo` and `-errorcode`,
    /// the return options this frontend does not carry, so they are dropped —
    /// which is visible at the point either option is asked for, and is the
    /// same gap `throw`'s type word already has.
    ///
    /// They were refused outright until `lsort -command` landed and made the
    /// three-argument form reachable without anyone writing it: a comparison
    /// script is called with the two elements appended, so `lsort -command
    /// {error boom} {a b}` runs `error boom a b`. tclsh reports `boom`; the
    /// refusal reported the arity message instead.
    pub(crate) fn cmd_error(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [message, extra @ ..] = args else {
            return self.error("wrong # args: should be \"error message ?errorInfo? ?errorCode?\"");
        };
        if extra.len() > 2 {
            return self.error("wrong # args: should be \"error message ?errorInfo? ?errorCode?\"");
        }
        self.word(message)?;
        for w in extra {
            self.word(w)?;
        }
        self.emit(
            Op::Extended(ext::ERROR, extra.len() as u8),
            -(extra.len() as i32 + 1),
        );
        // Control has left; the value keeps the depth arithmetic honest.
        self.push_empty();
        Ok(())
    }

    /// `throw type message`.
    ///
    /// The type is a *value*, and whether it is a well-formed list of at least
    /// one element is decided when the command runs — `Tcl_ThrowObjCmd` reaches
    /// `TclListObjLength` there, so `catch {throw "\{" x}` is a caught error in
    /// tclsh rather than a script that will not compile, and `throw $t $m` is
    /// ordinary. Both words therefore ride on the stack.
    pub(crate) fn cmd_throw(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [kind, message] = args else {
            return self.error("wrong # args: should be \"throw type message\"");
        };
        self.word(kind)?;
        self.word(message)?;
        self.emit(Op::Extended(ext::THROW, 0), -2);
        // Control has left; the value keeps the depth arithmetic honest.
        self.push_empty();
        Ok(())
    }

    /// `try body ?handler ...? ?finally script?` (`Tcl_TryObjCmd`,
    /// `generic/tclCmdMZ.c:4692-4870`).
    ///
    /// The body runs in a `catch` region, so both of its endings arrive at one
    /// dispatch point holding `[code, options, result]` — the ordinary path
    /// builds that triple itself with code 0. [`ext::TRY_MATCH`] picks the
    /// first handler whose `on` code or `trap` prefix fits, the handler binds
    /// its variables and runs as the command's value, and an outcome no
    /// handler takes is handed on unchanged through [`ext::RERAISE`] (or, for
    /// code 0, simply becomes the value). A `finally` wraps all of that in a
    /// second region: its script runs on the ordinary path and in the handler,
    /// its value is dropped, and an error it raises replaces whatever was
    /// leaving — the order `TryPostFinal` applies.
    pub(crate) fn cmd_try(&mut self, args: &[Word]) -> Result<(), CompileError> {
        const USAGE: &str = "wrong # args: should be \"try body ?handler ...? ?finally script?\"";
        let Some((body, mut rest)) = args.split_first() else {
            return Err(self.deferrable_err(USAGE));
        };
        let mut handlers: Vec<TryHandler> = Vec::new();
        let mut finally = None;
        while let Some(kind) = rest.first() {
            let kind = self.literal_of(kind, "try handler type")?.to_string();
            match kind.as_str() {
                "on" | "trap" => {
                    if rest.len() < 4 {
                        return Err(self.deferrable_err(format!(
                            "wrong # args to {kind} clause: must be \"... {kind} {} variableList script\"",
                            if kind == "on" { "code" } else { "pattern" }
                        )));
                    }
                    let what = self.literal_of(&rest[1], "try handler")?.to_string();
                    let test = if kind == "on" {
                        let code = completion_code(&what).ok_or_else(|| {
                            self.deferrable_err(format!(
                                "bad completion code \"{what}\": must be ok, error, return, break, continue, or an integer"
                            ))
                        })?;
                        format!("on {code}")
                    } else {
                        list::join(&["trap", what.as_str()])
                    };
                    let vars = self.literal_of(&rest[2], "try handler variable list")?;
                    let vars = list::split(vars).map_err(|e| self.deferrable_err(e))?;
                    let script = self.literal_of(&rest[3], "try handler script")?.to_string();
                    handlers.push(TryHandler { test, vars, script });
                    rest = &rest[4..];
                }
                "finally" => {
                    match rest.len() {
                        1 => {
                            return Err(self.deferrable_err(
                                "wrong # args to finally clause: must be \"... finally script\"",
                            ))
                        }
                        2 => {}
                        _ => return Err(self.deferrable_err("finally clause must be last")),
                    }
                    finally = Some(self.body_of(&rest[1])?);
                    rest = &[];
                }
                other => {
                    return Err(self.deferrable_err(format!(
                        "bad handler type \"{other}\": must be finally, on, or trap"
                    )))
                }
            }
        }
        if handlers.last().is_some_and(|h| h.script == "-") {
            return Err(
                self.deferrable_err("last non-finally clause must not have a body of \"-\"")
            );
        }
        let body = self.body_of(body)?;
        match finally {
            None => self.try_core(&body, &handlers),
            Some(cleanup) => {
                let entry = self.depth;
                let over = self.emit(Op::Jump(usize::MAX), 0);
                // Handler: `[code, options, message]` from the driver. Run the
                // cleanup for its effect, then hand the outcome on.
                let handler = self.b.current_pos();
                self.depth = entry + 3;
                self.emit_body(&cleanup)?;
                self.emit(Op::Extended(ext::RERAISE, 0), -3);

                let guarded = self.b.current_pos();
                self.b.patch_jump(over, guarded);
                self.depth = entry;
                self.emit(Op::ExtendedWide(ext_wide::CATCH, handler), 0);
                self.catch_depth += 1;
                let compiled = self.try_core(&body, &handlers);
                self.catch_depth -= 1;
                compiled?;
                self.emit(Op::Extended(ext::CATCH_END, 0), 0);
                // Outside the region, so an error the cleanup raises leaves
                // the command instead of re-entering the handler above.
                self.emit_body(&cleanup)
            }
        }
    }

    /// The body and its handlers, leaving one value: see [`Compiler::cmd_try`].
    fn try_core(&mut self, body: &Body, handlers: &[TryHandler]) -> Result<(), CompileError> {
        let entry = self.depth;
        let over = self.emit(Op::Jump(usize::MAX), 0);
        // The driver resumes here with `[code, options, result]` pushed.
        let caught = self.b.current_pos();
        self.depth = entry + 3;
        let to_dispatch = self.emit(Op::Jump(usize::MAX), 0);

        let guarded = self.b.current_pos();
        self.b.patch_jump(over, guarded);
        self.depth = entry;
        self.emit(Op::ExtendedWide(ext_wide::CATCH, caught), 0);
        self.catch_depth += 1;
        let compiled = self.emit_body_value(body);
        self.catch_depth -= 1;
        compiled?;
        self.emit(Op::Extended(ext::CATCH_END, 0), 0);
        // The ordinary ending, as the same triple: `[0, "-code 0 -level 0", v]`.
        self.emit(Op::LoadInt(0), 1);
        self.emit(Op::Swap, 0);
        self.push_str("-code 0 -level 0");
        self.emit(Op::Swap, 0);

        let dispatch = self.b.current_pos();
        self.b.patch_jump(to_dispatch, dispatch);
        self.depth = entry + 3;
        let spec: Vec<&str> = handlers.iter().map(|h| h.test.as_str()).collect();
        self.push_str(&list::join(&spec));
        self.emit(Op::Extended(ext::TRY_MATCH, 0), 0);

        let mut to_end = Vec::new();
        for (i, h) in handlers.iter().enumerate() {
            // `[code, options, result, index]`
            self.emit(Op::Dup, 1);
            self.emit(Op::LoadInt(i as i64), 1);
            self.emit(Op::NumEq, -1);
            let miss = self.emit(Op::JumpIfFalse(usize::MAX), -1);
            self.emit(Op::Pop, -1);
            // A body of `-` is the next handler's (`TryPostBody`).
            let script = handlers[i..]
                .iter()
                .map(|h| h.script.as_str())
                .find(|s| *s != "-")
                .unwrap_or_default();
            let mut vars = h.vars.iter().map(String::as_str);
            let result_var = vars.next();
            let options_var = vars.next();
            self.store_or_drop(result_var);
            self.store_or_drop(options_var);
            self.emit(Op::Pop, -1);
            let body = match crate::parser::parse(script) {
                Ok(script) => Body::Script(script),
                Err(e) => Body::deferred(script, e.msg),
            };
            self.emit_body_value(&body)?;
            to_end.push(self.emit(Op::Jump(usize::MAX), 0));
            let next = self.b.current_pos();
            self.b.patch_jump(miss, next);
            self.depth = entry + 4;
        }
        // No handler took it. Code 0 is the command's value; anything else
        // leaves as it arrived.
        self.emit(Op::Dup, 1);
        self.emit(Op::LoadInt(-1), 1);
        self.emit(Op::NumEq, -1);
        let reraise = self.emit(Op::JumpIfFalse(usize::MAX), -1);
        self.emit(Op::Pop, -1);
        self.emit(Op::Swap, 0);
        self.emit(Op::Pop, -1);
        self.emit(Op::Swap, 0);
        self.emit(Op::Pop, -1);
        to_end.push(self.emit(Op::Jump(usize::MAX), 0));
        let here = self.b.current_pos();
        self.b.patch_jump(reraise, here);
        self.depth = entry + 4;
        self.emit(Op::Pop, -1);
        self.emit(Op::Extended(ext::RERAISE, 0), -3);

        let end = self.b.current_pos();
        for j in to_end {
            self.b.patch_jump(j, end);
        }
        self.depth = entry + 1;
        Ok(())
    }

    /// Store the top of the stack in `var`, or discard it when `catch` was
    /// given no variable to write.
    fn store_or_drop(&mut self, var: Option<&str>) {
        match var {
            Some(name) => {
                // An array refuses, as `set` does, and the refusal is `catch`'s
                // own error: `Tcl_CatchObjCmd` returns `TCL_ERROR` when
                // `Tcl_ObjSetVar2` fails.
                self.scalar_set_guard(name);
                self.emit_set_var(name)
            }
            None => {
                self.emit(Op::Pop, -1);
            }
        }
    }
}

/// One `on`/`trap` clause of a `try`, as [`ext::TRY_MATCH`] reads its test:
/// `on <code>` or `trap <pattern>`.
struct TryHandler {
    test: String,
    /// `?resultVar? ?optionsVar?` — a longer list binds its first two.
    vars: Vec<String>,
    /// The handler script, or `-` for "the next handler's".
    script: String,
}

/// `TclGetCompletionCodeFromObj` (`generic/tclIndexObj.c:1360-1392`): a code
/// name or an integer.
fn completion_code(word: &str) -> Option<i64> {
    match word {
        "ok" => Some(0),
        "error" => Some(1),
        "return" => Some(2),
        "break" => Some(3),
        "continue" => Some(4),
        _ => list::parse_int(word),
    }
}
