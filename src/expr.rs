//! The `expr` expression language.
//!
//! `expr` is a second grammar layered on Tcl's word syntax: operators, C-like
//! precedence, and operands that may themselves be substitutions. Parsing it
//! separately from the command language is what lets a braced expression —
//! `expr {$i < $n}` — be compiled once instead of re-parsed on every
//! evaluation, which is the single largest cost in the reference
//! implementation's hot loops.
//!
//! Precedence and associativity follow `expr(n)`, verified against tclsh 9.0.4:
//! `**` groups right-to-left, everything else left-to-right, and the
//! string-comparison operators share a level with their numeric counterparts
//! (`"a" eq "a" == 1` is 1, so `eq` cannot bind looser than `==`).

use crate::parser::{self, ParseError, Part};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    BitNot,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Pow,
    Mul,
    Div,
    Mod,
    Add,
    Sub,
    Shl,
    Shr,
    /// Numeric-preferring comparisons, falling back to string order.
    Lt,
    Gt,
    Le,
    Ge,
    /// Always-string comparisons: `lt gt le ge`.
    StrLt,
    StrGt,
    StrLe,
    StrGe,
    Eq,
    Ne,
    /// `eq ne`.
    StrEq,
    StrNe,
    In,
    Ni,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// An integer literal: its value, and the text the script wrote — which is
    /// what `eq` and the other always-string operators compare, since `010` and
    /// `10` are the same number and different strings.
    Int(i64, Box<str>),
    /// A double literal, with its spelling for the same reason: `1e3` and
    /// `1000.0` are one number and two strings.
    Float(f64, Box<str>),
    /// An operand built from literal text and substitutions: a quoted or braced
    /// string, `$var`, `$arr(i)`, or `[script]`.
    Subst(Vec<Part>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A math function call — parsed here, lowered in a later phase.
    Call(String, Vec<Expr>),
}

/// How deeply subexpressions may nest before the parser refuses to go further.
///
/// This parser is recursive descent, so nesting costs native stack — and running
/// out of it is a signal, not an error, which kills the process with nothing to
/// report. [`crate::parser::MAX_NESTING_DEPTH`] bounds the command language's
/// recursion for the same reason; this is the same mechanism for the expression
/// language.
///
/// The number is measured, not chosen for looks. On the stack the `tclrs` binary
/// gives the parser ([`crate::runtime::RECOMMENDED_STACK`], which is what a host
/// embedding this crate is documented to provide), an unoptimized build of
/// `expr {((…1…))}` still parses and compiles at 7_500 parentheses and aborts by
/// 8_000; a chain of unary operators, whose frames are far cheaper, survives
/// 100_000 and aborts by 150_000. 5_000 is a third under the parenthesis floor,
/// which is the expensive descent of the two.
///
/// The unoptimized build is the one to calibrate against: it is the weakest this
/// crate is built as, and the one `cargo test` runs. An optimized build's frames
/// are small enough to survive 32_000 parentheses on the same stack, so a limit
/// set from *its* floor would leave a debug build aborting where a release build
/// only reported an error.
///
/// Unlike the command parser's limit, this one is *not* above every depth the
/// reference interpreter survives: tclsh 9.0.4 parses expressions with an
/// explicit stack (`tclCompExpr.c`) rather than by recursion, and evaluates
/// 1_000_000 nested parentheses without complaint. Bounding here therefore
/// refuses a handful of inputs tclsh accepts. That is the deliberate trade — an
/// input past the limit gets a Tcl error the script can catch, where before it
/// took the whole process down.
pub const MAX_EXPR_DEPTH: usize = 5_000;

/// Parse a complete expression.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut p = ExprParser {
        src,
        pos: 0,
        open_parens: 0,
        depth: 0,
    };
    p.skip_space();
    // An expression with no tokens at all is its own diagnostic in tclsh 9.0.4,
    // and is reached before any operand is looked for: `expr {}` and `expr {  }`
    // are both `empty expression`, not the missing-operand the first token's
    // absence would otherwise report.
    if p.pos >= p.src.len() {
        return Err(in_expression(src, p.error("empty expression")));
    }
    let e = p.parse_binary(0).map_err(|e| in_expression(src, e))?;
    p.skip_space();
    if p.pos < p.src.len() {
        return Err(in_expression(src, p.after_expression()));
    }
    Ok(e)
}

