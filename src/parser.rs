//! Tcl parser — the twelve syntax rules of `Tcl(n)`, the dodekalogue.
//!
//! Parsing a Tcl script produces a [`Script`]: a list of [`Command`]s, each a
//! list of [`Word`]s, each a sequence of [`Part`]s. A part is either literal
//! text or a substitution (variable, array element, or nested script) that the
//! compiler resolves at runtime. Substitutions never split words (rule 12); the
//! sole exception is `{*}` argument expansion (rule 5), which is recorded on
//! the word as [`Word::expand`] and applied when the command is assembled.
//!
//! Two properties of the grammar are what make a compiler worthwhile: braces
//! suppress all substitution (rule 6), so a braced body is known in full at
//! parse time; and each character is processed exactly once (rule 11), so the
//! parse is single-pass with no rescanning of substituted values.
//!
//! Behavior is matched against tclsh 9.0.4, which is the specification here.
//! Where the man page is silent, the observed behavior of that interpreter is
//! reproduced and noted at the site.

use std::fmt;

/// A parse failure, carrying the byte offset and 1-based line where it was
/// detected. Messages match the interpreter's wording (`missing close-brace`,
/// `extra characters after close-quote`, …) so diagnostics are comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub msg: String,
    pub offset: usize,
    pub line: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (line {})", self.msg, self.line)
    }
}

impl std::error::Error for ParseError {}

/// One piece of a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Literal text, with backslash sequences already resolved.
    Lit(String),
    /// `$name` or `${name}` — a scalar variable.
    Var(String),
    /// `$name(index)` or `${name(index)}` — an array element. The index is
    /// itself substitutable in the unbraced form; the braced form yields a
    /// single [`Part::Lit`], since no substitution happens inside `${}`.
    Elem { name: String, index: Vec<Part> },
    /// `[...]` — command substitution, parsed eagerly into a nested script.
    Script(Script),
}

/// One word of a command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Word {
    /// The word's pieces, concatenated at runtime. Empty means the empty word.
    pub parts: Vec<Part>,
    /// Rule 5: the word was prefixed with `{*}` and expands into multiple
    /// arguments, its value re-parsed as a list at call time.
    pub expand: bool,
    /// Rule 6: the word came from braces, so it is literal by construction.
    /// The compiler uses this to decide whether a body or expression can be
    /// compiled statically rather than assembled and parsed at runtime.
    pub braced: bool,
    /// The word was double-quoted. Substitutions still apply (rule 4).
    pub quoted: bool,
}

impl Word {
    /// The word's text when it is fully literal — no substitutions to perform.
    pub fn as_literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [] => Some(""),
            [Part::Lit(s)] => Some(s),
            _ => None,
        }
    }

    fn literal(text: String, braced: bool, expand: bool) -> Word {
        let parts = if text.is_empty() {
            Vec::new()
        } else {
            vec![Part::Lit(text)]
        };
        Word {
            parts,
            expand,
            braced,
            quoted: false,
        }
    }
}

/// One command: its words and the 1-based line it started on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<Word>,
    pub line: usize,
}

/// A parsed script.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    pub commands: Vec<Command>,
}

/// How deeply command substitutions and array indices may nest before the parser
/// refuses to go further.
///
/// The parse of `[...]` and of a `$name(...)` index is recursive, so nesting
/// costs native stack — and running out of it is a signal, not an error, which
/// kills the process with nothing to report. Refusing at a fixed depth turns
/// that into a Tcl error the input can be blamed for.
///
/// The number is measured, not chosen for looks. On the stack the `tclrs` binary
/// gives the parser ([`crate::runtime::RECOMMENDED_STACK`], which is what a host
/// embedding this crate is documented to provide) a script of nothing but `[`
/// still parses at 80_000 levels and aborts by 90_000; the reference interpreter
/// segfaults on the same input between 20_000 and 30_000. 64_000 is under the
/// measured floor with room for the deeper frames a nested index or quoted word
/// adds, and above every depth tclsh itself survives, so nothing tclsh can parse
/// is refused here.
pub const MAX_NESTING_DEPTH: usize = 64_000;

