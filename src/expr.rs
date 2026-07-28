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
    Int(i64),
    Float(f64),
    /// An operand built from literal text and substitutions: a quoted or braced
    /// string, `$var`, `$arr(i)`, or `[script]`.
    Subst(Vec<Part>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A math function call — parsed here, lowered in a later phase.
    Call(String, Vec<Expr>),
}

/// Parse a complete expression.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut p = ExprParser { src, pos: 0 };
    p.skip_space();
    let e = p.parse_binary(0)?;
    p.skip_space();
    if p.pos < p.src.len() {
        return Err(p.error(&format!(
            "extra characters after expression: {:?}",
            &p.src[p.pos..]
        )));
    }
    Ok(e)
}

/// Binding powers, lowest first. Each entry is one precedence level; every
/// level is left-associative except `**`, handled in [`ExprParser::parse_binary`].
const LEVELS: &[&[(&str, BinOp)]] = &[
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

struct ExprParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> ExprParser<'a> {
    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
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

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
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
                self.parse_binary(level)?
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
                let then = self.parse_binary(0)?;
                self.skip_space();
                if self.peek() != Some(b':') {
                    return Err(self.error("missing : in ternary"));
                }
                self.pos += 1;
                self.skip_space();
                let other = self.parse_binary(0)?;
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
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary(op, Box::new(operand)));
        }
        self.parse_operand()
    }

    fn parse_operand(&mut self) -> Result<Expr, ParseError> {
        self.skip_space();
        match self.peek() {
            None => Err(self.error("premature end of expression")),
            Some(b'(') => {
                self.pos += 1;
                let e = self.parse_binary(0)?;
                self.skip_space();
                if self.peek() != Some(b')') {
                    return Err(self.error("missing close-paren"));
                }
                self.pos += 1;
                Ok(e)
            }
            Some(b'$') => {
                let Some((part, next)) = parser::substitution_at(self.src, self.pos)? else {
                    return Err(self.error("invalid $ in expression"));
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
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => self.parse_call(),
            Some(b) => Err(self.error(&format!(
                "unexpected character {:?} in expression",
                b as char
            ))),
        }
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

        let mut end = start;
        let b = self.bytes();
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        let mut is_float = false;
        if end < b.len() && b[end] == b'.' {
            is_float = true;
            end += 1;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
        }
        if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
            let mut probe = end + 1;
            if probe < b.len() && (b[probe] == b'+' || b[probe] == b'-') {
                probe += 1;
            }
            if probe < b.len() && b[probe].is_ascii_digit() {
                is_float = true;
                end = probe;
                while end < b.len() && b[end].is_ascii_digit() {
                    end += 1;
                }
            }
        }

        let text = &self.src[start..end];
        self.pos = end;
        if is_float {
            text.parse::<f64>()
                .map(Expr::Float)
                .map_err(|_| self.error(&format!("invalid floating-point number {text:?}")))
        } else {
            text.parse::<i64>()
                .map(Expr::Int)
                // Out of i64 range: Tcl promotes to a bignum, which this
                // frontend does not have yet, so keep the text and let the
                // numeric hook report it rather than silently wrapping.
                .or_else(|_| Ok(Expr::Subst(vec![Part::Lit(text.to_string())])))
        }
    }

    fn radix_literal(
        &mut self,
        body: &str,
        radix: u32,
        prefix_len: usize,
    ) -> Result<Expr, ParseError> {
        let digits: String = body
            .chars()
            .take_while(|c| c.is_digit(radix) || *c == '_')
            .filter(|c| *c != '_')
            .collect();
        if digits.is_empty() {
            return Err(self.error("missing digits after radix prefix"));
        }
        self.pos += prefix_len + digits.len();
        i64::from_str_radix(&digits, radix)
            .map(Expr::Int)
            .map_err(|_| self.error("integer literal out of range"))
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
        self.pos = end;
        self.skip_space();
        if self.peek() != Some(b'(') {
            return Err(self.error(&format!("invalid bare word {name:?} in expression")));
        }
        self.pos += 1;
        let mut args = Vec::new();
        self.skip_space();
        if self.peek() == Some(b')') {
            self.pos += 1;
            return Ok(Expr::Call(name, args));
        }
        loop {
            args.push(self.parse_binary(0)?);
            self.skip_space();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    return Ok(Expr::Call(name, args));
                }
                _ => return Err(self.error("missing close-paren in function call")),
            }
        }
    }
}