/// Add the context tclsh 9.0.4 puts under a refused expression.
///
/// Every one of its expression diagnostics carries the source it was reading,
/// and a bare word additionally gets the spelling hint:
///
/// ```text
/// invalid bareword "a"
/// in expression "a";
/// should be "$a" or "{a}" or "a(...)" or ...
///
/// missing operand at _@_
/// in expression "1 + _@_* 2"
/// ```
///
/// Three rules, each measured against tclsh rather than inferred:
///
/// * The source is quoted verbatim, leading and trailing space included —
///   `expr {  a  }` reports `in expression "  a  "`.
/// * `_@_` marks the position, and only for the diagnostics whose own text ends
///   in `at _@_`: the missing operand, the missing operator, and the missing
///   `":"`. `unbalanced open paren` and the bare word carry no marker.
/// * The `;` after the closing quote appears only when a third line follows,
///   which is only for a bare word.
///
/// This is the whole message a script sees, because `catch` yields these three
/// lines; the stack trace tclsh adds beyond them is not part of the result.
fn in_expression(src: &str, mut err: ParseError) -> ParseError {
    let marked = if err.msg.ends_with("at _@_") {
        let at = char_boundary(src, err.offset.min(src.len()));
        format!("{}_@_{}", &src[..at], &src[at..])
    } else {
        src.to_string()
    };

    // `invalid bareword "w"` — the only diagnostic with a hint, and the word is
    // already quoted in it, so it is read back out rather than threaded through.
    //
    // A word that opens with a radix prefix whose FIRST digit is already wrong
    // gets a guess at what the script meant appended to the hint. Measured
    // against tclsh 9.0.4 over 36 spellings, three rules:
    //
    // * Only `0b` and `0o`. `0x` and `0d` never get one — `0x_1f` and `0d_9`
    //   end at the `...`.
    // * The prefix is matched case-SENSITIVELY: `0b2` is
    //   `(invalid binary number?)` and `0B2` is the same diagnostic with nothing
    //   after the `...`.
    // * The guess is about the digit that FOLLOWS the prefix, so it appears only
    //   when that character is absent or is not a digit of the radix: `0b`,
    //   `0b2`, `0bz` and `0b_101` get it, while `0b1z` and `0b1_1z` do not —
    //   those start out as binary and go wrong later.
    let radix_guess = |w: &str| {
        let bad_first =
            |body: &str, radix: u32| body.chars().next().is_none_or(|c| !c.is_digit(radix));
        match () {
            _ if w.strip_prefix("0b").is_some_and(|b| bad_first(b, 2)) => {
                " (invalid binary number?)"
            }
            _ if w.strip_prefix("0o").is_some_and(|b| bad_first(b, 8)) => {
                " (invalid octal number?)"
            }
            _ => "",
        }
    };
    let hint = err
        .msg
        .strip_prefix("invalid bareword \"")
        .and_then(|rest| rest.strip_suffix('"'))
        .map(|w| {
            format!(
                "should be \"${w}\" or \"{{{w}}}\" or \"{w}(...)\" or ...{}",
                radix_guess(w)
            )
        });

    err.msg = match hint {
        Some(hint) => format!("{}\nin expression \"{marked}\";\n{hint}", err.msg),
        None => format!("{}\nin expression \"{marked}\"", err.msg),
    };
    err
}