/// Whether `src` needs more input before it is a script.
///
/// `tclsh` asks `Tcl_CommandComplete`, a scanner written for the question.
/// tclrs asks the parser instead and reads which failure it reports, because the
/// parser already separates a construct left open at the end of the input from
/// one that is malformed — and only the first can be fixed by typing more. A
/// malformed script is *complete*: it is evaluated, and its error reported,
/// rather than leaving the prompt waiting for input that cannot help.
///
/// Two callers, and they are asking the same question: the REPL, deciding
/// whether to keep reading a line, and `info complete`, which is that question
/// spelled as a command. `info complete "}"` is 1 in tclsh 9.0.4 — a lone close
/// brace is not a script but nothing further would make it one — and that falls
/// out of the rule rather than being special-cased.
pub fn incomplete(src: &str) -> bool {
    matches!(parse(src), Err(e) if UNTERMINATED.contains(&e.msg.as_str()))
}

/// The parser's messages for a construct still open at the end of the input.
/// Every one of them is reached only by running out of text.
const UNTERMINATED: &[&str] = &[
    "missing close-brace",
    "missing close-brace for variable name",
    "missing close-bracket",
    "missing \"",
    "missing )",
];

/// Parse a complete script.
pub fn parse(src: &str) -> Result<Script, ParseError> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let script = p.parse_script(false)?;
    // A `]` with no opening `[` reaches here as an unconsumed terminator.
    if p.pos < p.src.len() {
        return Err(p.error("extra characters after close-bracket"));
    }
    Ok(script)
}

/// The prefix of `src` that parses as whole commands, for a script the whole-
/// script parse rejected.
///
/// `Tcl_EvalEx` parses ONE command, evaluates it, and only then parses the
/// next, so a syntax error partway through a script never stops the commands
/// before it from running — `puts hi` followed by `puts {` writes `hi` and
/// *then* reports `missing close-brace`, and the location it names is the line
/// the failing command STARTS on, not the line the scan ran out of text on.
/// Parsing the whole script up front, which is what lets one script lower to
/// one chunk, would swallow both.
///
/// Returns `(end, line, err)` — `end` is the byte offset just past the last
/// command that parsed (0 when the very first one fails), `line` is the failing
/// command's own first line, and `err` is what it raised. `None` when `src`
/// parses: this is the recovery path and nothing else calls it.
pub(crate) fn valid_prefix(src: &str) -> Option<(usize, usize, ParseError)> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let mut end = 0;
    loop {
        p.skip_between_commands();
        if p.pos >= p.src.len() {
            return None;
        }
        // Where this command begins — what `end` becomes once it parses, and
        // the line the error names when it does not.
        let start_line = p.line;
        loop {
            if let Err(e) = p.parse_word(false) {
                return Some((end, start_line, e));
            }
            if !p.skip_word_gap() || p.at_command_end(false) {
                break;
            }
        }
        // A `]` with no opening `[` stops the scan here exactly as it stops
        // `parse`, and it belongs to this command rather than to the prefix.
        if p.pos < p.src.len() && p.peek() == Some(b']') {
            return Some((
                end,
                start_line,
                p.error("extra characters after close-bracket"),
            ));
        }
        end = p.pos;
    }
}

/// Parse one `$...` substitution at `at`, which must index a `$`. Returns the
/// part and the offset just past it, or `None` when the dollar introduces no
/// name and is therefore literal text.
///
/// The `expr` language embeds the same substitutions as a word (`$x`, `$a(i)`,
/// `${x}`), so its parser reaches them through here rather than reimplementing
/// rule 8.
pub(crate) fn substitution_at(src: &str, at: usize) -> Result<Option<(Part, usize)>, ParseError> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: at,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    Ok(p.parse_dollar()?.map(|part| (part, p.pos)))
}

/// Parse one `[...]` command substitution at `at`, which must index a `[`.
pub(crate) fn command_at(src: &str, at: usize) -> Result<(Script, usize), ParseError> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: at + 1,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let script = p.parse_script(true)?;
    if p.peek() != Some(b']') {
        return Err(p.error("missing close-bracket"));
    }
    p.pos += 1;
    Ok((script, p.pos))
}

/// Parse a double-quoted operand at `at`, which must index a `"`. Substitutions
/// inside it are resolved as in a quoted word (rule 4).
pub(crate) fn quoted_at(src: &str, at: usize) -> Result<(Vec<Part>, usize), ParseError> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: at + 1,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let parts = p.parse_parts(Ctx::Quoted, false)?;
    p.pos += 1; // closing quote
    Ok((parts, p.pos))
}

