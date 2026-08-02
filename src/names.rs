//! The command vocabulary, for whoever needs to offer it rather than compile
//! it.
//!
//! The compiler learns a command name by matching it — `compiler.rs` for the
//! ones it lowers itself, `cmd_list.rs` for the rest — and an ensemble learns a
//! subcommand by resolving it against its own table. None of those are lists
//! anyone can read: the REPL's completer needs one, so it is assembled here
//! from the same constants the matches are written beside, never copied.
//!
//! Nothing in the compile path calls this module. It exists so that a name
//! offered at the prompt is a name the compiler knows.

use crate::assoc::{ARRAY_SUBCOMMANDS, DICT_SUBCOMMANDS};
use crate::cmd_list;
use crate::cmd_string;
use crate::compiler::Compiler;

/// Every command name the compiler accepts: the ones it lowers itself, then
/// the list commands it forwards. Sorted and free of duplicates, because a
/// completion menu is read by eye.
pub fn commands() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = Compiler::BUILTINS
        .iter()
        .copied()
        .chain(cmd_list::COMMANDS.iter().copied())
        .chain(crate::regexp::COMMANDS.iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// One command: its name, the synopsis the compiler reports when the argument
/// count is wrong, and what it does in a line.
///
/// The synopsis is the wording of the `wrong # args` message at the command's
/// own compile site, so a reader who provokes the error sees the same text the
/// reference page prints.
pub struct Entry {
    pub name: &'static str,
    pub synopsis: &'static str,
    pub summary: &'static str,
}

/// The documented command set, in the order [`commands`] produces.
///
/// A description cannot be derived from the compiler the way a name can, so
/// this table is written by hand — and pinned to [`commands`] by a test, which
/// is what keeps it from listing a command the compiler refuses or omitting one
/// it accepts. Nothing in the compile path reads it; `gen-docs` renders it.
pub const CORPUS: &[Entry] = &[
    Entry {
        name: "after",
        synopsis: "after ms|cancel|idle|info ?arg ...?",
        summary: "Register a script to run after a delay or when nothing else is pending, cancel one, or list what is registered. See the module note in src/cmd_after.rs for which event sources a build has.",
    },
    Entry {
        name: "append",
        synopsis: "append varName ?value ...?",
        summary: "Append every value to the variable's string; yields the new value.",
    },
    Entry {
        name: "apply",
        synopsis: "apply lambdaExpr ?arg ...?",
        summary: "Call an anonymous procedure. The lambda has to be written out, because its body is compiled with the script that contains it.",
    },
    Entry {
        name: "array",
        synopsis: "array subcommand ?arg ...?",
        summary: "The array ensemble, over a variable rather than a value: an array is never itself a value.",
    },
    Entry {
        name: "break",
        synopsis: "break",
        summary: "Leave the innermost loop. The stack is unwound by a count the compiler knows statically.",
    },
    Entry {
        name: "catch",
        synopsis: "catch script ?resultVarName?",
        summary: "Run the script and trap an error from it, including one raised inside a procedure it called; yields the completion code.",
    },
    Entry {
        name: "concat",
        synopsis: "concat ?arg ...?",
        summary: "Trim each argument of surrounding whitespace and join them with single spaces into one list.",
    },
    Entry {
        name: "continue",
        synopsis: "continue",
        summary: "Begin the innermost loop's next iteration; in a `for`, its step still runs first.",
    },
    Entry {
        name: "coroutine",
        synopsis: "coroutine name cmd ?arg ...?",
        summary: "Create a coroutine context — a second VM over the same chunk — running one of the script's procedures, and enter it.",
    },
    Entry {
        name: "dict",
        synopsis: "dict subcommand ?arg ...?",
        summary: "The dict ensemble, over a value: a dict is a list of alternating keys and values, so it can be passed and printed like any string.",
    },
    Entry {
        name: "error",
        synopsis: "error message",
        summary: "Raise an error carrying the message.",
    },
    Entry {
        name: "eval",
        synopsis: "eval arg ?arg ...?",
        summary: "Concatenate the arguments and run the result as a script against the interpreter's own variables. The chunk is cached by source text.",
    },
    Entry {
        name: "expr",
        synopsis: "expr arg ?arg ...?",
        summary: "Evaluate the arguments as an expression. A braced argument is compiled once, not re-parsed per evaluation.",
    },
    Entry {
        name: "for",
        synopsis: "for start test next body",
        summary: "Run start, then the body while test holds, running next after each iteration. Emitted rotated, like every loop here.",
    },
    Entry {
        name: "foreach",
        synopsis: "foreach varList list ?varList list ...? command",
        summary: "Iterate over one or more lists in parallel; the longest fixes the count and shorter ones supply empty values.",
    },
    Entry {
        name: "format",
        synopsis: "format formatString ?arg ...?",
        summary: "Format the arguments the way `sprintf` does, with Tcl's conversion set.",
    },
    Entry {
        name: "global",
        synopsis: "global ?varName ...?",
        summary: "Address the named variables in the global table rather than the procedure's frame slots. Outside a procedure it does nothing.",
    },
    Entry {
        name: "if",
        synopsis: "if test ?then? body ?elseif test ?then? body ...? ?else? ?body?",
        summary: "The first branch whose test is a true Tcl boolean; a test that is not a boolean is an error, not a false branch.",
    },
    Entry {
        name: "incr",
        synopsis: "incr varName ?increment?",
        summary: "Add the increment (1 by default) to the variable; yields the new value.",
    },
    Entry {
        name: "info",
        synopsis: "info subcommand ?argument ...?",
        summary: "Interpreter introspection. Only `info coroutine` is implemented; every other subcommand is refused by name.",
    },
    Entry {
        name: "join",
        synopsis: "join list ?joinString?",
        summary: "Concatenate the list's elements, separated by joinString (a space by default).",
    },
    Entry {
        name: "lappend",
        synopsis: "lappend varName ?value ...?",
        summary: "Append each value to the list held in the variable; yields the new list.",
    },
    Entry {
        name: "lassign",
        synopsis: "lassign list ?varName ...?",
        summary: "Assign the elements to the variables in order; yields the unassigned remainder.",
    },
    Entry {
        name: "ledit",
        synopsis: "ledit listVar first last ?element ...?",
        summary: "Replace the range in the variable's list with the elements; yields the new list.",
    },
    Entry {
        name: "lindex",
        synopsis: "lindex list ?index ...?",
        summary: "The element at an index path, or the list itself when no index is given.",
    },
    Entry {
        name: "linsert",
        synopsis: "linsert list index ?element ...?",
        summary: "A copy of the list with the elements inserted before the index.",
    },
    Entry {
        name: "list",
        synopsis: "list ?arg ...?",
        summary: "A list of the arguments, quoted by the reference algorithm so it reads back as the same elements.",
    },
    Entry {
        name: "llength",
        synopsis: "llength list",
        summary: "How many elements the list has.",
    },
    Entry {
        name: "lmap",
        synopsis: "lmap varList list ?varList list ...? command",
        summary: "`foreach` that collects each iteration's value into a list; `continue` contributes nothing.",
    },
    Entry {
        name: "lpop",
        synopsis: "lpop listvar ?index ...?",
        summary: "Remove the element at an index path from the variable's list and yield it.",
    },
    Entry {
        name: "lrange",
        synopsis: "lrange list first last",
        summary: "The sublist between two indices, inclusive.",
    },
    Entry {
        name: "lremove",
        synopsis: "lremove list ?index ...?",
        summary: "A copy of the list without the indexed elements; an index outside it is ignored.",
    },
    Entry {
        name: "lrepeat",
        synopsis: "lrepeat count ?value ...?",
        summary: "The values repeated count times, as one list.",
    },
    Entry {
        name: "lreplace",
        synopsis: "lreplace list first last ?element ...?",
        summary: "A copy of the list with the range replaced by the elements.",
    },
    Entry {
        name: "lreverse",
        synopsis: "lreverse list",
        summary: "The list, reversed.",
    },
    Entry {
        name: "lsearch",
        synopsis: "lsearch ?-option value ...? list pattern",
        summary: "The index of the first matching element, or -1. Option parsing is the reference one, abbreviation included.",
    },
    Entry {
        name: "lseq",
        synopsis: "lseq n ??op? n ??by? n??",
        summary: "An arithmetic sequence; a zero step yields one element and a step pointing away from the end yields none.",
    },
    Entry {
        name: "lset",
        synopsis: "lset listVar ?index? ?index ...? value",
        summary: "Replace the element at an index path in the variable's list; the end grows by one element only.",
    },
    Entry {
        name: "lsort",
        synopsis: "lsort ?-option value ...? list",
        summary: "The list sorted by the reference merge sort — the algorithm, not just the ordering, because `-unique` observes it.",
    },
    Entry {
        name: "proc",
        synopsis: "proc name args body",
        summary: "Define a procedure. Parameters and locals are frame slots; defaults and a trailing `args` are resolved at the call site.",
    },
    Entry {
        name: "puts",
        synopsis: "puts ?-nonewline? string",
        summary: "Write the string to stdout.",
    },
    Entry {
        name: "regexp",
        synopsis: "regexp ?-option ...? exp string ?matchVar? ?subMatchVar ...?",
        summary: "Match a regular expression, setting the match variables; the count under -all, the text under -inline.",
    },
    Entry {
        name: "regsub",
        synopsis: "regsub ?-option ...? exp string subSpec ?varName?",
        summary: "Substitute for a regular expression's matches; the new string, or the count when a variable is named.",
    },
    Entry {
        name: "return",
        synopsis: "return ?-code code? ?result?",
        summary: "Return from the enclosing procedure with the result. `-code ok` and `-code error` are the codes implemented.",
    },
    Entry {
        name: "set",
        synopsis: "set varName ?newValue?",
        summary: "Read or write a variable; yields its value. A procedure's variables are slots, a script's are VM globals.",
    },
    Entry {
        name: "split",
        synopsis: "split string ?splitChars?",
        summary: "Split the string at any of the characters into a list.",
    },
    Entry {
        name: "string",
        synopsis: "string subcommand ?arg ...?",
        summary: "The string ensemble. Subcommands, options and the `string is` class are resolved while compiling.",
    },
    Entry {
        name: "switch",
        synopsis: "switch ?options? string {pattern body ...}",
        summary: "Run the body of the first pattern that matches, `-exact` or `-glob`.",
    },
    Entry {
        name: "unset",
        synopsis: "unset ?-nocomplain? ?--? ?name ...?",
        summary: "Remove variables or array elements.",
    },
    Entry {
        name: "update",
        synopsis: "update ?idletasks?",
        summary: "Service everything that is pending and return; idletasks services only the idle handlers.",
    },
    Entry {
        name: "uplevel",
        synopsis: "uplevel ?level? command ?arg ...?",
        summary: "Run a script at another level. The level is resolved when the command runs, and one that is a procedure activation is refused rather than served against the wrong variables.",
    },
    Entry {
        name: "upvar",
        synopsis: "upvar ?level? otherVar localVar ?otherVar localVar ...?",
        summary: "Bind a local name to a global. Only level #0 can be bound, because the binding is made while the script is read.",
    },
    Entry {
        name: "variable",
        synopsis: "variable ?name value ...? name ?value?",
        summary: "Declare a namespace variable, which without namespaces is a global, and optionally give it a value.",
    },
    Entry {
        name: "vwait",
        synopsis: "vwait ?varName?",
        summary: "Service events until the named global is written. With no name it is update.",
    },
    Entry {
        name: "while",
        synopsis: "while test command",
        summary: "Run the body while the test holds. Inside a procedure this is the loop that reaches a compiled trace.",
    },
    Entry {
        name: "yield",
        synopsis: "yield ?value?",
        summary: "Suspend the running coroutine and hand the value to whoever resumed it.",
    },
    Entry {
        name: "yieldto",
        synopsis: "yieldto command ?arg ...?",
        summary: "Suspend and donate the resumer to another coroutine of the script, which is what makes the transfer symmetric rather than a queue.",
    },
];

/// The subcommands of an ensemble command, or an empty slice for a command
/// that has none. `info` is not an ensemble here in the way the others are —
/// the frontend supports exactly one of its subcommands (`coro.rs`) — so that
/// one word is what it offers.
pub fn subcommands(command: &str) -> &'static [&'static str] {
    match command {
        "string" => cmd_string::SUBCOMMANDS,
        "array" => ARRAY_SUBCOMMANDS,
        "dict" => DICT_SUBCOMMANDS,
        "info" => INFO_SUBCOMMANDS,
        _ => &[],
    }
}

