//! Translation of a Tcl Advanced Regular Expression into `regex` syntax.
//!
//! The lexer is a port of Henry Spencer's `generic/regc_lex.c` (`prefixes`,
//! `next`, `lexescape`, `lexdigits`, `skip`) and the grammar checks of
//! `generic/regcomp.c` (`parseqatom`, `brackpart`, `scannum`) as Tcl 9.0.4 ships
//! them, so a pattern is read token for token the way `regcomp` reads it and
//! every defect is reported with `regcomp`'s own `REG_*` message
//! (`generic/regerrs.h`) at the point `regcomp` would have stopped. What comes
//! out is a `regex` pattern in which nothing is left for the two syntaxes to
//! disagree about:
//!
//! * every character an escape or a bracket expression names is written as a
//!   `\x{…}` literal, so `regex`'s own escapes (`\<`, `\pL`, `\z`, `&&` and
//!   `--` in a class) never come into play;
//! * `[:class:]`, `\d`, `\s` and `\w` are expanded from the tables of
//!   `generic/regc_locale.c` ([`crate::regc_locale`]), which is what Tcl
//!   matches them against — not `regex`'s Unicode tables;
//! * `-expanded` white space and `#` comments are removed here, under ARE's
//!   rule (outside brackets only), rather than handed to `(?x)`, which also
//!   strips white space inside a bracket expression;
//! * `-linestop` keeps a newline out of `[^…]` as well as out of `.`, which is
//!   `REG_NLSTOP`'s definition and not what `(?-s)` does.
//!
//! `(?e)` selects POSIX ERE, which is this grammar with the ARE extensions
//! off (`REG_ADVF`). Constructs a finite-automaton matcher cannot express are
//! refused by name: back-references and look-ahead. So is the BRE syntax
//! `(?b)` selects, a separate grammar (`brenext`) this module does not carry.

use crate::regc_locale as loc;

/// The compile flags a pattern starts from: the command's switches, which
/// embedded options and directors may then change.
#[derive(Clone, Copy, Default)]
pub(crate) struct Flags {
    pub icase: bool,
    pub expanded: bool,
    /// `REG_NLSTOP`: `.` and `[^…]` do not match a newline.
    pub nlstop: bool,
    /// `REG_NLANCH`: `^` and `$` match at a newline.
    pub nlanch: bool,
}

/// `REG_*` messages, as `regerror` words them.
const EESCAPE: &str = "invalid escape \\ sequence";
const BADOPT: &str = "invalid embedded option";
const BADRPT: &str = "invalid quantifier operand";
const BADBR: &str = "invalid repetition count(s)";
const EBRACE: &str = "braces {} not balanced";
const EBRACK: &str = "brackets [] not balanced";
const EPAREN: &str = "parentheses () not balanced";
const ERANGE: &str = "invalid character range";
const ECTYPE: &str = "invalid character class";
const ECOLLATE: &str = "invalid collating element";
const ESUBREG: &str = "invalid backreference number";
const BADPAT: &str = "invalid regexp (reg version 0.8)";

/// `_POSIX2_RE_DUP_MAX`, the largest repetition count (`regguts.h`).
const DUPMAX: u32 = 255;

fn reg(msg: &str) -> String {
    format!("cannot compile regular expression pattern: {msg}")
}

fn refusal(what: &str) -> String {
    format!("{what} is not supported yet: the regular expression engine here matches in linear time, which back-references and look-around cannot")
}

/// A `regex` literal for one character, safe inside and outside a class.
fn lit(out: &mut String, c: u32) {
    match char::from_u32(c) {
        Some(ch) if ch.is_ascii_alphanumeric() || !ch.is_ascii() => out.push(ch),
        Some(_) => out.push_str(&format!("\\x{{{c:X}}}")),
        // A surrogate: a Tcl string can hold one, a Rust string cannot, so
        // nothing a script can pass here ever contains it.
        None => out.push_str("[^\\x{0}-\\x{10FFFF}]"),
    }
}