/// Resolve the backslash sequence at `at`, which must index a `\`. Returns the
/// text it stands for and the offset just past it.
///
/// List elements carry the same escapes as a word (rule 9), so `list` reaches
/// rule 9's table through here rather than repeating it. Backslash-newline is
/// handled here too: outside a word there is no separator to produce, so it
/// simply becomes the space it folds to.
pub(crate) fn backslash_at(src: &str, at: usize) -> (String, usize) {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: at,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let mut out = String::new();
    if p.at(1) == Some(b'\n') {
        p.skip_line_continuation();
        out.push(' ');
    } else {
        p.parse_backslash(&mut out);
    }
    (out, p.pos)
}

/// Parse a braced operand at `at`, which must index a `{`. The text is literal
/// (rule 6).
pub(crate) fn braced_at(src: &str, at: usize) -> Result<(String, usize), ParseError> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: at,
        line: 1,
        depth: 0,
        subst: SubstFlags::default(),
        collect: false,
        scripts: Vec::new(),
        mark: 0,
    };
    let text = p.parse_braced()?;
    Ok((text, p.pos))
}

/// Where a run of substitutable text sits, which decides what ends it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// An unquoted word: whitespace, `;`, newline (and `]` when nested) end it.
    Bare,
    /// Inside double quotes: only the closing quote ends it.
    Quoted,
    /// An array index inside `$name(...)`: `)` ends it.
    Index,
    /// A whole value read as one word, which nothing but the end of the input
    /// ends: what `subst` substitutes over. `ParseTokens` reaches this by being
    /// called with an empty stop mask (`generic/tclParse.c:1921`), so a space, a
    /// `;`, a `"` and a `]` are all ordinary text here.
    All,
}

/// Which of the three substitutions a `Ctx::All` scan performs.
///
/// The three `subst` options each clear one, and a cleared one makes its
/// introducer a single character of literal text rather than the start of a
/// construct — `ParseTokens`' `noSubstVars` / `noSubstCmds` / `noSubstBS`
/// (`generic/tclParse.c:1057-1059`). Applying them while scanning, rather than
/// unpicking a parse afterwards, is what makes `subst -nobackslashes {a\[b]c}`
/// substitute the command: the backslash is text and the `[` after it still
/// opens a substitution, which is what tclsh 9.0.4 answers (measured).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SubstFlags {
    pub backslashes: bool,
    pub commands: bool,
    pub variables: bool,
}

impl Default for SubstFlags {
    fn default() -> Self {
        SubstFlags {
            backslashes: true,
            commands: true,
            variables: true,
        }
    }
}

/// One piece of a value being substituted.
///
/// [`Part`] with the command substitutions kept as *source* rather than as a
/// parsed [`Script`]. The reference interpreter compiles and caches a nested
/// script by its text, and so does this one ([`crate::cache`]), so `subst` runs
/// the text between the brackets exactly as `eval` runs its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstPart {
    Lit(String),
    Var(String),
    Elem { name: String, index: Vec<SubstPart> },
    Script(String),
}

/// What [`subst_parts`] read: the parts to substitute, and the syntax error
/// that stops the substitution *after* they have run, if there was one.
pub struct SubstParse {
    pub parts: Vec<SubstPart>,
    /// A parse failure. The reference implementation substitutes the good
    /// prefix and its side effects happen before this is reported, which is why
    /// it travels beside the parts instead of replacing them.
    pub error: Option<ParseError>,
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    /// How many command substitutions and array indices are open at the cursor —
    /// the recursion this parser does, bounded by [`MAX_NESTING_DEPTH`].
    depth: usize,
    /// Which substitutions a [`Ctx::All`] scan performs. Ignored in every other
    /// context, where all three always apply.
    subst: SubstFlags,
    /// Where the [`Ctx::All`] construct being scanned started. Only that scan
    /// writes it, so after a failure it names the top-level `$` or `[` that
    /// failed — which is what the recovery in [`subst_parts`] needs and what
    /// `Tcl_Parse.term` carries in the reference implementation.
    mark: usize,
    /// Whether to record the source of each command substitution as it closes.
    /// On only for a `subst` scan, and only *outside* any bracket: a `[...]`
    /// inside another one is part of that one's text and is never run on its
    /// own, so recording it would leave an entry nothing consumes.
    collect: bool,
    /// The text between the brackets of each command substitution recorded,
    /// in the order the scan closed them — which is source order, and is the
    /// order [`with_sources`] walks the parts in.
    scripts: Vec<String>,
}