/// The documented subcommands of an ensemble, in the order [`subcommands`]
/// lists them.
///
/// An ensemble's table is the set of names it *resolves*, which is wider than
/// the set it lowers: a name is listed so that an abbreviation of it can be
/// found ambiguous, and then refused with `is not supported yet`. Those are
/// documented here as refused, because a completion menu that silently omitted
/// them would make an abbreviation's ambiguity unexplainable.
pub fn subcommand_corpus(command: &str) -> &'static [Entry] {
    match command {
        "string" => STRING_CORPUS,
        "array" => ARRAY_CORPUS,
        "dict" => DICT_CORPUS,
        "info" => INFO_CORPUS,
        _ => &[],
    }
}

const STRING_CORPUS: &[Entry] = &[
    Entry {
        name: "cat",
        synopsis: "string cat ?string ...?",
        summary: "Concatenate the arguments; no arguments yields the empty string.",
    },
    Entry {
        name: "compare",
        synopsis: "string compare ?-nocase? ?-length int? string1 string2",
        summary: "-1, 0 or 1 for the ordering of the two strings.",
    },
    Entry {
        name: "equal",
        synopsis: "string equal ?-nocase? ?-length int? string1 string2",
        summary: "1 when the two strings are the same, 0 otherwise.",
    },
    Entry {
        name: "first",
        synopsis: "string first needleString haystackString ?startIndex?",
        summary: "The index of the first occurrence of the needle, or -1.",
    },
    Entry {
        name: "index",
        synopsis: "string index string charIndex",
        summary: "The character at the index, or the empty string when out of range.",
    },
    Entry {
        name: "insert",
        synopsis: "string insert string index insertString",
        summary: "The string with insertString placed before the index.",
    },
    Entry {
        name: "is",
        synopsis: "string is class ?-strict? ?-failindex var? str",
        summary: "Whether the string belongs to a character class. The class is resolved while compiling.",
    },
    Entry {
        name: "last",
        synopsis: "string last needleString haystackString ?lastIndex?",
        summary: "The index of the last occurrence of the needle, or -1.",
    },
    Entry {
        name: "length",
        synopsis: "string length string",
        summary: "How many characters the string has.",
    },
    Entry {
        name: "map",
        synopsis: "string map ?-nocase? mapping string",
        summary: "Replace every key of the mapping list with its value, scanning left to right.",
    },
    Entry {
        name: "match",
        synopsis: "string match ?-nocase? pattern string",
        summary: "Whether the glob pattern matches the whole string.",
    },
    Entry {
        name: "range",
        synopsis: "string range string first last",
        summary: "The characters between two indices, inclusive.",
    },
    Entry {
        name: "repeat",
        synopsis: "string repeat string count",
        summary: "The string repeated count times.",
    },
    Entry {
        name: "replace",
        synopsis: "string replace string first last ?string?",
        summary: "The string with the range replaced by the fourth argument, or removed.",
    },
    Entry {
        name: "reverse",
        synopsis: "string reverse string",
        summary: "The string, reversed by character.",
    },
    Entry {
        name: "tolower",
        synopsis: "string tolower string ?first? ?last?",
        summary: "The string in lower case, or only the range given.",
    },
    Entry {
        name: "totitle",
        synopsis: "string totitle string ?first? ?last?",
        summary: "The string with its first character in title case and the rest lowered.",
    },
    Entry {
        name: "toupper",
        synopsis: "string toupper string ?first? ?last?",
        summary: "The string in upper case, or only the range given.",
    },
    Entry {
        name: "trim",
        synopsis: "string trim string ?chars?",
        summary: "The string without leading and trailing characters from the set (whitespace by default).",
    },
    Entry {
        name: "trimleft",
        synopsis: "string trimleft string ?chars?",
        summary: "The string without leading characters from the set.",
    },
    Entry {
        name: "trimright",
        synopsis: "string trimright string ?chars?",
        summary: "The string without trailing characters from the set.",
    },
    Entry {
        name: "wordend",
        synopsis: "string wordend string charIndex",
        summary: "Refused: not built yet. Listed because its presence decides whether an abbreviation is ambiguous.",
    },
    Entry {
        name: "wordstart",
        synopsis: "string wordstart string charIndex",
        summary: "Refused: not built yet. Listed for the same reason as `wordend`.",
    },
];