/// `allcases`: a character's lowercase, uppercase and titlecase, which is what
/// a case-independent match accepts for it. Tcl folds the *pattern* this way
/// and leaves the subject alone, so this is not `regex`'s `(?i)`, whose case
/// folding orbits are wider (`k` also matches U+212A KELVIN SIGN there).
fn allcases(c: u32) -> Vec<u32> {
    let Some(ch) = char::from_u32(c) else {
        return vec![c];
    };
    use crate::cmd_string::{lower, title, upper};
    let (lc, uc, tc) = (lower(ch) as u32, upper(ch) as u32, title(ch) as u32);
    let mut v = Vec::with_capacity(3);
    if tc != uc {
        v.push(tc);
    }
    v.push(lc);
    if lc != uc {
        v.push(uc);
    }
    v
}

/// One character as a `regex` atom: a literal, or under `REG_ICASE` the class
/// of its `allcases` (`onechr`).
fn onechr(out: &mut String, c: u32, icase: bool) {
    if !icase {
        return lit(out, c);
    }
    let cases = allcases(c);
    if cases.len() == 1 {
        return lit(out, cases[0]);
    }
    out.push('[');
    for k in cases {
        lit(out, k);
    }
    out.push(']');
}

/// `range`: the class items for `a`–`b`, plus, under `REG_ICASE`, every
/// member's lowercase, uppercase and titlecase.
fn range(out: &mut String, a: u32, b: u32, icase: bool) {
    lit(out, a);
    if a != b {
        out.push('-');
        lit(out, b);
    }
    if !icase {
        return;
    }
    let mut extra: Vec<u32> = Vec::new();
    for c in a..=b {
        let Some(ch) = char::from_u32(c) else {
            continue;
        };
        use crate::cmd_string::{lower, title, upper};
        for k in [lower(ch) as u32, upper(ch) as u32, title(ch) as u32] {
            if !(a..=b).contains(&k) {
                extra.push(k);
            }
        }
    }
    extra.sort_unstable();
    extra.dedup();
    for k in extra {
        lit(out, k);
    }
}

/// The class members `regc_locale.c`'s `cclass` supplies, as `regex` class
/// items (no surrounding brackets).
fn class_items(name: &str, icase: bool, out: &mut String) -> Result<(), String> {
    // `cclass`: lower and upper become alnum when matching case-independently.
    let name = match name {
        "lower" | "upper" if icase => "alnum",
        other => other,
    };
    let ranges = |out: &mut String, rs: &[(u32, u32)]| {
        for &(a, b) in rs {
            lit(out, a);
            out.push('-');
            lit(out, b);
        }
    };
    let chars = |out: &mut String, cs: &[u32]| {
        for &c in cs {
            lit(out, c);
        }
    };
    match name {
        "alnum" => {
            chars(out, loc::ALPHA_CHARS);
            ranges(out, loc::ALPHA_RANGES);
            ranges(out, loc::DIGIT_RANGES);
        }
        "alpha" => {
            ranges(out, loc::ALPHA_RANGES);
            chars(out, loc::ALPHA_CHARS);
        }
        "ascii" => ranges(out, &[(0, 0x7F)]),
        "blank" => chars(out, &[0x09, 0x20]),
        "cntrl" => {
            ranges(out, loc::CONTROL_RANGES);
            chars(out, loc::CONTROL_CHARS);
        }
        "digit" => ranges(out, loc::DIGIT_RANGES),
        "punct" => {
            ranges(out, loc::PUNCT_RANGES);
            chars(out, loc::PUNCT_CHARS);
        }
        "xdigit" => ranges(out, &[(0x30, 0x39), (0x61, 0x66), (0x41, 0x46)]),
        "space" => {
            ranges(out, loc::SPACE_RANGES);
            chars(out, loc::SPACE_CHARS);
        }
        "lower" => {
            ranges(out, loc::LOWER_RANGES);
            chars(out, loc::LOWER_CHARS);
        }
        "upper" => {
            ranges(out, loc::UPPER_RANGES);
            chars(out, loc::UPPER_CHARS);
        }
        "print" => {
            // `CC_PRINT` skips the first space range (`\t`–`\r`).
            ranges(out, &loc::SPACE_RANGES[1..]);
            chars(out, loc::SPACE_CHARS);
            ranges(out, loc::GRAPH_RANGES);
            chars(out, loc::GRAPH_CHARS);
        }
        "graph" => {
            ranges(out, loc::GRAPH_RANGES);
            chars(out, loc::GRAPH_CHARS);
        }
        _ => return Err(reg(ECTYPE)),
    }
    Ok(())
}