impl<'a> Parser<'a> {
    fn over(src: &'a [u8], subst: SubstFlags) -> Parser<'a> {
        Parser {
            src,
            pos: 0,
            line: 1,
            depth: 0,
            subst,
            mark: 0,
            collect: true,
            scripts: Vec::new(),
        }
    }
}

/// Pair a value's parts with the sources recorded for the command
/// substitutions in them.
///
/// Both sequences are in source order — the scanner records a `[...]` when it
/// consumes the closing bracket, and this walk visits parts left to right — so
/// one iterator is the whole of the correspondence. A recorded source with no
/// part to take it, or the other way round, is impossible by construction: the
/// scanner records exactly when it pushes a [`Part::Script`] outside a bracket,
/// and those are exactly the parts this walk reaches.
fn with_sources(parts: Vec<Part>, scripts: &mut std::vec::IntoIter<String>) -> Vec<SubstPart> {
    parts
        .into_iter()
        .map(|part| match part {
            Part::Lit(text) => SubstPart::Lit(text),
            Part::Var(name) => SubstPart::Var(name),
            Part::Elem { name, index } => SubstPart::Elem {
                name,
                index: with_sources(index, scripts),
            },
            Part::Script(_) => SubstPart::Script(scripts.next().unwrap_or_default()),
        })
        .collect()
}

/// Read a value the way `subst` reads it: one word's worth of parts, running to
/// the end of the input, with the three substitutions the flags admit.
///
/// A port of `TclSubstParse` (`generic/tclParse.c:1901-2073`), recovery
/// included: when the scan fails, everything before the construct that failed
/// is still substituted — and still runs — before the caller reports the error.
/// That is observable, not bookkeeping: `subst {[puts hi][}` writes `hi` and
/// *then* fails with `missing close-bracket` in tclsh 9.0.4 (measured).
pub fn subst_parts(src: &str, flags: SubstFlags) -> SubstParse {
    let bytes = src.as_bytes();
    let mut p = Parser::over(bytes, flags);
    match p.parse_parts(Ctx::All, false) {
        Ok(parts) => SubstParse {
            parts: with_sources(parts, &mut p.scripts.into_iter()),
            error: None,
        },
        Err(e) => {
            let mark = p.mark;
            // The tokens of the first attempt are gone, so the prefix is read
            // again — the same thing `TclSubstParse` does for the same reason.
            // It parsed once already, so it cannot fail now.
            let mut head = Parser::over(&bytes[..mark], flags);
            let parts = head.parse_parts(Ctx::All, false).unwrap_or_default();
            let mut parts = with_sources(parts, &mut head.scripts.into_iter());
            // An unterminated `[` still runs the complete commands inside it;
            // an unterminated `${` or `$name(` runs nothing, and the reference
            // implementation drops the half-read variable rather than reading a
            // scalar of that name.
            if bytes.get(mark) == Some(&b'[') {
                let inner = &bytes[mark + 1..];
                let end = Parser::over(inner, flags).complete_command_text();
                if end > 0 {
                    parts.push(SubstPart::Script(
                        String::from_utf8_lossy(&inner[..end]).into_owned(),
                    ));
                }
            }
            SubstParse {
                parts,
                error: Some(e),
            }
        }
    }
}

impl<'a> Parser<'a> {
    /// Descend one nesting level, or refuse. The message names what ran out, in
    /// the shape the reference interpreter words its own depth refusals
    /// (`too many nested evaluations (infinite loop?)`), because there is no
    /// reference behavior to copy: tclsh has no limit here and dies on a signal.
    fn descend(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err(self.error("too many nested substitutions (infinite loop?)"));
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
        }
        Some(b)
    }

    fn error(&self, msg: &str) -> ParseError {
        ParseError {
            msg: msg.to_string(),
            offset: self.pos,
            line: self.line,
        }
    }

