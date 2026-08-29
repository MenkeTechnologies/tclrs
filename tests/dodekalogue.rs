//! Rule-by-rule tests for the twelve syntax rules of `Tcl(n)`.
//!
//! Every expectation here was taken from tclsh 9.0.4, not from reasoning about
//! the man page: the cases that distinguish plausible readings (how far `\x`
//! scans, whether `{a\}b}` keeps its backslash, when `{*}` expands) were run
//! through the interpreter first.

use tclrs::{parse, Part, Word};

/// The words of a single-command script.
fn words(src: &str) -> Vec<Word> {
    let script = parse(src).unwrap_or_else(|e| panic!("parse {src:?} failed: {e}"));
    assert_eq!(
        script.commands.len(),
        1,
        "expected one command in {src:?}, got {:#?}",
        script.commands
    );
    script.commands.into_iter().next().unwrap().words
}

/// The literal text of every word of a single-command script.
fn lits(src: &str) -> Vec<String> {
    words(src)
        .iter()
        .map(|w| {
            w.as_literal()
                .unwrap_or_else(|| panic!("word {w:?} is not literal"))
                .to_string()
        })
        .collect()
}

fn err(src: &str) -> String {
    parse(src).expect_err("expected a parse error").msg
}

#[test]
fn rule1_commands_split_on_newlines_and_semicolons() {
    let script = parse("set a 1; set b 2\nset c 3").unwrap();
    assert_eq!(script.commands.len(), 3);
    assert_eq!(script.commands[2].line, 2, "line numbers track newlines");
    // Blank lines and stray semicolons produce no empty commands.
    assert_eq!(parse(";\n\n;  ;\n").unwrap().commands.len(), 0);
}

#[test]
fn rule3_and_4_words_split_on_whitespace_but_not_inside_quotes() {
    assert_eq!(lits("puts a b"), ["puts", "a", "b"]);
    // A quoted word keeps spaces, semicolons and newlines as ordinary text.
    assert_eq!(lits("puts \"a b;c\nd\""), ["puts", "a b;c\nd"]);
    // Quotes are only special at the start of a word.
    assert_eq!(lits("set v ab\"cd"), ["set", "v", "ab\"cd"]);
}

#[test]
fn rule5_expansion_needs_a_nonspace_follower() {
    let w = words("list {*}{a b} c");
    assert!(w[1].expand, "{{*}} before a word marks it for expansion");
    assert_eq!(w[1].as_literal(), Some("a b"));
    assert!(!w[2].expand);

    // Followed by whitespace or a terminator it is just a braced word `*`,
    // matching `list {*} x` -> `* x` and `list {*};` -> `*`.
    let w = words("list {*} x");
    assert!(!w[1].expand);
    assert_eq!(w[1].as_literal(), Some("*"));
    assert!(!words("list {*};")[1].expand);

    // The remainder after `{*}` is parsed as any other word, so a quote there
    // still opens a quoted word: `list {*}"a b" c` yields three arguments.
    let w = words("list {*}\"a b\" c");
    assert!(w[1].expand && w[1].quoted);
    assert_eq!(w[1].as_literal(), Some("a b"));
}

#[test]
fn rule6_braces_nest_and_suppress_substitution() {
    assert_eq!(lits("set v {a {b c} d}"), ["set", "v", "a {b c} d"]);
    // No substitution inside braces, and the backslash before a brace is kept
    // even though it does not count toward nesting.
    assert_eq!(lits("set v {$x [y] \\}z}"), ["set", "v", "$x [y] \\}z"]);
    // Rule 9's pre-pass is the one thing that does apply inside braces.
    assert_eq!(lits("set v {a\\\n   b}"), ["set", "v", "a b"]);
    // A brace that does not start a word is ordinary text.
    assert_eq!(lits("set v ab{cd"), ["set", "v", "ab{cd"]);
}