/// `element`: a collating-element name, one character or a `cnames` entry.
fn element(name: &[char]) -> Result<u32, String> {
    if let [c] = name {
        return Ok(*c as u32);
    }
    let name: String = name.iter().collect();
    loc::CNAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, c)| c)
        .ok_or_else(|| reg(ECOLLATE))
}

/// `iscspace`, which is `Tcl_UniCharIsSpace`.
fn is_space(c: char) -> bool {
    if (c as u32) < 0x80 {
        matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
    } else {
        c.is_whitespace() || matches!(c, '\u{180E}' | '\u{200B}' | '\u{2060}' | '\u{FEFF}')
    }
}

/// What `lexescape` made of one escape.
enum Escape {
    Plain(u32),
    /// `\d \D \s \S \w \W`, by letter.
    Class(char),
    /// `\A \Z \m \M \y \Y`, by letter.
    Constraint(char),
    Backref(u32),
}

/// Where the parser is relative to the last atom, for `parseqatom`'s rule that
/// a quantifier needs an atom to apply to.
#[derive(PartialEq)]
enum Last {
    /// Start of a branch, after a constraint, or after a quantifier.
    Nothing,
    Atom,
}

struct Lexer {
    s: Vec<char>,
    i: usize,
    f: Flags,
    /// `REG_ADVF`: ARE extensions on. Off, the syntax is POSIX ERE: an escape
    /// is the next character, a `\` in brackets is ordinary, and there are no
    /// non-greedy quantifiers and no `(?` groups.
    advf: bool,
    /// Capturing groups opened so far (`v->nsubexp`).
    nsub: u32,
    /// Capturing groups whose `)` has been read (`v->subs[n] != NULL`).
    closed: Vec<u32>,
    out: String,
    /// Whether the pattern looks behind the place a search starts: see
    /// [`Translated::left_context`].
    left: bool,
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }

    /// `skip`: white space and `#` comments, in expanded syntax.
    fn skip(&mut self) {
        if !self.f.expanded {
            return;
        }
        loop {
            while self.peek(0).is_some_and(is_space) {
                self.i += 1;
            }
            if self.peek(0) != Some('#') {
                break;
            }
            while self.peek(0).is_some_and(|c| c != '\n') {
                self.i += 1;
            }
        }
    }

    /// `lexdigits`.
    fn digits(&mut self, base: u32, min: usize, max: usize) -> Result<u32, String> {
        let mut n: u32 = 0;
        let mut len = 0;
        while len < max && !self.at_end() {
            if n > 0x10FFF {
                break;
            }
            let Some(d) = self
                .peek(0)
                .and_then(|c| c.to_digit(16))
                .filter(|&d| d < base)
            else {
                break;
            };
            self.i += 1;
            n = n * base + d;
            len += 1;
        }
        if len < min {
            return Err(reg(EESCAPE));
        }
        Ok(n)
    }

    /// `lexescape`, with the backslash already consumed.
    fn escape(&mut self) -> Result<Escape, String> {
        let Some(c) = self.peek(0) else {
            return Err(reg(EESCAPE));
        };
        self.i += 1;
        if !self.advf || !c.is_alphanumeric() {
            return Ok(Escape::Plain(c as u32));
        }
        Ok(match c {
            'a' => Escape::Plain(0x07),
            'b' => Escape::Plain(0x08),
            'B' => Escape::Plain('\\' as u32),
            'c' => {
                let Some(n) = self.peek(0) else {
                    return Err(reg(EESCAPE));
                };
                self.i += 1;
                Escape::Plain(n as u32 & 0o37)
            }
            'e' => Escape::Plain(0x1B),
            'f' => Escape::Plain(0x0C),
            'n' => Escape::Plain(0x0A),
            'r' => Escape::Plain(0x0D),
            't' => Escape::Plain(0x09),
            'v' => Escape::Plain(0x0B),
            'u' => Escape::Plain(self.digits(16, 1, 4)?),
            'U' => Escape::Plain(self.digits(16, 1, 8)?),
            'x' => Escape::Plain(self.digits(16, 1, 2)?),
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => Escape::Class(c),
            'A' | 'Z' | 'y' | 'Y' => Escape::Constraint(c),
            'm' => Escape::Constraint('m'),
            'M' => Escape::Constraint('M'),
            '1'..='9' => {
                let save = self.i;
                self.i -= 1;
                let n = self.digits(10, 1, 255)?;
                // "Ugly heuristic": one digit, or a number no larger than the
                // groups opened so far, is a back-reference.
                if self.i == save || (n > 0 && n <= self.nsub) {
                    return Ok(Escape::Backref(n));
                }
                self.i = save;
                self.octal()?
            }
            '0' => self.octal()?,
            _ => return Err(reg(EESCAPE)),
        })
    }

    /// The octal tail of `lexescape`, with the first digit already consumed.
    fn octal(&mut self) -> Result<Escape, String> {
        self.i -= 1;
        let mut n = self.digits(8, 1, 3)?;
        if n > 0xFF {
            self.i -= 1;
            n >>= 3;
        }
        Ok(Escape::Plain(n))
    }

    /// The `regex` form of a `\d`-style class outside a bracket expression,
    /// which `next` nests as `[[:digit:]]`, `[^[:digit:]]` and so on.
    fn class_escape(&mut self, c: char) -> Result<(), String> {
        let (negated, name) = match c {
            'd' => (false, "digit"),
            'D' => (true, "digit"),
            's' => (false, "space"),
            'S' => (true, "space"),
            'w' => (false, "alnum"),
            _ => (true, "alnum"),
        };
        let mut items = String::new();
        class_items(name, self.f.icase, &mut items)?;
        if name == "alnum" {
            items.push('_');
        }
        self.push_class(negated, &items);
        Ok(())
    }

    /// Emit a class; a negated one keeps a newline out under `REG_NLSTOP`.
    fn push_class(&mut self, negated: bool, items: &str) {
        self.out.push('[');
        if negated {
            self.out.push('^');
            if self.f.nlstop {
                self.out.push_str("\\n");
            }
        }
        self.out.push_str(items);
        self.out.push(']');
    }

    /// The bracket expression after its `[`: `bracket` / `cbracket` with
    /// `brackpart` for each item and the `L_BRACK` lexer underneath.
    fn bracket(&mut self) -> Result<(), String> {
        let negated = self.peek(0) == Some('^');
        if negated {
            self.i += 1;
        }
        let mut items = String::new();
        // `LASTTYPE('[')`: nothing read since the opening bracket.
        let mut first = true;
        loop {
            let start = self.item(first)?;
            first = false;
            let start = match start {
                Item::End => break,
                Item::Range => return Err(reg(ERANGE)),
                Item::Set(set) => {
                    items.push_str(&set);
                    continue;
                }
                Item::Elem(c) => c,
            };
            // A range, when a `-` follows that is not the last thing before `]`.
            if self.peek(0) == Some('-') && self.peek(1) != Some(']') {
                if self.peek(1).is_none() {
                    return Err(reg(EBRACK));
                }
                self.i += 1;
                let end = match self.item(false)? {
                    Item::Elem(c) => c,
                    Item::Range => '-' as u32,
                    Item::End | Item::Set(_) => return Err(reg(ERANGE)),
                };
                if start > end {
                    return Err(reg(ERANGE));
                }
                range(&mut items, start, end, self.f.icase);
            } else {
                range(&mut items, start, start, self.f.icase);
            }
        }
        self.push_class(negated, &items);
        Ok(())
    }

    /// One `L_BRACK` token, read as `brackpart` sees it.
    fn item(&mut self, first: bool) -> Result<Item, String> {
        let Some(c) = self.peek(0) else {
            return Err(reg(EBRACK));
        };
        self.i += 1;
        match c {
            ']' if !first => Ok(Item::End),
            '\\' if !self.advf => Ok(Item::Elem('\\' as u32)),
            '\\' => match self.escape()? {
                Escape::Plain(p) => Ok(Item::Elem(p)),
                Escape::Class(k @ ('d' | 's' | 'w')) => {
                    let mut set = String::new();
                    let name = match k {
                        'd' => "digit",
                        's' => "space",
                        _ => "alnum",
                    };
                    class_items(name, self.f.icase, &mut set)?;
                    if k == 'w' {
                        set.push('_');
                    }
                    Ok(Item::Set(set))
                }
                _ => Err(reg(EESCAPE)),
            },
            '-' if !first && self.peek(0) != Some(']') => Ok(Item::Range),
            '[' => match self.peek(0) {
                None => Err(reg(EBRACK)),
                Some(open @ ('.' | '=' | ':')) => {
                    self.i += 1;
                    let mut name = Vec::new();
                    loop {
                        match self.peek(0) {
                            None => return Err(reg(EBRACK)),
                            Some(c) if c == open && self.peek(1) == Some(']') => {
                                self.i += 2;
                                break;
                            }
                            Some(c) => {
                                name.push(c);
                                self.i += 1;
                            }
                        }
                    }
                    match open {
                        ':' => {
                            if name.is_empty() {
                                return Err(reg(ECTYPE));
                            }
                            let mut set = String::new();
                            class_items(&name.iter().collect::<String>(), self.f.icase, &mut set)?;
                            Ok(Item::Set(set))
                        }
                        _ => {
                            if name.is_empty() {
                                return Err(reg(ECOLLATE));
                            }
                            let c = element(&name)?;
                            if open == '=' {
                                // `eclass`: the character itself, or its
                                // `allcases` when matching case-independently.
                                let mut set = String::new();
                                if self.f.icase {
                                    allcases(c).into_iter().for_each(|k| lit(&mut set, k));
                                } else {
                                    lit(&mut set, c);
                                }
                                Ok(Item::Set(set))
                            } else {
                                Ok(Item::Elem(c))
                            }
                        }
                    }
                }
                Some(_) => Ok(Item::Elem('[' as u32)),
            },
            other => Ok(Item::Elem(other as u32)),
        }
    }

    /// The quantifier after an atom, if any. Returns whether one was read.
    fn quantifier(&mut self) -> Result<bool, String> {
        self.skip();
        let Some(c) = self.peek(0) else {
            return Ok(false);
        };
        match c {
            '*' | '+' | '?' => {
                self.i += 1;
                self.out.push(c);
                if self.advf && self.peek(0) == Some('?') {
                    self.i += 1;
                    self.out.push('?');
                }
                Ok(true)
            }
            '{' => {
                let save = self.i;
                self.i += 1;
                self.skip();
                if !self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    // A plain `{`: not a quantifier, and read again as an atom.
                    self.i = save;
                    return Ok(false);
                }
                let m = self.bound_number()?;
                let n = if self.bound_token()? == Some(',') {
                    self.i += 1;
                    if self.bound_token()?.is_some_and(|c| c.is_ascii_digit()) {
                        Some(self.bound_number()?)
                    } else {
                        None
                    }
                } else {
                    Some(m)
                };
                if n.is_some_and(|n| m > n) {
                    return Err(reg(BADBR));
                }
                if self.bound_token()? != Some('}') {
                    return Err(reg(BADBR));
                }
                self.i += 1;
                match n {
                    Some(n) if n == m => self.out.push_str(&format!("{{{m}}}")),
                    Some(n) => self.out.push_str(&format!("{{{m},{n}}}")),
                    None => self.out.push_str(&format!("{{{m},}}")),
                }
                if self.advf && self.peek(0) == Some('?') {
                    self.i += 1;
                    self.out.push('?');
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// The next `L_EBND` token's character, after expanded-syntax skipping:
    /// a digit, `,` or `}`; anything else is `REG_BADBR` and the end of the
    /// pattern is `REG_EBRACE`.
    fn bound_token(&mut self) -> Result<Option<char>, String> {
        self.skip();
        match self.peek(0) {
            None => Err(reg(EBRACE)),
            Some(c) if c.is_ascii_digit() || c == ',' || c == '}' => Ok(Some(c)),
            Some(_) => Err(reg(BADBR)),
        }
    }

    /// `scannum`.
    fn bound_number(&mut self) -> Result<u32, String> {
        let mut n = 0u32;
        while n <= DUPMAX {
            match self.bound_token()? {
                Some(c) if c.is_ascii_digit() => {
                    n = n * 10 + c.to_digit(10).unwrap_or(0);
                    self.i += 1;
                }
                _ => break,
            }
        }
        if n > DUPMAX {
            return Err(reg(BADBR));
        }
        Ok(n)
    }

    /// The pattern body, in ARE context.
    fn regex(&mut self) -> Result<(), String> {
        let mut last = Last::Nothing;
        // The capturing number of each open group, `None` for a non-capturing one.
        let mut open: Vec<Option<u32>> = Vec::new();
        loop {
            if last == Last::Atom && self.quantifier()? {
                last = Last::Nothing;
                continue;
            }
            self.skip();
            let Some(c) = self.peek(0) else {
                break;
            };
            self.i += 1;
            last = match c {
                '*' | '+' | '?' => return Err(reg(BADRPT)),
                '{' if self.bound_follows() => return Err(reg(BADRPT)),
                '|' => {
                    self.out.push('|');
                    Last::Nothing
                }
                '^' | '$' => {
                    self.left |= c == '^' && !self.f.nlanch;
                    self.out.push(c);
                    Last::Nothing
                }
                '.' => {
                    self.out.push('.');
                    Last::Atom
                }
                '(' => {
                    if self.advf && self.peek(0) == Some('?') {
                        self.i += 1;
                        let k = self.peek(0);
                        self.i += 1;
                        match k {
                            Some(':') => {
                                open.push(None);
                                self.out.push_str("(?:");
                            }
                            Some('#') => {
                                while self.peek(0).is_some_and(|c| c != ')') {
                                    self.i += 1;
                                }
                                if !self.at_end() {
                                    self.i += 1;
                                }
                                // A comment is no token: the previous one stands.
                                continue;
                            }
                            Some('=' | '!') => return Err(refusal("look-ahead ((?= ) or (?! ))")),
                            _ => return Err(reg(BADRPT)),
                        }
                    } else {
                        self.nsub += 1;
                        open.push(Some(self.nsub));
                        self.out.push('(');
                    }
                    Last::Nothing
                }
                ')' => {
                    let Some(group) = open.pop() else {
                        // "Legal in EREs due to specification botch."
                        if !self.advf {
                            lit(&mut self.out, ')' as u32);
                            last = Last::Atom;
                            continue;
                        }
                        return Err(reg(EPAREN));
                    };
                    if let Some(n) = group {
                        self.closed.push(n);
                    }
                    self.out.push(')');
                    Last::Atom
                }
                '[' => {
                    let rest: String = self.s[self.i..].iter().take(6).collect();
                    if rest == "[:<:]]" || rest == "[:>:]]" {
                        let start = rest.as_bytes()[2] == b'<';
                        self.i += 6;
                        self.left = true;
                        self.out
                            .push_str(if start { "\\b{start}" } else { "\\b{end}" });
                        Last::Nothing
                    } else {
                        self.bracket()?;
                        Last::Atom
                    }
                }
                '\\' => match self.escape()? {
                    Escape::Plain(p) => {
                        onechr(&mut self.out, p, self.f.icase);
                        Last::Atom
                    }
                    Escape::Class(k) => {
                        self.class_escape(k)?;
                        Last::Atom
                    }
                    Escape::Constraint(k) => {
                        self.left |= k != 'Z';
                        self.out.push_str(match k {
                            'A' => "\\A",
                            'Z' => "\\z",
                            'm' => "\\b{start}",
                            'M' => "\\b{end}",
                            'y' => "\\b",
                            _ => "\\B",
                        });
                        Last::Nothing
                    }
                    Escape::Backref(n) => {
                        if !self.closed.contains(&n) {
                            return Err(reg(ESUBREG));
                        }
                        return Err(refusal("a back-reference (\\1 … \\9)"));
                    }
                },
                other => {
                    onechr(&mut self.out, other as u32, self.f.icase);
                    Last::Atom
                }
            };
        }
        if !open.is_empty() {
            return Err(reg(EPAREN));
        }
        Ok(())
    }

    /// Whether the `{` just consumed begins a bound (a digit follows, after
    /// expanded-syntax skipping), which at an atom position is `REG_BADRPT`.
    fn bound_follows(&mut self) -> bool {
        let save = self.i;
        self.skip();
        let digit = self.peek(0).is_some_and(|c| c.is_ascii_digit());
        self.i = save;
        digit
    }
}

/// One bracket-expression item as `brackpart` classifies it.
enum Item {
    /// The closing `]`.
    End,
    /// A `-` where a range operator is read.
    Range,
    /// A single collating element: a character, possibly a range endpoint.
    Elem(u32),
    /// Class items that cannot be a range endpoint.
    Set(String),
}

/// Translate `are`, compiled under `flags`, into a `regex` pattern.
/// A translated pattern.
pub(crate) struct Translated {
    pub pattern: String,
    /// The pattern holds a constraint that reads the character before the
    /// place a search starts — `\A`, `^` without `REG_NLANCH`, or a word
    /// boundary. tclsh starts every search on the rest of the subject
    /// (`Tcl_RegExpExecObj`), so such a constraint sees no character there,
    /// where `regex`'s `captures_at` sees the real one.
    pub left_context: bool,
}

pub(crate) fn translate(are: &str, flags: Flags) -> Result<Translated, String> {
    let s: Vec<char> = are.chars().collect();
    let mut f = flags;
    let mut i = 0;
    let mut quote = false;
    // `REG_ADVANCED` is `REG_EXTENDED | REG_ADVF`; the two halves are tracked
    // apart because `(?e)` keeps one and drops the other.
    let (mut extended, mut advf) = (true, true);
    // `prefixes`: the `***` directors, then embedded options.
    if s.len() >= 4 && s[..3] == ['*', '*', '*'] {
        match s[3] {
            '?' => return Err(reg(BADPAT)),
            '=' => {
                quote = true;
                f.expanded = false;
                f.nlstop = false;
                f.nlanch = false;
                i = 4;
            }
            ':' => i = 4,
            _ => return Err(reg(BADRPT)),
        }
    }
    if !quote && s.len() >= i + 3 && s[i] == '(' && s[i + 1] == '?' && s[i + 2].is_alphabetic() {
        i += 2;
        while i < s.len() && s[i].is_alphabetic() {
            match s[i] {
                'b' => {
                    extended = false;
                    advf = false;
                    quote = false;
                }
                'c' => f.icase = false,
                'e' => {
                    extended = true;
                    advf = false;
                    quote = false;
                }
                'i' => f.icase = true,
                'm' | 'n' => {
                    f.nlstop = true;
                    f.nlanch = true;
                }
                'p' => {
                    f.nlstop = true;
                    f.nlanch = false;
                }
                'q' => {
                    quote = true;
                    extended = false;
                    advf = false;
                }
                's' => {
                    f.nlstop = false;
                    f.nlanch = false;
                }
                't' => f.expanded = false,
                'w' => {
                    f.nlstop = false;
                    f.nlanch = true;
                }
                'x' => f.expanded = true,
                _ => return Err(reg(BADOPT)),
            }
            i += 1;
        }
        if s.get(i) != Some(&')') {
            return Err(reg(BADOPT));
        }
        i += 1;
        if quote {
            f.expanded = false;
            f.nlstop = false;
            f.nlanch = false;
        } else if !extended {
            return Err("the BRE syntax ((?b)) is not supported yet".to_string());
        }
    }

    let mut out = prefix(f);
    if quote {
        for &c in &s[i..] {
            onechr(&mut out, c as u32, f.icase);
        }
        return Ok(Translated {
            pattern: out,
            left_context: false,
        });
    }
    let mut lx = Lexer {
        s,
        i,
        f,
        advf,
        nsub: 0,
        closed: Vec::new(),
        out,
        left: false,
    };
    lx.regex()?;
    Ok(Translated {
        pattern: lx.out,
        left_context: lx.left,
    })
}

/// The inline flags of the translated pattern. ARE's `.` crosses a newline
/// unless `REG_NLSTOP`, so `(?s)` is the default here, not the exception.
/// `REG_ICASE` is not among them: it is applied to each character as it is
/// emitted ([`onechr`], [`range`]).
fn prefix(f: Flags) -> String {
    let mut p = String::from("(?");
    if f.nlanch {
        p.push('m');
    }
    if !f.nlstop {
        p.push('s');
    }
    if p == "(?" {
        return String::new();
    }
    p.push(')');
    p
}