    /// Rule 1: commands separated by newlines and semicolons; rule 10:
    /// a `#` where a command's first word would start begins a comment.
    fn parse_script(&mut self, nested: bool) -> Result<Script, ParseError> {
        let mut commands = Vec::new();
        loop {
            self.skip_between_commands();
            if self.pos >= self.src.len() {
                break;
            }
            if nested && self.peek() == Some(b']') {
                break;
            }
            let line = self.line;
            let mut words = Vec::new();
            loop {
                words.push(self.parse_word(nested)?);
                if !self.skip_word_gap() || self.at_command_end(nested) {
                    break;
                }
            }
            commands.push(Command { words, line });
        }
        Ok(Script { commands })
    }

    /// How much of an unterminated `[`'s text still runs: everything before the
    /// last command separator the scan reached.
    ///
    /// `TclSubstParse`'s bracket recovery (`generic/tclParse.c:2015-2066`) parses
    /// commands out of the unterminated substitution until one fails or the text
    /// runs out, and gives the substitution token everything up to the last
    /// *separator* it saw — so a command the missing bracket cut short does not
    /// run, and the ones before it do. `subst {a[puts hi; puts there}` writes
    /// `hi` and then fails, in tclsh 9.0.4 and here.
    fn complete_command_text(&mut self) -> usize {
        let mut end = 0;
        loop {
            self.skip_between_commands();
            if self.pos >= self.src.len() {
                return end;
            }
            loop {
                if self.parse_word(false).is_err() {
                    return end;
                }
                if !self.skip_word_gap() || self.at_command_end(false) {
                    break;
                }
            }
            // Only a command a `;` or a newline closed is complete; one that ran
            // to the end of the text is the one the bracket was meant to close.
            match self.peek() {
                Some(b';') | Some(b'\n') => {
                    end = self.pos;
                    self.bump();
                }
                _ => return end,
            }
        }
    }