/// The largest character boundary at or below `at`, so inserting the marker
/// cannot split a character. A position past one — the parser measures bytes —
/// would otherwise panic on a multi-byte operand.
fn char_boundary(src: &str, at: usize) -> usize {
    let mut at = at;
    while at > 0 && !src.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Binding powers, lowest first. Each entry is one precedence level; every
/// level is left-associative except `**`, handled in `ExprParser::parse_binary`.
///
/// Public because the reference page prints the ladder, and printing it from
/// anywhere but the table the parser binds with would let the two disagree.
pub const LEVELS: &[&[(&str, BinOp)]] = &[
    &[("||", BinOp::Or)],
    &[("&&", BinOp::And)],
    &[("|", BinOp::BitOr)],
    &[("^", BinOp::BitXor)],
    &[("&", BinOp::BitAnd)],
    &[
        ("==", BinOp::Eq),
        ("!=", BinOp::Ne),
        ("eq", BinOp::StrEq),
        ("ne", BinOp::StrNe),
        ("in", BinOp::In),
        ("ni", BinOp::Ni),
    ],
    &[
        ("<=", BinOp::Le),
        (">=", BinOp::Ge),
        ("<", BinOp::Lt),
        (">", BinOp::Gt),
        ("lt", BinOp::StrLt),
        ("gt", BinOp::StrGt),
        ("le", BinOp::StrLe),
        ("ge", BinOp::StrGe),
    ],
    &[("<<", BinOp::Shl), (">>", BinOp::Shr)],
    &[("+", BinOp::Add), ("-", BinOp::Sub)],
    &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
    &[("**", BinOp::Pow)],
];

/// A character that begins one of `expr`'s operators. `+`, `-`, `~` and `!` are
/// in the set even though they are also unary, because reaching
/// [`ExprParser::missing_operand`] means the unary reading has already been
/// taken and what is left is a binary operator with nothing on its left.
fn is_operator_char(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '~'
            | '!'
            | '&'
            | '|'
            | '^'
            | '<'
            | '>'
            | '='
            | '?'
            | ':'
            | ','
    )
}

/// A character that begins one of `expr`'s operands — a number, a variable, a
/// command substitution, a quoted or braced word, a bare word, or a
/// parenthesised subexpression.
fn starts_an_operand(c: char) -> bool {
    c.is_ascii_digit()
        || c.is_ascii_alphabetic()
        || matches!(c, '.' | '_' | '$' | '[' | '"' | '{' | '(')
}

/// Is this bare word one of `expr`'s operators spelt with letters — `eq`, `ne`,
/// `in`, `ni`, `lt`, `gt`, `le`, `ge`? Read off [`LEVELS`], so the answer cannot
/// drift from the table the parser binds with.
fn is_word_operator(name: &str) -> bool {
    LEVELS
        .iter()
        .flat_map(|level| level.iter())
        .any(|(text, _)| *text == name && text.bytes().all(|b| b.is_ascii_alphabetic()))
}

struct ExprParser<'a> {
    src: &'a str,
    pos: usize,
    /// How many `(` are open at the cursor. Read only by
    /// [`ExprParser::missing_operand`]: input that runs out inside a paren is
    /// `unbalanced open paren` in tclsh, not the missing operand.
    open_parens: usize,
    /// How many subexpressions are open at the cursor — the recursion this
    /// parser does, bounded by [`MAX_EXPR_DEPTH`].
    depth: usize,
}

impl<'a> ExprParser<'a> {
    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    /// Parse one nesting level deeper, or refuse.
    ///
    /// Every recursive call that opens a subexpression goes through here: a
    /// parenthesized operand, a function argument, both arms of a ternary, the
    /// right operand of the right-associative `**`, and a unary operator's
    /// operand. The level walk inside `ExprParser::parse_binary` does not,
    /// because it is bounded by [`LEVELS`] rather than by the input.
    ///
    /// The wording follows the shape the reference interpreter uses for its own
    /// depth refusals (`too many nested evaluations (infinite loop?)`), because
    /// there is no reference behavior to copy: tclsh's expression parser does
    /// not recurse and so has no limit to match.
    fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.depth >= MAX_EXPR_DEPTH {
            return Err(self.error("too many nested subexpressions (infinite loop?)"));
        }
        self.depth += 1;
        let parsed = parse(self);
        self.depth -= 1;
        parsed
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    fn error(&self, msg: &str) -> ParseError {
        ParseError {
            msg: msg.to_string(),
            offset: self.pos,
            line: 1,
        }
    }

    /// The whole character at the cursor, not the byte: `expr {Ü}` reports
    /// `invalid character "Ü"`, and reporting `self.src[pos]` named the lead
    /// byte of its UTF-8 encoding instead.
    fn char_here(&self) -> char {
        self.src[self.pos..]
            .chars()
            .next()
            .expect("a byte at the cursor is part of a character")
    }

