//! What is under a cursor: the word, and what that word is allowed to be.
//!
//! The REPL's completer and the language server ask the same two questions of a
//! half-typed line — where does this word start, and is it a command name, an
//! ensemble's subcommand, a variable, or an argument — so they ask them here.
//!
//! This is deliberately not the parser. A line being typed is usually not a
//! script yet: `puts [str` has no close bracket and `string tou` has no
//! arguments. The parser refuses both, and rightly; a cursor still has to be
//! answered about them. So the scan is textual and local, reading backwards
//! from the cursor to the nearest boundary, and it agrees with the parser on
//! complete input by using the same boundary characters.

/// What a word in a given position can be.
#[derive(Debug, PartialEq, Eq)]
pub enum Context<'a> {
    /// The head of a command: a command name, or a procedure.
    Command,
    /// The word after an ensemble's name: one of its subcommands. Carries the
    /// ensemble.
    Subcommand(&'a str),
    /// A `$` substitution: a variable.
    Variable,
    /// An argument, which could be anything.
    Argument,
}

/// Where the word under the cursor starts, and what has been typed of it.
///
/// Word separators are the characters that end a word for the parser:
/// whitespace, the command separator `;`, and the brackets, braces and quotes
/// that open or close a nested construct. A `$` is kept inside the word so that
/// a suggestion replaces the sigil too and the line does not end up as
/// `$$name`.
pub fn word_at(line: &str, pos: usize) -> (usize, &str) {
    let pos = pos.min(line.len());
    let before = line.get(..pos).unwrap_or("");
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || matches!(*c, ';' | '[' | ']' | '{' | '}' | '"'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    (start, line.get(start..pos).unwrap_or(""))
}

/// What the word starting at `start` is allowed to be.
///
/// The command it belongs to begins after the last separator that starts one —
/// a newline, a `;`, or a `[` opening a command substitution. If the word is
/// the first since then it names a command; if it is the second and the first
/// names an ensemble, it names a subcommand.
///
/// `ensemble` decides which head words take subcommands. [`crate::names`]
/// answers it for the real command set; a caller that has no opinion can pass
/// one that always answers false and get `Command` / `Argument` only.
pub fn context_at<'a>(
    line: &'a str,
    start: usize,
    word: &str,
    ensemble: impl Fn(&str) -> bool,
) -> Context<'a> {
    if word.starts_with('$') {
        return Context::Variable;
    }
    let before = line.get(..start).unwrap_or("");
    let command = match before.rfind(['\n', ';', '[']) {
        Some(at) => &before[at + 1..],
        None => before,
    };
    let mut words = command.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => Context::Command,
        (Some(head), None) if ensemble(head) => Context::Subcommand(head),
        _ => Context::Argument,
    }
}

/// [`context_at`] against the command set this crate compiles.
pub fn context_in_tcl<'a>(line: &'a str, start: usize, word: &str) -> Context<'a> {
    context_at(line, start, word, |head| {
        !crate::names::subcommands(head).is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_keeps_its_sigil_and_stops_at_a_bracket() {
        assert_eq!(word_at("puts $ab", 8), (5, "$ab"));
        assert_eq!(word_at("puts [ll", 8), (6, "ll"));
        assert_eq!(word_at("set x {a", 8), (7, "a"));
        assert_eq!(word_at("", 0), (0, ""));
    }

    /// A cursor in the middle of a line reads the word it is inside, not the
    /// one the line ends with.
    #[test]
    fn a_word_is_read_up_to_the_cursor_only() {
        assert_eq!(word_at("puts hello", 7), (5, "he"));
    }

    #[test]
    fn the_head_of_a_command_names_a_command() {
        for line in ["ls", "puts hi; ls", "puts [ls"] {
            let (start, word) = word_at(line, line.len());
            assert_eq!(
                context_in_tcl(line, start, word),
                Context::Command,
                "{line}"
            );
        }
    }

    #[test]
    fn the_word_after_an_ensemble_names_a_subcommand() {
        let line = "string tou";
        let (start, word) = word_at(line, line.len());
        assert_eq!(
            context_in_tcl(line, start, word),
            Context::Subcommand("string")
        );
        // Only the word right after it, and only for a command that has any.
        for line in ["string toupper ab", "puts to"] {
            let (start, word) = word_at(line, line.len());
            assert_eq!(
                context_in_tcl(line, start, word),
                Context::Argument,
                "{line}"
            );
        }
    }

    #[test]
    fn a_dollar_names_a_variable_wherever_it_is() {
        for line in ["puts $x", "$x"] {
            let (start, word) = word_at(line, line.len());
            assert_eq!(
                context_in_tcl(line, start, word),
                Context::Variable,
                "{line}"
            );
        }
    }
}