    /// Consume separators and comments between commands.
    fn skip_between_commands(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b';') => {
                    self.bump();
                }
                Some(b'\\') if self.at(1) == Some(b'\n') => {
                    self.skip_line_continuation();
                }
                // Rule 10: the hash is only special in first-word position,
                // which is exactly where this loop leaves off.
                Some(b'#') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        // A backslash-newline inside a comment keeps the
                        // comment going, since the pre-pass folds it to a space.
                        if b == b'\\' && self.at(1) == Some(b'\n') {
                            self.skip_line_continuation();
                            continue;
                        }
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// Consume the whitespace between two words of the same command. Returns
    /// false when no gap was found, meaning the word list is finished.
    fn skip_word_gap(&mut self) -> bool {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') => {
                    self.bump();
                }
                // Rule 9: backslash-newline collapses to a space, and outside
                // braces and quotes that space separates words.
                Some(b'\\') if self.at(1) == Some(b'\n') => {
                    self.skip_line_continuation();
                }
                _ => break,
            }
        }
        self.pos > start
    }

    /// Rule 9's pre-pass: `\`, newline, and following spaces and tabs become a
    /// single space. The caller decides whether that space is text or a
    /// separator.
    fn skip_line_continuation(&mut self) {
        self.bump(); // backslash
        self.bump(); // newline
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.bump();
        }
    }

    fn at_command_end(&self, nested: bool) -> bool {
        match self.peek() {
            None | Some(b'\n') | Some(b';') => true,
            Some(b']') => nested,
            _ => false,
        }
    }

    fn parse_word(&mut self, nested: bool) -> Result<Word, ParseError> {
        let expand = self.at_expansion_prefix(nested);
        if expand {
            self.pos += 3;
        }
        match self.peek() {
            Some(b'{') => {
                let text = self.parse_braced()?;
                self.check_word_end(nested, "close-brace")?;
                Ok(Word::literal(text, true, expand))
            }
            Some(b'"') => {
                self.bump();
                let parts = self.parse_parts(Ctx::Quoted, nested)?;
                self.bump(); // closing quote
                self.check_word_end(nested, "close-quote")?;
                Ok(Word {
                    parts,
                    expand,
                    braced: false,
                    quoted: true,
                })
            }
            _ => {
                let parts = self.parse_parts(Ctx::Bare, nested)?;
                Ok(Word {
                    parts,
                    expand,
                    braced: false,
                    quoted: false,
                })
            }
        }
    }

    /// Rule 5: `{*}` expands only when followed by a non-whitespace character
    /// that starts a word. `list {*} x` and `list {*};` both pass `{*}` through
    /// as an ordinary braced word yielding `*`, which is what tclsh 9.0.4 does.
    fn at_expansion_prefix(&self, nested: bool) -> bool {
        if self.src[self.pos..].starts_with(b"{*}") {
            match self.at(3) {
                None | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b';') => false,
                Some(b']') => !nested,
                Some(_) => true,
            }
        } else {
            false
        }
    }

    /// Rule 6: braces nest, nothing inside is substituted, and the braces are
    /// dropped. A backslash-escaped brace does not count toward nesting but the
    /// backslash itself is kept: `{a\}b}` is the five characters `a\}b`. The
    /// one transformation that does apply is rule 9's backslash-newline.
    fn parse_braced(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.bump(); // opening brace
        let mut depth = 1usize;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                self.pos = open;
                return Err(self.error("missing close-brace"));
            };
            match b {
                b'\\' if self.at(1) == Some(b'\n') => {
                    self.skip_line_continuation();
                    out.push(' ');
                }
                b'\\' => {
                    self.bump();
                    out.push('\\');
                    if self.peek().is_some() {
                        self.copy_char(&mut out);
                    }
                }
                b'{' => {
                    depth += 1;
                    self.bump();
                    out.push('{');
                }
                b'}' => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        return Ok(out);
                    }
                    out.push('}');
                }
                _ => self.copy_char(&mut out),
            }
        }
    }

    /// Rules 7, 8 and 9: read text and substitutions until the context's
    /// terminator.
    fn parse_parts(&mut self, ctx: Ctx, nested: bool) -> Result<Vec<Part>, ParseError> {
        let mut parts: Vec<Part> = Vec::new();
        let mut lit = String::new();

        loop {
            let Some(b) = self.peek() else {
                match ctx {
                    Ctx::Quoted => return Err(self.error("missing \"")),
                    Ctx::Index => return Err(self.error("missing )")),
                    Ctx::Bare | Ctx::All => break,
                }
            };
            // Where the construct about to be read starts. Only the whole-value
            // scan records it, and only it needs to: after a failure there it
            // names the `$` or `[` to blame, which is the recovery point.
            if ctx == Ctx::All {
                self.mark = self.pos;
            }
            match b {
                b'"' if ctx == Ctx::Quoted => break,
                b')' if ctx == Ctx::Index => break,
                // tclsh 9.0.4 rejects a literal `(` inside `$name(index)`:
                // `$a(x(y))` is an error even though `set a(x(y)) 1` is legal,
                // because only the parsed form constrains the index text.
                b'(' if ctx == Ctx::Index => {
                    return Err(self.error("invalid character in array index"))
                }
                b' ' | b'\t' | b'\r' | b'\n' | b';' if ctx == Ctx::Bare => break,
                b']' if ctx == Ctx::Bare && nested => break,
                // `subst -nobackslashes`, `-nocommands` and `-novariables`: the
                // introducer is one character of text and whatever follows it is
                // read on its own terms. Ahead of the arms that would open a
                // construct, and only reachable from the whole-value scan.
                b'\\' if ctx == Ctx::All && !self.subst.backslashes => {
                    self.bump();
                    lit.push('\\');
                }
                b'[' if ctx == Ctx::All && !self.subst.commands => {
                    self.bump();
                    lit.push('[');
                }
                b'$' if ctx == Ctx::All && !self.subst.variables => {
                    self.bump();
                    lit.push('$');
                }
                // Rule 9: outside braces and quotes the folded space is a word
                // separator, so it ends the word rather than joining it.
                b'\\' if self.at(1) == Some(b'\n') => {
                    if ctx == Ctx::Bare {
                        break;
                    }
                    self.skip_line_continuation();
                    lit.push(' ');
                }
                b'\\' => self.parse_backslash(&mut lit),
                b'[' => {
                    flush(&mut lit, &mut parts);
                    self.bump();
                    let opened = self.pos;
                    self.descend()?;
                    // A `[...]` inside this one belongs to its text and is never
                    // run on its own, so nothing inside is recorded.
                    let collecting = std::mem::replace(&mut self.collect, false);
                    let inner = self.parse_script(true);
                    self.collect = collecting;
                    let inner = inner?;
                    self.depth -= 1;
                    if self.peek() != Some(b']') {
                        return Err(self.error("missing close-bracket"));
                    }
                    if self.collect {
                        self.scripts.push(
                            String::from_utf8_lossy(&self.src[opened..self.pos]).into_owned(),
                        );
                    }
                    self.bump();
                    parts.push(Part::Script(inner));
                }
                b'$' => {
                    if let Some(part) = self.parse_dollar()? {
                        flush(&mut lit, &mut parts);
                        parts.push(part);
                    } else {
                        // A `$` that begins no valid name is ordinary text.
                        self.bump();
                        lit.push('$');
                    }
                }
                _ => self.copy_char(&mut lit),
            }
        }

        flush(&mut lit, &mut parts);
        Ok(parts)
    }

    /// Rule 8. Returns `None` when the dollar sign introduces no variable name
    /// and is therefore literal.
    fn parse_dollar(&mut self) -> Result<Option<Part>, ParseError> {
        if self.at(1) == Some(b'{') {
            return self.parse_braced_var().map(Some);
        }

        let name = self.scan_var_name(self.pos + 1);
        if name.is_empty() {
            return Ok(None);
        }
        self.pos += 1 + name.len();

        if self.peek() == Some(b'(') {
            self.bump();
            self.descend()?;
            let index = self.parse_parts(Ctx::Index, false)?;
            self.depth -= 1;
            if self.peek() != Some(b')') {
                return Err(self.error("missing )"));
            }
            self.bump();
            return Ok(Some(Part::Elem { name, index }));
        }
        Ok(Some(Part::Var(name)))
    }

    /// A bare variable name: ASCII letters, digits, underscores, and runs of
    /// two or more colons. A single colon ends the name, so `$b:x` is `$b`
    /// followed by the text `:x`.
    fn scan_var_name(&self, from: usize) -> String {
        let mut i = from;
        while i < self.src.len() {
            let b = self.src[i];
            if b.is_ascii_alphanumeric() || b == b'_' {
                i += 1;
            } else if b == b':' {
                let colons = self.src[i..].iter().take_while(|&&c| c == b':').count();
                if colons < 2 {
                    break;
                }
                i += colons;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.src[from..i]).into_owned()
    }

    /// `${name}`: the name is taken verbatim, with no substitution, and it ends
    /// at the close brace that BALANCES the ones inside it.
    ///
    /// A port of `Tcl_ParseVarName`'s scan (`generic/tclParse.c:1383-1416`),
    /// whose three rules the naive "up to the first `}`" reading gets wrong:
    ///
    /// * a `{` inside opens a group and the `}` that closes it does not end the
    ///   name, so `${a{b}c}` is the variable `a{b}c` and not `a{b`;
    /// * a backslash consumes the byte after it, so `${a\}b}` names `a\}b` —
    ///   the escaped brace neither ends the name nor changes the depth, and both
    ///   bytes stay IN the name, which is why it is a different variable from
    ///   the one `set "a\}b"` creates;
    /// * running out of text is `missing close-brace for variable name`. This is
    ///   what `puts ${` followed by a line with a balanced `{…}` in it reports,
    ///   where stopping at the first `}` instead swallowed the rest of the
    ///   script into the name.
    ///
    /// The array form `${name(index)}` applies when the text before `(` holds no
    /// `(` of its own and the text ends with `)`.
    fn parse_braced_var(&mut self) -> Result<Part, ParseError> {
        let open = self.pos;
        self.pos += 2; // `${`
        let start = self.pos;
        let mut depth = 0usize;
        while let Some(b) = self.peek() {
            match b {
                b'}' if depth == 0 => {
                    let raw = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                    self.bump();
                    return Ok(braced_var_part(raw));
                }
                b'}' => depth -= 1,
                b'{' => depth += 1,
                // "if 2 or more left, consume 2, else consume just the \ and
                // let it run into the end" — the escaped byte is skipped
                // whatever it is, so it can neither close the name nor nest.
                b'\\' if self.at(1).is_some() => {
                    self.bump();
                }
                _ => {}
            }
            self.bump();
        }
        self.pos = open;
        Err(self.error("missing close-brace for variable name"))
    }

    /// After a braced or quoted word, only a separator or terminator may
    /// follow — `set v {a}b` is an error, not a concatenation.
    fn check_word_end(&mut self, nested: bool, what: &str) -> Result<(), ParseError> {
        let ok = match self.peek() {
            None | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b';') => true,
            Some(b']') => nested,
            Some(b'\\') => self.at(1) == Some(b'\n'),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(self.error(&format!("extra characters after {what}")))
        }
    }

    /// Rule 9's escape table. Anything not listed drops the backslash and keeps
    /// the character.
    fn parse_backslash(&mut self, out: &mut String) {
        self.bump(); // backslash
        let Some(b) = self.peek() else {
            out.push('\\');
            return;
        };
        match b {
            b'a' => {
                self.bump();
                out.push('\u{7}');
            }
            b'b' => {
                self.bump();
                out.push('\u{8}');
            }
            b'f' => {
                self.bump();
                out.push('\u{c}');
            }
            b'n' => {
                self.bump();
                out.push('\n');
            }
            b'r' => {
                self.bump();
                out.push('\r');
            }
            b't' => {
                self.bump();
                out.push('\t');
            }
            b'v' => {
                self.bump();
                out.push('\u{b}');
            }
            b'\\' => {
                self.bump();
                out.push('\\');
            }
            b'x' => {
                self.bump();
                match self.scan_radix(16, 2, 0xFF) {
                    Some(v) => push_code_point(out, v),
                    None => out.push('x'),
                }
            }
            b'u' => {
                self.bump();
                match self.scan_radix(16, 4, 0x10FFFF) {
                    Some(v) => push_code_point(out, v),
                    None => out.push('u'),
                }
            }
            b'U' => {
                self.bump();
                match self.scan_radix(16, 8, 0x10FFFF) {
                    Some(v) => push_code_point(out, v),
                    None => out.push('U'),
                }
            }
            b'0'..=b'7' => {
                // Octal takes at most three digits and stops before it would
                // exceed one byte: `\1011` is `A` followed by `1`.
                match self.scan_radix(8, 3, 0xFF) {
                    Some(v) => push_code_point(out, v),
                    None => self.copy_char(out),
                }
            }
            _ => self.copy_char(out),
        }
    }

    /// Read up to `max_digits` digits in `radix`, stopping early rather than
    /// letting the value exceed `limit`. Returns `None` if no digit is present.
    fn scan_radix(&mut self, radix: u32, max_digits: usize, limit: u32) -> Option<u32> {
        let mut value: u32 = 0;
        let mut digits = 0;
        while digits < max_digits {
            let Some(b) = self.peek() else { break };
            let Some(d) = (b as char).to_digit(radix) else {
                break;
            };
            let next = value * radix + d;
            if next > limit {
                break;
            }
            value = next;
            digits += 1;
            self.bump();
        }
        (digits > 0).then_some(value)
    }

    /// Copy one whole UTF-8 character from the source into `out`.
    fn copy_char(&mut self, out: &mut String) {
        let start = self.pos;
        let len = utf8_len(self.src[start]);
        let end = (start + len).min(self.src.len());
        match std::str::from_utf8(&self.src[start..end]) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push(char::REPLACEMENT_CHARACTER),
        }
        for _ in start..end {
            self.bump();
        }
    }
}

fn flush(lit: &mut String, parts: &mut Vec<Part>) {
    if !lit.is_empty() {
        parts.push(Part::Lit(std::mem::take(lit)));
    }
}

/// Split `${...}` text into a scalar or an array element.
fn braced_var_part(raw: String) -> Part {
    if raw.ends_with(')') {
        if let Some(open) = raw.find('(') {
            let name = &raw[..open];
            if !name.contains('(') {
                let index = raw[open + 1..raw.len() - 1].to_string();
                return Part::Elem {
                    name: name.to_string(),
                    index: if index.is_empty() {
                        Vec::new()
                    } else {
                        vec![Part::Lit(index)]
                    },
                };
            }
        }
    }
    Part::Var(raw)
}

/// Append a code point. Lone surrogates have no `char` representation in Rust;
/// they become the replacement character rather than failing the parse.
fn push_code_point(out: &mut String, value: u32) {
    out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