    /// What tclsh 9.0.4 says when it wanted an operand and did not find one.
    ///
    /// Its expression lexer tokenises before it parses, so the diagnostic names
    /// what the *token* was rather than where the parser was, and there are four
    /// answers rather than one:
    ///
    /// ```text
    /// expr {1 + }   missing operand at _@_     input ran out
    /// expr {1 + (}  unbalanced open paren      ... with a paren still open
    /// expr {*1}     missing operand at _@_     an operator, which is no operand
    /// expr {)1}     unbalanced close paren
    /// expr {@1}     invalid character "@"      no token at all
    /// ```
    ///
    /// The `_@_` is literal: tclsh marks the position on the message's *second*
    /// line (`in expression "1 + _@_"`) and leaves the marker unexpanded on the
    /// first.
    fn missing_operand(&self) -> ParseError {
        if self.pos >= self.src.len() {
            return self.error(if self.open_parens > 0 {
                "unbalanced open paren"
            } else {
                "missing operand at _@_"
            });
        }
        match self.char_here() {
            ')' => self.error("unbalanced close paren"),
            // `=` is the one operator character that is not an operator on its
            // own, and tclsh names it as an unfinished one rather than as a
            // missing operand.
            '=' if self.bytes().get(self.pos + 1) != Some(&b'=') => {
                self.error("incomplete operator \"=\"")
            }
            c if is_operator_char(c) => self.error("missing operand at _@_"),
            c => self.error(&format!("invalid character \"{c}\"")),
        }
    }

    /// What tclsh 9.0.4 says about text left over once a complete expression has
    /// been read: two operands running together is a missing operator, and
    /// anything that is no token at all is still the invalid character.
    fn after_expression(&self) -> ParseError {
        match self.char_here() {
            ')' => self.error("unbalanced close paren"),
            c if is_operator_char(c) => self.error("missing operand at _@_"),
            // A word left over is named, where a second *number* or variable is
            // only a missing operator: `expr {1 x}` is `invalid bareword "x"`
            // and `expr {1 1}` is `missing operator at _@_`. An operator spelt
            // with letters never reaches here — `eq` and `in` matched already.
            c if c.is_ascii_alphabetic() => {
                let b = self.bytes();
                let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
                // The word tclsh names is the whole RUN of word characters the
                // leftover sits in, not just the part left over: `expr {1a}` is
                // `invalid bareword "1a"` and `expr {007z}` names `007z`, where
                // starting at the current position named `a` and `z`. The run
                // stops at anything that is not a word character, which keeps
                // `expr {1+2a}` at `2a` and `expr {1 x}` at `x`.
                //
                // A run a `.` introduces is the tail of a decimal number, and
                // there tclsh names the leftover ALONE: `expr {1.5a}` is `a`,
                // not `5a`, and so are `.5a`, `1.5e3a`, `12.5x` and `2.a`. That
                // is the one place the run and the word disagree — measured
                // across 53 spellings, every other one is the run.
                let mut lo = self.pos;
                while lo > 0 && is_word(b[lo - 1]) {
                    lo -= 1;
                }
                if lo > 0 && b[lo - 1] == b'.' {
                    lo = self.pos;
                }
                let mut end = self.pos;
                while end < b.len() && is_word(b[end]) {
                    end += 1;
                }
                let word = &self.src[lo..end];
                // A boolean word is an operand, so two of them running together
                // is the missing operator between them: `expr {1 y}` is
                // `missing operator at _@_` where `expr {1 x}` names `x`.
                if crate::runtime::boolean_word(word).is_some() {
                    return self.error("missing operator at _@_");
                }
                self.error(&format!("invalid bareword {word:?}"))
            }
            c if starts_an_operand(c) => self.error("missing operator at _@_"),
            c => self.error(&format!("invalid character \"{c}\"")),
        }
    }

