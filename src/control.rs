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

use crate::compiler::{ext, ext_wide, CompileError, Compiler, LoopCtx};
use crate::list;
use crate::parser::Word;

/// How `switch` compares its subject to a pattern.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    Exact,
    Glob,
}

/// One `pattern body` clause, with `-` fall-through already resolved.
struct Clause {
    /// The pattern's literal text, or `None` when it is a word to evaluate.
    text: Option<String>,
    word: Option<Word>,
    body: String,
}

impl Compiler {
    /// `for start test next body`.
    pub(crate) fn cmd_for(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [start, test, next, body] = args else {
            return self.error("wrong # args: should be \"for start test next body\"");
        };
        let start = self.body_script(start)?;
        let body = self.body_script(body)?;
        let next = self.body_script(next)?;

        self.nested_effect(&start)?;
        let top = self.b.current_pos();
        self.expr_word(test)?;
        let exit = self.emit(Op::JumpIfFalse(usize::MAX), -1);

        self.loops.push(LoopCtx {
            depth: self.depth,
            catch_depth: self.catch_depth,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.nested_effect(&body)?;
        // `for(n)`: `break` in the step terminates the loop too, so the step
        // is compiled with the loop still open.
        let step = self.b.current_pos();
        self.nested_effect(&next)?;
        let ctx = self.loops.pop().expect("loop context");

        let backedge = self.emit(Op::Jump(usize::MAX), 0);
        self.b.patch_jump(backedge, top);
        // `continue` skips the rest of the body and runs the step.
        for j in ctx.continues {
            self.b.patch_jump(j, step);
        }
        let end = self.b.current_pos();
        self.b.patch_jump(exit, end);
        for j in ctx.breaks {
            self.b.patch_jump(j, end);
        }
        self.push_empty();
        Ok(())
    }

    /// `switch ?options? string pattern body ?pattern body ...?` and the form
    /// that groups the patterns and bodies into one braced list.
    pub(crate) fn cmd_switch(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let mut i = 0;
        let mut mode = Match::Exact;
        // `switch(n)`: leading `-` arguments are options unless the command
        // has exactly two arguments, where the first is always the subject.
        if args.len() != 2 {
            while let Some(text) = args.get(i).and_then(|w| w.as_literal()) {
                if !text.starts_with('-') {
                    break;
                }
                i += 1;
                match text {
                    "-exact" => mode = Match::Exact,
                    "-glob" => mode = Match::Glob,
                    "--" => break,
                    other => {
                        return self.error(format!(
                            "bad option \"{other}\": only -exact, -glob and -- are supported"
                        ))
                    }
                }
            }
        }

        let Some(subject) = args.get(i) else {
            return self
                .error("wrong # args: should be \"switch ?options? string {pattern body ...}\"");
        };
        let clauses = self.switch_clauses(&args[i + 1..])?;

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
                self.switch_body(&clause.body)?;
                break;
            }
            self.emit(Op::Dup, 1);
            match (&clause.text, &clause.word) {
                (Some(text), _) => self.push_text(text),
                (None, Some(w)) => self.word(w)?,
                (None, None) => unreachable!("a clause has a pattern"),
            }
            self.emit(Op::Extended(ext::MATCH, mode as u8), -1);
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

        if tail.is_empty() {
            return self
                .error("wrong # args: should be \"switch ?options? string pattern body ...\"");
        }
        if tail.len() == 1 {
            // The grouped form: the whole tail is one list, and because braces
            // suppress substitution its patterns are literal text.
            let text = self.literal_of(&tail[0], "switch pattern list")?;
            let elements = match list::split(text) {
                Ok(elements) => elements,
                Err(msg) => return self.error(msg),
            };
            if elements.is_empty() {
                return self.error(
                    "wrong # args: should be \"switch ?options? string {pattern body ...}\"",
                );
            }
            if !elements.len().is_multiple_of(2) {
                return self.error("extra switch pattern with no body");
            }
            for pair in elements.chunks(2) {
                patterns.push((Some(pair[0].clone()), None));
                bodies.push(pair[1].clone());
            }
        } else {
            if !tail.len().is_multiple_of(2) {
                return self.error("extra switch pattern with no body");
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
                    return self.error(format!(
                        "no body specified for pattern \"{}\"",
                        text.as_deref().unwrap_or_default()
                    ));
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
        let script = match crate::parser::parse(text) {
            Ok(s) => s,
            Err(e) => {
                return Err(CompileError {
                    msg: e.msg,
                    line: self.line,
                })
            }
        };
        self.nested_value(&script)
    }

    /// `catch script ?resultVarName?`.
    pub(crate) fn cmd_catch(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let (body, var) = match args {
            [b] => (b, None),
            [b, v] => (b, Some(self.var_name_of(v)?)),
            _ => {
                return self.error(
                    "wrong # args: should be \"catch script ?resultVarName?\"; the options \
                     variable is not supported",
                )
            }
        };
        let script = self.body_script(body)?;
        let entry = self.depth;

        // The handler comes first so its op index is known when the region is
        // opened; the ordinary path jumps over it.
        let over = self.emit(Op::Jump(usize::MAX), 0);
        let handler = self.b.current_pos();
        // The driver resumes here with the error message on the stack.
        self.depth = entry + 1;
        self.store_or_drop(var.as_deref());
        self.emit(Op::LoadInt(1), 1);
        let to_end = self.emit(Op::Jump(usize::MAX), 0);

        let guarded = self.b.current_pos();
        self.b.patch_jump(over, guarded);
        self.depth = entry;
        self.emit(Op::ExtendedWide(ext_wide::CATCH, handler), 0);
        self.catch_depth += 1;
        let compiled = self.nested_value(&script);
        self.catch_depth -= 1;
        compiled?;
        self.emit(Op::Extended(ext::CATCH_END, 0), 0);
        self.store_or_drop(var.as_deref());
        self.emit(Op::LoadInt(0), 1);

        let end = self.b.current_pos();
        self.b.patch_jump(to_end, end);
        Ok(())
    }

    /// `error message` — `info` and `code` set return options this frontend
    /// does not model.
    pub(crate) fn cmd_error(&mut self, args: &[Word]) -> Result<(), CompileError> {
        let [message] = args else {
            return self.error(
                "wrong # args: should be \"error message\"; the info and code arguments are \
                 not supported",
            );
        };
        self.word(message)?;
        self.emit(Op::Extended(ext::ERROR, 0), -1);
        // Control has left; the value keeps the depth arithmetic honest.
        self.push_empty();
        Ok(())
    }

    /// Store the top of the stack in `var`, or discard it when `catch` was
    /// given no variable to write.
    fn store_or_drop(&mut self, var: Option<&str>) {
        match var {
            Some(name) => self.emit_set_var(name),
            None => {
                self.emit(Op::Pop, -1);
            }
        }
    }
}