const ARRAY_CORPUS: &[Entry] = &[
    Entry {
        name: "anymore",
        synopsis: "array anymore arrayName searchId",
        summary: "Refused: not built yet — the search-token subcommands need a cursor the frontend does not keep.",
    },
    Entry {
        name: "default",
        synopsis: "array default subcommand arrayName ?value?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "donesearch",
        synopsis: "array donesearch arrayName searchId",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "exists",
        synopsis: "array exists arrayName",
        summary: "Whether the variable exists and holds an array.",
    },
    Entry {
        name: "for",
        synopsis: "array for {keyVar valueVar} arrayName script",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "get",
        synopsis: "array get arrayName ?pattern?",
        summary: "The array as a flat list of alternating names and values.",
    },
    Entry {
        name: "names",
        synopsis: "array names arrayName ?pattern?",
        summary: "The element names, optionally those matching a glob pattern.",
    },
    Entry {
        name: "nextelement",
        synopsis: "array nextelement arrayName searchId",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "set",
        synopsis: "array set arrayName list",
        summary: "Set elements from a flat list of alternating names and values.",
    },
    Entry {
        name: "size",
        synopsis: "array size arrayName",
        summary: "How many elements the array has.",
    },
    Entry {
        name: "startsearch",
        synopsis: "array startsearch arrayName",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "statistics",
        synopsis: "array statistics arrayName",
        summary: "Refused: not built yet — it reports on the reference interpreter's own hash table.",
    },
    Entry {
        name: "unset",
        synopsis: "array unset arrayName ?pattern?",
        summary: "Remove matching elements, or the whole array when no pattern is given.",
    },
];