    /// Whitespace, and the comments `expr` allows within it.
    ///
    /// A `#` outside a quoted or braced operand starts a comment that runs to
    /// the end of the line, exactly as in the command language: `expr {1 + # c
    /// 2}` is 3 in tclsh 9.0.4, and `expr {#1}` is `empty expression` rather
    /// than anything about the `#`.
    fn skip_space(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// Does an operator token sit at the cursor? Word operators (`eq`, `in`)
    /// must not be a prefix of a longer bare word, so `$income` is not `in`.
    fn match_op(&self, op: &str) -> bool {
        if !self.src[self.pos..].starts_with(op) {
            return false;
        }
        if op.as_bytes()[0].is_ascii_alphabetic() {
            match self.bytes().get(self.pos + op.len()) {
                Some(b) if b.is_ascii_alphanumeric() || *b == b'_' => return false,
                _ => {}
            }
        }
        // `**` must win over `*`, `<=` over `<`; the level tables are ordered
        // so the longer token is tried first within a level, but `*` and `**`
        // sit on different levels, so guard here too.
        if op == "*" && self.src[self.pos..].starts_with("**") {
            return false;
        }
        if op == "<" && self.src[self.pos..].starts_with("<<") {
            return false;
        }
        if op == ">" && self.src[self.pos..].starts_with(">>") {
            return false;
        }
        if (op == "&" && self.src[self.pos..].starts_with("&&"))
            || (op == "|" && self.src[self.pos..].starts_with("||"))
        {
            return false;
        }
        true
    }

    fn parse_binary(&mut self, level: usize) -> Result<Expr, ParseError> {
        if level >= LEVELS.len() {
            return self.parse_unary();
        }
        let mut lhs = self.parse_binary(level + 1)?;
        loop {
            self.skip_space();
            let Some(&(tok, op)) = LEVELS[level].iter().find(|(tok, _)| self.match_op(tok)) else {
                break;
            };
            self.pos += tok.len();
            self.skip_space();
            // Exponentiation is the one right-associative level.
            let rhs = if op == BinOp::Pow {
                self.nested(|p| p.parse_binary(level))?
            } else {
                self.parse_binary(level + 1)?
            };
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        // The ternary sits below every binary level.
        if level == 0 {
            self.skip_space();
            if self.peek() == Some(b'?') {
                self.pos += 1;
                self.skip_space();
                let then = self.nested(|p| p.parse_binary(0))?;
                self.skip_space();
                if self.peek() != Some(b':') {
                    return Err(self.error("missing operator \":\" at _@_"));
                }
                self.pos += 1;
                self.skip_space();
                let other = self.nested(|p| p.parse_binary(0))?;
                lhs = Expr::Ternary(Box::new(lhs), Box::new(then), Box::new(other));
            }
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.skip_space();
        let op = match self.peek() {
            Some(b'-') => Some(UnOp::Neg),
            Some(b'+') => Some(UnOp::Plus),
            Some(b'~') => Some(UnOp::BitNot),
            Some(b'!') if self.bytes().get(self.pos + 1) != Some(&b'=') => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let operand = self.nested(|p| p.parse_unary())?;
            return Ok(Expr::Unary(op, Box::new(operand)));
        }
        self.parse_operand()
    }

    fn parse_operand(&mut self) -> Result<Expr, ParseError> {
        self.skip_space();
        match self.peek() {
            None => Err(self.missing_operand()),
            Some(b'(') => {
                self.pos += 1;
                self.open_parens += 1;
                let e = self.nested(|p| p.parse_binary(0))?;
                self.skip_space();
                if self.peek() != Some(b')') {
                    return Err(self.error("unbalanced open paren"));
                }
                self.pos += 1;
                self.open_parens -= 1;
                Ok(e)
            }
            Some(b'$') => {
                let Some((part, next)) = parser::substitution_at(self.src, self.pos)? else {
                    return Err(self.error("invalid character \"$\""));
                };
                self.pos = next;
                Ok(Expr::Subst(vec![part]))
            }
            Some(b'[') => {
                let (script, next) = parser::command_at(self.src, self.pos)?;
                self.pos = next;
                Ok(Expr::Subst(vec![Part::Script(script)]))
            }
            Some(b'"') => {
                let (parts, next) = parser::quoted_at(self.src, self.pos)?;
                self.pos = next;
                Ok(Expr::Subst(parts))
            }
            Some(b'{') => {
                let (text, next) = parser::braced_at(self.src, self.pos)?;
                self.pos = next;
                Ok(Expr::Subst(vec![Part::Lit(text)]))
            }
            Some(b) if b.is_ascii_digit() || b == b'.' => self.parse_number(),
            // A leading `_` is not a word here. tclsh's lexer starts a bare word
            // at a letter only, so `expr {_1}` is `invalid character "_"` rather
            // than a bare word named `_1`, and the arm below is what says so.
            Some(b) if b.is_ascii_alphabetic() => self.parse_call(),
            // Neither an operand nor a unary operator. `missing_operand` decides
            // between `missing operand at _@_` (a binary operator with nothing
            // to its left, `expr {*1}`) and `invalid character "@"` (no token at
            // all), which is the distinction tclsh's lexer makes.
            Some(_) => Err(self.missing_operand()),
        }
    }

    /// Scan one run of digits, in which `_` may appear only *between* two of
    /// them.
    ///
    /// Tcl 9 reads `1_000_000` as a million and even `1__0` as ten, but the
    /// separator has to have a digit on each side: `0x_10`, `1_`, `1e_10` and
    /// `1_.5` are all refused, each as a bare word rather than as a number.
    /// Returns the end of the run, or the offset of the `_` that broke the rule.
    fn digit_run(&self, start: usize) -> Result<usize, usize> {
        let b = self.bytes();
        let mut end = start;
        while end < b.len() && (b[end].is_ascii_digit() || b[end] == b'_') {
            end += 1;
        }
        if end > start && b[start] == b'_' {
            return Err(start);
        }
        if end > start && b[end - 1] == b'_' {
            return Err(end - 1);
        }
        Ok(end)
    }

    /// What tclsh says about a `_` that is not between two digits.
    ///
    /// It names the whole word the separator sits in, which is not always the
    /// number that was being read: `1.5_` is `invalid bareword "5_"`, because
    /// the run stops at the `.`, while `0x_10` names itself entire. A literal
    /// that never got past its own leading `.` is the exception — there is no
    /// word to name, so the `.` is reported as the invalid character it is.
    fn separator_error(&self, at: usize, dot_led: bool) -> ParseError {
        if dot_led {
            return self.error("invalid character \".\"");
        }
        let b = self.bytes();
        let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut lo = at;
        while lo > 0 && is_word(b[lo - 1]) {
            lo -= 1;
        }
        let mut hi = at;
        while hi < b.len() && is_word(b[hi]) {
            hi += 1;
        }
        self.error(&format!("invalid bareword {:?}", &self.src[lo..hi]))
    }

    /// Tcl integer literals carry the C-ish radix prefixes; anything with a
    /// decimal point or exponent is a double.
    fn parse_number(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let rest = &self.src[start..];

        if let Some(radix_body) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            return self.radix_literal(radix_body, 16, 2);
        }
        if let Some(radix_body) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            return self.radix_literal(radix_body, 8, 2);
        }
        if let Some(radix_body) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            return self.radix_literal(radix_body, 2, 2);
        }
        if let Some(radix_body) = rest.strip_prefix("0d").or_else(|| rest.strip_prefix("0D")) {
            return self.radix_literal(radix_body, 10, 2);
        }

        let mut end = start;
        let b = self.bytes();
        // Each run of digits — the integer part, the fraction, the exponent —
        // may carry `_` between two of its digits and nowhere else. A number
        // whose leading `.` never resolves is not a number at all, and tclsh
        // names the `.`: `expr {.5_}` is `invalid character "."`.
        let dot_led = b[start] == b'.';
        end = self
            .digit_run(end)
            .map_err(|at| self.separator_error(at, dot_led))?;
        let mut is_float = false;
        if end < b.len() && b[end] == b'.' {
            is_float = true;
            end += 1;
            // A separator immediately after the point is the point failing to
            // lead anywhere, which is how tclsh reports it: `1._5` names the
            // `.`, where `1.5_` names the word the trailing `_` sits in.
            let fraction = end;
            end = self
                .digit_run(end)
                .map_err(|at| self.separator_error(at, dot_led || at == fraction))?;
        }
        if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
            let mut probe = end + 1;
            if probe < b.len() && (b[probe] == b'+' || b[probe] == b'-') {
                probe += 1;
            }
            // A `_` here is an exponent that was *meant*, so it is committed to
            // and reported, rather than left behind as trailing text: tclsh
            // answers `invalid bareword "1e_10"`, not a missing operator.
            if probe < b.len() && (b[probe].is_ascii_digit() || b[probe] == b'_') {
                is_float = true;
                end = self
                    .digit_run(probe)
                    .map_err(|at| self.separator_error(at, dot_led))?;
            }
        }