#[test]
fn rule7_command_substitution_nests() {
    let w = words("set v [list [expr {1+1}] b]");
    let Part::Script(inner) = &w[2].parts[0] else {
        panic!("expected a nested script, got {:?}", w[2].parts);
    };
    assert_eq!(inner.commands.len(), 1);
    let Part::Script(innermost) = &inner.commands[0].words[1].parts[0] else {
        panic!("expected a doubly nested script");
    };
    assert_eq!(innermost.commands[0].words[0].as_literal(), Some("expr"));

    // A close bracket inside quotes belongs to the quoted word, not the
    // enclosing command substitution.
    let w = words("set v [list \"a]b\"]");
    let Part::Script(inner) = &w[2].parts[0] else {
        panic!("expected a nested script");
    };
    assert_eq!(inner.commands[0].words[1].as_literal(), Some("a]b"));

    // Brackets are inert inside braces.
    assert_eq!(lits("set v {[list a]}"), ["set", "v", "[list a]"]);
}

#[test]
fn rule8_variable_forms() {
    assert_eq!(words("puts $x")[1].parts, vec![Part::Var("x".into())]);
    assert_eq!(
        words("puts ${a b}")[1].parts,
        vec![Part::Var("a b".into())],
        "a braced name is taken verbatim"
    );
    // The name ends at the close brace that BALANCES the ones inside it, not at
    // the first one — `Tcl_ParseVarName` keeps a `braceCount`
    // (generic/tclParse.c:1383-1416). Reading it as "up to the first `}`" made
    // `${a{b}c}` the variable `a{b`, and left the rest of the script to be
    // re-parsed as though the name had ended there.
    assert_eq!(
        words("puts ${a{b}c}")[1].parts,
        vec![Part::Var("a{b}c".into())],
        "a nested group's close brace does not end the name"
    );
    assert_eq!(
        words("puts ${a{b}c{d}e}")[1].parts,
        vec![Part::Var("a{b}c{d}e".into())],
        "each group balances independently"
    );
    // A backslash consumes the byte after it, so an escaped brace neither ends
    // the name nor nests — and BOTH bytes stay in the name, which is what makes
    // `${a\}b}` a different variable from the one `set "a\}b"` creates.
    assert_eq!(
        words("puts ${a\\}b}")[1].parts,
        vec![Part::Var("a\\}b".into())],
        "an escaped brace is part of the name"
    );
    assert_eq!(
        words("puts $::ns::v")[1].parts,
        vec![Part::Var("::ns::v".into())],
        "namespace separators are part of the name"
    );
    // A single colon ends the name: `$b:x` is `$b` followed by `:x`.
    assert_eq!(
        words("puts $b:x")[1].parts,
        vec![Part::Var("b".into()), Part::Lit(":x".into())]
    );
    // An index is substituted; a braced index is not.
    assert_eq!(
        words("puts $a($i)")[1].parts,
        vec![Part::Elem {
            name: "a".into(),
            index: vec![Part::Var("i".into())],
        }]
    );
    assert_eq!(
        words("puts ${a(x(y))}")[1].parts,
        vec![Part::Elem {
            name: "a".into(),
            index: vec![Part::Lit("x(y)".into())],
        }],
        "the braced form takes the index verbatim"
    );
    assert_eq!(
        words("puts $a()")[1].parts,
        vec![Part::Elem {
            name: "a".into(),
            index: vec![],
        }]
    );
    // A dollar sign that starts no name is literal text.
    assert_eq!(lits("set v \"cost: $ 5\""), ["set", "v", "cost: $ 5"]);
}

#[test]
fn rule9_backslash_escapes_stop_at_the_documented_width() {
    // \x takes at most two hex digits, \ooo at most three octal digits, and
    // \U stops before the value would leave the Unicode range.
    assert_eq!(lits("set v \\x41BC"), ["set", "v", "ABC"]);
    assert_eq!(lits("set v \\1011"), ["set", "v", "A1"]);
    assert_eq!(lits("set v \\u00411"), ["set", "v", "A1"]);
    assert_eq!(lits("set v \\U000041B"), ["set", "v", "\u{41b}"]);
    assert_eq!(
        lits("set v \\a\\b\\f\\n\\r\\t\\v"),
        ["set", "v", "\u{7}\u{8}\u{c}\n\r\t\u{b}"]
    );
    // Anything else drops the backslash and keeps the character, which is how
    // spaces, quotes and dollars get into bare words.
    assert_eq!(lits("set v \\q\\;\\$"), ["set", "v", "q;$"]);
    assert_eq!(lits("set v a\\ b"), ["set", "v", "a b"]);
    // \u with no hex digit at all is just the letter.
    assert_eq!(lits("set v \\u{1F600}"), ["set", "v", "u{1F600}"]);
}