const DICT_CORPUS: &[Entry] = &[
    Entry {
        name: "append",
        synopsis: "dict append dictVarName key ?string ...?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "create",
        synopsis: "dict create ?key value ...?",
        summary: "A dict of the given pairs.",
    },
    Entry {
        name: "exists",
        synopsis: "dict exists dictionary key ?key ...?",
        summary: "Whether the key path resolves.",
    },
    Entry {
        name: "filter",
        synopsis: "dict filter dictionary filterType arg ?arg ...?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "for",
        synopsis: "dict for {keyVarName valueVarName} dictionary script",
        summary: "Run the script for each pair, with the two variables bound.",
    },
    Entry {
        name: "get",
        synopsis: "dict get dictionary ?key ...?",
        summary: "The value at the key path; the whole dict when no key is given.",
    },
    Entry {
        name: "getdef",
        synopsis: "dict getdef dictionary ?key ...? key default",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "getwithdefault",
        synopsis: "dict getwithdefault dictionary ?key ...? key default",
        summary: "Refused: not built yet — the long spelling of `getdef`.",
    },
    Entry {
        name: "incr",
        synopsis: "dict incr dictVarName key ?increment?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "info",
        synopsis: "dict info dictionary",
        summary: "Refused: not built yet — it reports on the reference interpreter's hash table.",
    },
    Entry {
        name: "keys",
        synopsis: "dict keys dictionary ?pattern?",
        summary: "The keys, optionally those matching a glob pattern.",
    },
    Entry {
        name: "lappend",
        synopsis: "dict lappend dictVarName key ?value ...?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "map",
        synopsis: "dict map {keyVarName valueVarName} dictionary script",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "merge",
        synopsis: "dict merge ?dictionary ...?",
        summary: "One dict of all the arguments, later keys winning.",
    },
    Entry {
        name: "remove",
        synopsis: "dict remove dictionary ?key ...?",
        summary: "The dict without the given keys.",
    },
    Entry {
        name: "replace",
        synopsis: "dict replace dictionary ?key value ...?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "set",
        synopsis: "dict set dictVarName key ?key ...? value",
        summary: "Set the value at a key path in the dict held by the variable.",
    },
    Entry {
        name: "size",
        synopsis: "dict size dictionary",
        summary: "How many pairs the dict has.",
    },
    Entry {
        name: "unset",
        synopsis: "dict unset dictVarName key ?key ...?",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "update",
        synopsis: "dict update dictVarName key varName ?key varName ...? script",
        summary: "Refused: not built yet.",
    },
    Entry {
        name: "values",
        synopsis: "dict values dictionary ?pattern?",
        summary: "The values, optionally those whose string matches a glob pattern.",
    },
    Entry {
        name: "with",
        synopsis: "dict with dictVarName ?key ...? script",
        summary: "Refused: not built yet.",
    },
];

