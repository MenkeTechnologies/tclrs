//! What the parser saw, printed: `--dump-tokens` and `--dump-ast`.
//!
//! Tcl has no lexer to dump. Substitution is decided while a word is being
//! read — `$x` is a variable because of where the `$` sits, not because a
//! separate pass classified it — so the pieces of a word, [`Part`], *are* the
//! lexical output, and [`tokens`] prints them flat, one per line, in the order
//! they were read. [`ast`] prints the same parse as the tree it is: script,
//! command, word, part, with a command substitution nesting inside the word
//! that contains it.
//!
//! Neither is a debugging aid for this crate. They exist so an editor, a
//! grammar test, or somebody reading a script can see the twelve rules being
//! applied — which word was braced (so nothing inside it will be substituted),
//! which was quoted (so everything will be), where a `{*}` expansion is.

use crate::parser::{Command, Part, Script, Word};

/// The lexical view: one line per part, deepest last, with the position of the
/// word it belongs to.
///
/// ```text
/// line word  kind    value
///    1    1  lit     puts
///    1    2  quoted  "x is $x"
///    1    2  · lit   x is
///    1    2  · var   x
/// ```
pub fn tokens(src: &str) -> Result<String, String> {
    let script = crate::parse(src).map_err(|e| e.to_string())?;
    let mut out = String::from("line word  kind     value\n");
    for command in &script.commands {
        for (index, word) in command.words.iter().enumerate() {
            let position = format!("{:>4} {:>4}", command.line, index + 1);
            out.push_str(&format!(
                "{position}  {:<8} {}\n",
                shape(word),
                one_line(word)
            ));
            for part in &word.parts {
                part_tokens(&mut out, &position, 1, part);
            }
        }
    }
    Ok(out)
}

/// How the word was written, which is what decides whether its parts will be
/// substituted at all.
fn shape(word: &Word) -> &'static str {
    match (word.braced, word.quoted, word.expand) {
        (_, _, true) => "expand",
        (true, _, _) => "braced",
        (_, true, _) => "quoted",
        _ => "bare",
    }
}

fn part_tokens(out: &mut String, position: &str, depth: usize, part: &Part) {
    let indent = "· ".repeat(depth);
    match part {
        Part::Lit(text) => out.push_str(&format!("{position}  {indent}lit    {text}\n")),
        Part::Var(name) => out.push_str(&format!("{position}  {indent}var    {name}\n")),
        Part::Elem { name, index } => {
            out.push_str(&format!("{position}  {indent}elem   {name}\n"));
            for part in index {
                part_tokens(out, position, depth + 1, part);
            }
        }
        Part::Script(script) => {
            out.push_str(&format!("{position}  {indent}script\n"));
            for command in &script.commands {
                for word in &command.words {
                    for part in &word.parts {
                        part_tokens(out, position, depth + 1, part);
                    }
                }
            }
        }
    }
}

/// The tree view: the parse as it is structured, indented.
///
/// ```text
/// script
///   command line 1
///     word bare
///       lit puts
///     word quoted
///       lit x is
///       var x
/// ```
pub fn ast(src: &str) -> Result<String, String> {
    let script = crate::parse(src).map_err(|e| e.to_string())?;
    let mut out = String::new();
    write_script(&mut out, 0, &script);
    Ok(out)
}

fn write_script(out: &mut String, depth: usize, script: &Script) {
    line(out, depth, "script");
    for command in &script.commands {
        write_command(out, depth + 1, command);
    }
}

fn write_command(out: &mut String, depth: usize, command: &Command) {
    line(out, depth, &format!("command line {}", command.line));
    for word in &command.words {
        line(out, depth + 1, &format!("word {}", shape(word)));
        for part in &word.parts {
            write_part(out, depth + 2, part);
        }
    }
}

fn write_part(out: &mut String, depth: usize, part: &Part) {
    match part {
        Part::Lit(text) => line(out, depth, &format!("lit {text}")),
        Part::Var(name) => line(out, depth, &format!("var {name}")),
        Part::Elem { name, index } => {
            line(out, depth, &format!("elem {name}"));
            for part in index {
                write_part(out, depth + 1, part);
            }
        }
        Part::Script(script) => write_script(out, depth, script),
    }
}

fn line(out: &mut String, depth: usize, text: &str) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(text);
    out.push('\n');
}

/// A word on one line, for the token listing's value column. Substitutions are
/// shown in the spelling they were written in, so the line reads like the
/// source rather than like the data structure.
fn one_line(word: &Word) -> String {
    let mut out = String::new();
    for part in &word.parts {
        match part {
            Part::Lit(text) => out.push_str(text),
            Part::Var(name) => out.push_str(&format!("${name}")),
            Part::Elem { name, .. } => out.push_str(&format!("${name}(…)")),
            Part::Script(_) => out.push_str("[…]"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_listed_with_its_parts_under_it() {
        let out = tokens("puts \"x is $x\"").expect("parses");
        assert!(out.contains("   1    1  bare     puts"), "{out}");
        assert!(out.contains("   1    2  quoted   x is $x"), "{out}");
        assert!(out.contains("· lit    x is"), "{out}");
        assert!(out.contains("· var    x"), "{out}");
    }

    #[test]
    fn a_braced_word_has_no_substitutions_to_show() {
        let out = tokens("set body {puts $x}").expect("parses");
        assert!(out.contains("braced"), "{out}");
        // The `$x` inside braces is literal text, not a variable part.
        assert!(!out.contains("var    x"), "{out}");
    }

    #[test]
    fn expansion_and_command_substitution_are_visible() {
        let out = tokens("puts {*}$args [llength $args]").expect("parses");
        assert!(out.contains("expand"), "{out}");
        assert!(out.contains("script"), "{out}");
    }

    #[test]
    fn the_tree_nests_a_command_substitution_inside_its_word() {
        let out = ast("puts [expr {1 + 2}]").expect("parses");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "script");
        assert_eq!(lines[1], "  command line 1");
        assert!(out.contains("      script"), "{out}");
        assert!(out.contains("        command line 1"), "{out}");
    }

    #[test]
    fn an_array_element_shows_the_index_it_was_given() {
        let out = ast("puts $a($i)").expect("parses");
        assert!(out.contains("elem a"), "{out}");
        assert!(out.contains("var i"), "{out}");
    }

    /// A script that does not parse is reported, not dumped — the message is
    /// the parser's own, so it reads the same as it would from running it.
    #[test]
    fn a_malformed_script_is_refused_by_both() {
        let err = tokens("puts {unclosed").expect_err("refused");
        assert!(err.contains("missing close-brace"), "{err}");
        assert!(ast("puts \"unclosed").is_err());
    }
}