#[test]
fn rule9_backslash_newline_is_a_separator_outside_quotes() {
    // Between words it folds to a space, so the command continues on the next
    // line as separate words rather than one joined word.
    assert_eq!(lits("puts a\\\n   b"), ["puts", "a", "b"]);
    // Inside quotes the folded space is text.
    assert_eq!(lits("puts \"a\\\n   b\""), ["puts", "a b"]);
    // A command may start on a continued line.
    let script = parse("set a 1 \\\n  2").unwrap();
    assert_eq!(script.commands.len(), 1);
    assert_eq!(script.commands[0].words.len(), 4);
}

#[test]
fn rule10_hash_is_a_comment_only_in_first_word_position() {
    let script = parse("# comment\nputs hi ;# trailing\nputs there").unwrap();
    assert_eq!(script.commands.len(), 2);
    assert_eq!(script.commands[0].words[1].as_literal(), Some("hi"));
    // Mid-command a hash is an ordinary word.
    assert_eq!(lits("puts # hi"), ["puts", "#", "hi"]);
    // A comment ended by a continuation keeps swallowing the next line.
    assert_eq!(
        parse("# a \\\n still comment\nputs hi")
            .unwrap()
            .commands
            .len(),
        1
    );
}

#[test]
fn rules11_and_12_substitution_does_not_split_words() {
    // One word, three parts — the value of `$b` cannot introduce a new word.
    let w = words("puts a$b[c]");
    assert_eq!(w.len(), 2);
    assert_eq!(
        w[1].parts.len(),
        3,
        "literal, variable and script parts stay in one word: {:?}",
        w[1].parts
    );
    // Each character is consumed once: the `$` inside a nested script belongs
    // to that script, not to the outer word.
    let w = words("puts [set x $y]z");
    assert_eq!(w[1].parts.len(), 2);
}

#[test]
fn parse_errors_match_the_interpreter_wording() {
    assert_eq!(err("list {abc"), "missing close-brace");
    assert_eq!(err("list [a b"), "missing close-bracket");
    assert_eq!(err("list \"abc"), "missing \"");
    assert_eq!(err("set v $a("), "missing )");
    assert_eq!(err("set v {a}b"), "extra characters after close-brace");
    assert_eq!(err("set v \"a\"b"), "extra characters after close-quote");
    assert_eq!(err("set v $a(x(y))"), "invalid character in array index");
    assert_eq!(err("set v ${abc"), "missing close-brace for variable name");
    // Running out of text while a group is still open is the same failure, and
    // is what `puts ${` followed by a line holding a balanced `{…}` reports —
    // stopping at that group's `}` instead swallowed the rest of the script
    // into the variable's name.
    assert_eq!(
        err("puts ${\neval {puts a}\n"),
        "missing close-brace for variable name"
    );
    assert_eq!(err("set v ${a{b}"), "missing close-brace for variable name");
    // The error carries the line it was found on.
    assert_eq!(parse("set a 1\nset b {x").unwrap_err().line, 2);
}

#[test]
fn empty_and_degenerate_inputs() {
    assert!(parse("").unwrap().commands.is_empty());
    assert!(parse("   \n\t\n").unwrap().commands.is_empty());
    assert_eq!(lits("puts {}"), ["puts", ""]);
    assert_eq!(lits("puts \"\""), ["puts", ""]);
    // An empty word is still a word.
    assert_eq!(words("puts {} {}").len(), 3);
}

#[test]
fn braced_bodies_are_marked_for_static_compilation() {
    // The compiler keys off `braced` to decide whether a body or expression can
    // be compiled at parse time instead of assembled and parsed at runtime.
    let w = words("if {$x > 1} {puts big}");
    assert!(w[1].braced && w[2].braced);
    assert!(!words("expr $a + $b")[1].braced);
}

#[test]
fn utf8_survives_words_and_braces() {
    assert_eq!(lits("puts \"héllo wörld\""), ["puts", "héllo wörld"]);
    assert_eq!(lits("puts {日本語}"), ["puts", "日本語"]);
    assert_eq!(lits("puts \\é"), ["puts", "é"]);
}