/// The `info` subcommands this frontend answers, in the order the corpus below
/// documents them. Not the whole of tclsh's table: the ones outside this set are
/// resolved by `crate::cmd_info` so that an abbreviation of one can still be
/// found ambiguous, and then refused by name.
const INFO_SUBCOMMANDS: &[&str] = &[
    "args",
    "body",
    "commands",
    "complete",
    "coroutine",
    "default",
    "exists",
    "globals",
    "hostname",
    "level",
    "locals",
    "nameofexecutable",
    "patchlevel",
    "procs",
    "script",
    "tclversion",
    "vars",
];

const INFO_CORPUS: &[Entry] = &[
    Entry {
        name: "args",
        synopsis: "info args procname",
        summary: "The formal parameter names of a procedure the script defines.",
    },
    Entry {
        name: "body",
        synopsis: "info body procname",
        summary: "The body text a procedure was defined with.",
    },
    Entry {
        name: "commands",
        synopsis: "info commands ?pattern?",
        summary: "Every command name the compiler answers to, including the script's own procedures.",
    },
    Entry {
        name: "complete",
        synopsis: "info complete command",
        summary: "Whether the text is a whole script, or has a construct still open at its end.",
    },
    Entry {
        name: "coroutine",
        synopsis: "info coroutine",
        summary: "The name of the running coroutine, or the empty string outside one.",
    },
    Entry {
        name: "default",
        synopsis: "info default procname arg varname",
        summary: "Whether a formal parameter has a default, storing it in the named variable.",
    },
    Entry {
        name: "exists",
        synopsis: "info exists varName",
        summary: "Whether the variable is set right now.",
    },
    Entry {
        name: "globals",
        synopsis: "info globals ?pattern?",
        summary: "Every variable the interpreter holds.",
    },
    Entry {
        name: "hostname",
        synopsis: "info hostname",
        summary: "The name the host answers to, as gethostname reports it.",
    },
    Entry {
        name: "level",
        synopsis: "info level",
        summary: "How many procedure activations are on the stack. The form taking a level number is refused.",
    },
    Entry {
        name: "locals",
        synopsis: "info locals ?pattern?",
        summary: "The procedure locals that are set, as far as the body has been read.",
    },
    Entry {
        name: "nameofexecutable",
        synopsis: "info nameofexecutable",
        summary: "The path of the running binary.",
    },
    Entry {
        name: "patchlevel",
        synopsis: "info patchlevel",
        summary: "The Tcl patch level this frontend implements.",
    },
    Entry {
        name: "procs",
        synopsis: "info procs ?pattern?",
        summary: "The procedures the script defines.",
    },
    Entry {
        name: "script",
        synopsis: "info script",
        summary: "The file the running script came from, or the empty string for a script given as text.",
    },
    Entry {
        name: "tclversion",
        synopsis: "info tclversion",
        summary: "The Tcl version this frontend implements.",
    },
    Entry {
        name: "vars",
        synopsis: "info vars ?pattern?",
        summary: "Every variable visible where the command runs.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_sorted_and_cover_both_halves() {
        let all = commands();
        assert!(all.windows(2).all(|w| w[0] < w[1]), "not sorted or deduped");
        // One name from the compiler's own match, one it forwards to cmd_list.
        assert!(all.contains(&"proc"));
        assert!(all.contains(&"lsort"));
    }

    /// Every name offered has to be one the compiler knows — the point of the
    /// module. `invalid command name` is the compiler's answer for a name it
    /// does not; any other complaint (wrong argument count, say) means the name
    /// itself was recognized.
    #[test]
    fn every_offered_command_is_known_to_the_compiler() {
        for name in commands() {
            let err = crate::runtime::compile(name).err().unwrap_or_default();
            assert!(
                !err.contains("invalid command name"),
                "{name} is offered for completion but not compiled: {err}"
            );
        }
    }

    /// The reference page is generated from [`CORPUS`], so a command the
    /// compiler accepts and the table omits would be missing from the page, and
    /// one the table invents would be claimed and then refused. Neither is
    /// allowed to happen quietly.
    #[test]
    fn corpus_documents_exactly_the_commands_the_compiler_accepts() {
        let documented: Vec<&str> = CORPUS.iter().map(|e| e.name).collect();
        assert_eq!(documented, commands(), "CORPUS and commands() disagree");
        assert!(
            CORPUS.iter().all(|e| e.synopsis.starts_with(e.name)),
            "a synopsis has to start with the command it is for"
        );
    }

    #[test]
    fn ensembles_offer_their_subcommands_and_others_offer_none() {
        assert!(subcommands("string").contains(&"toupper"));
        assert!(subcommands("array").contains(&"names"));
        assert!(subcommands("dict").contains(&"keys"));
        assert!(subcommands("info").contains(&"coroutine"));
        assert!(subcommands("puts").is_empty());
    }
}