        let text = &self.src[start..end];
        self.pos = end;
        // Parsed without the separators, remembered with them: `1_0` is the
        // number ten and the string "1_0".
        let bare: String = text.chars().filter(|c| *c != '_').collect();
        if is_float {
            bare.parse::<f64>()
                .map(|v| Expr::Float(v, text.into()))
                // A lone `.`, or one with nothing a number can use after it, is
                // not a malformed number to tclsh — it is a character that
                // begins no token at all, and it says so.
                .map_err(|_| {
                    if dot_led {
                        self.error("invalid character \".\"")
                    } else {
                        self.error(&format!("invalid floating-point number {text:?}"))
                    }
                })
        } else {
            // Out of `i64` range: Tcl promotes to a bignum, which this frontend
            // does not have. The literal stays its own text, and every *operation*
            // on it is refused by the numeric hook — which is where the overflow
            // is reported now that `runtime::parse_number` no longer hands the
            // spelling to the double parser and answers `1e+20`.
            //
            // Deliberately not refused here. A decimal spelling this large is
            // exactly what tclsh prints for it, so `expr {99999999999999999999}`
            // and `puts 99999999999999999999` are both right as text; refusing at
            // compile time would take down whole scripts that only ever print the
            // value or never reach it at all.
            bare.parse::<i64>()
                .map(|v| Expr::Int(v, text.into()))
                .or_else(|_| Ok(Expr::Subst(vec![Part::Lit(text.to_string())])))
        }
    }

    fn radix_literal(
        &mut self,
        body: &str,
        radix: u32,
        prefix_len: usize,
    ) -> Result<Expr, ParseError> {
        let written: String = body
            .chars()
            .take_while(|c| c.is_digit(radix) || *c == '_')
            .collect();
        // The separator rule is the prefix's too, and the prefix is not a digit:
        // `0x1_0` is sixteen, `0x_10` and `0x1_` are bare words. The offset is
        // the offending `_` itself, so the message names the whole spelling.
        if written.starts_with('_') {
            return Err(self.separator_error(self.pos + prefix_len, false));
        }
        if written.ends_with('_') {
            return Err(self.separator_error(self.pos + prefix_len + written.len() - 1, false));
        }
        let digits: String = written.chars().filter(|c| *c != '_').collect();
        if digits.is_empty() {
            // A radix prefix with no digits is not a number, so tclsh's lexer
            // reads it as the bare word it looks like: `expr {0x}` is `invalid
            // bareword "0x"`. The word runs to the end of the alphanumeric run,
            // so `expr {0xg}` names `0xg` and not just the prefix.
            let word: String = self.src[self.pos..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            return Err(self.error(&format!("invalid bareword {word:?}")));
        }
        // The spelling, prefix included: `0x10` and `16` are the same number and
        // different strings, and `eq` compares the strings.
        let text: Box<str> =
            format!("{}{written}", &self.src[self.pos..self.pos + prefix_len]).into();
        // The written length, not the parsed one: `0x1_0` is five characters and
        // two digits, and advancing by the digits alone left the `_0` behind as
        // "extra characters after expression".
        self.pos += prefix_len + written.len();
        Ok(match i64::from_str_radix(&digits, radix) {
            Ok(v) => Expr::Int(v, text),
            // Wider than an `i64`, which is a bignum and not an error. It takes
            // the same shape a wide decimal literal takes — the text as its own
            // operand — and for the same reason: the spelling is what `eq`
            // compares, and tclsh agrees, answering 1 for
            // `expr {0x10000000000000000 eq "0x10000000000000000"}` and 0 for
            // the same against `"18446744073709551616"`. Arithmetic reads the
            // radix back through `runtime::parse_number`, and a result that is
            // printed rather than computed on is canonicalised to the decimal
            // `18446744073709551616` by `ext::CANON`.
            Err(_) => Expr::Subst(vec![Part::Lit(text.to_string())]),
        })
    }

    fn parse_call(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let b = self.bytes();
        let mut end = start;
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_' || b[end] == b':')
        {
            end += 1;
        }
        let name = self.src[start..end].to_string();
        // `inf`, `infinity` and `nan` are floating-point *literals* to `expr(n)`,
        // in any case, and are not function names: `expr {inf > 1}` is 1 and
        // `expr {nan == nan}` is 0. Nothing else spelt with letters is a
        // literal, so the set is exactly what `f64::from_str` takes — which is
        // these three and nothing more (`infinit`, `infx` and `nano` are all
        // `invalid bareword` in tclsh 9.0.4, and so are they here).
        //
        // Tested before the `(` below, because there is no `inf(...)`.
        if let Ok(f) = name.parse::<f64>() {
            self.pos = end;
            // The spelling is carried like every other numeric literal's, so
            // `expr {inf eq "Inf"}` compares what the script wrote rather than
            // what the number prints as.
            return Ok(Expr::Float(f, name.into()));
        }
        self.pos = end;
        self.skip_space();
        if self.peek() != Some(b'(') {
            // A word operator with nothing on its left is a missing operand,
            // not a bare word: tclsh's lexer has already read `expr {in}` as the
            // operator it is.
            if is_word_operator(&name) {
                self.pos = start;
                return Err(self.error("missing operand at _@_"));
            }
            // A boolean word is an *operand* in `expr(n)`, not a bad bareword:
            // `expr {yes}` is `yes`, `expr {true && false}` is 0, `expr {!yes}`
            // is 0 and `expr {on ? 1 : 2}` is 1. It carries its own spelling, so
            // it behaves exactly as the quoted form does — `eq` compares the
            // text, and arithmetic refuses it through the numeric rule with
            // tclsh's own wording (`cannot use non-numeric string "yes" as left
            // operand of "+"`) rather than through the parser.
            //
            // The table is `ParseBoolean`'s, shared with the condition rule
            // rather than copied: a unique prefix of true/false/yes/no/on/off in
            // any case, which is why `o` is still a bareword — `on` and `off`
            // both start with it.
            if crate::runtime::boolean_word(&name).is_some() {
                return Ok(Expr::Subst(vec![Part::Lit(name)]));
            }
            return Err(self.error(&format!("invalid bareword {name:?}")));
        }
        self.pos += 1;
        // A call's paren counts like a grouping one, so running out of input
        // inside the argument list is `unbalanced open paren` as it is for
        // `expr {1 + (}` — tclsh reports that for `expr {sin(}`, not the
        // missing operand the empty argument would otherwise be.
        self.open_parens += 1;
        let mut args = Vec::new();
        self.skip_space();
        if self.peek() == Some(b')') {
            self.pos += 1;
            self.open_parens -= 1;
            return Ok(Expr::Call(name, args));
        }
        // An argument list that opens with a comma has no first argument, which
        // tclsh names rather than reporting the comma as a stray operator.
        if self.peek() == Some(b',') {
            return Err(self.error("missing function argument at _@_"));
        }
        loop {
            args.push(self.nested(|p| p.parse_binary(0))?);
            self.skip_space();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_space();
                    // A comma promises another argument, so its absence is that
                    // argument's — `expr {sin(1,}` and `expr {sin(1,)}` are both
                    // `missing function argument at _@_` in tclsh 9.0.4, where
                    // running out with no comma pending (`expr {sin(1}`) is the
                    // unbalanced paren below. A comma followed by an *operator*
                    // is neither: `expr {max(1,,2}` is the ordinary missing
                    // operand, which the operand parser reports on its own.
                    if self.peek().is_none() || self.peek() == Some(b')') {
                        return Err(self.error("missing function argument at _@_"));
                    }
                }
                Some(b')') => {
                    self.pos += 1;
                    self.open_parens -= 1;
                    return Ok(Expr::Call(name, args));
                }
                // Input ran out with the call's paren still open, which is the
                // same diagnostic a grouping paren gets; anything else is a
                // second operand with no operator between them.
                None => return Err(self.error("unbalanced open paren")),
                Some(_) => return Err(self.error("missing operator at _@_")),
            }
        }
    }
}
