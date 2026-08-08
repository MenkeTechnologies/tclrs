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
use crate::cmd_clock;
use crate::cmd_file;
use crate::cmd_info;
use crate::cmd_list;
use crate::cmd_string;
use crate::compiler::Compiler;

/// Every table a command name can come from: the ones the compiler lowers
/// itself, then each command module's own list.
///
/// One list, read by both [`commands`] and [`is_command`], because they are the
/// same question asked twice and the two had already drifted: `encoding` was in
/// the vocabulary the completer offers and not in the one
/// [`crate::procs::expand_call_op`] dispatches through, so `encoding {*}$a`
/// answered `invalid command name "encoding"` where tclsh answers `utf-8`. A
/// module added here is added to both.
const TABLES: &[&[&str]] = &[
    Compiler::BUILTINS,
    cmd_list::COMMANDS,
    crate::cmd_channel::COMMANDS,
    crate::regexp::COMMANDS,
    cmd_clock::COMMANDS,
    cmd_file::COMMANDS,
    crate::cmd_encoding::COMMANDS,
];

/// Every command name the compiler accepts. Sorted and free of duplicates,
/// because a completion menu is read by eye.
pub fn commands() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = TABLES.iter().flat_map(|t| t.iter().copied()).collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// Whether the compiler lowers a command of this name.
///
/// [`commands`] answers the same question by building the whole sorted
/// vocabulary, which is what a completion menu wants and what a dispatch decision
/// must not do: [`crate::procs::expand_call_op`] asks this per call, for a name
/// only the running script knows. Both read [`TABLES`], so neither can be the
/// only one that knows about a command.
pub fn is_command(name: &str) -> bool {
    TABLES.iter().any(|t| t.contains(&name))
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
        summary: "Run a lambda — a list of parameters, a body and an optional namespace — as what it is: a procedure body with a frame of its own. The lambda may be computed. A wrong argument count is reported against the lambda, which has no name to report.",
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
        name: "cd",
        synopsis: "cd ?dirName?",
        summary: "Change the working directory; no argument means the home directory.",
    },
    Entry {
        name: "clock",
        synopsis: "clock subcommand ?arg ...?",
        summary: "The time ensemble: read the clock, and convert between an instant and a calendar.",
    },
    Entry {
        name: "close",
        synopsis: "close channel ?direction?",
        summary: "Drop a reference to a channel and close it once none is left; ?direction? half-closes a read-write channel.",
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
        name: "encoding",
        synopsis: "encoding subcommand ?arg ...?",
        summary: "The transcoding ensemble: convert between a byte string and a string, and report which encodings and error profiles exist.",
    },
    Entry {
        name: "eof",
        synopsis: "eof channel",
        summary: "Whether the channel's device reported end of file and nothing decoded is still buffered.",
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
        name: "fconfigure",
        synopsis: "fconfigure channel ?-option value ...?",
        summary: "Read or set a channel's generic options: -translation, -encoding, -buffering, -buffersize, -blocking.",
    },
    Entry {
        name: "file",
        synopsis: "file subcommand ?arg ...?",
        summary: "The path and filesystem ensemble; the path halves need nothing on disk.",
    },
    Entry {
        name: "flush",
        synopsis: "flush channel",
        summary: "Hand everything buffered for the channel to its device.",
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
        name: "gets",
        synopsis: "gets channel ?varName?",
        summary: "The next line without its terminator; with a variable, the line goes there and the count is the result.",
    },
    Entry {
        name: "glob",
        synopsis: "glob ?switches? ?pattern ...?",
        summary: "Every existing name a pattern matches, walked one path component at a time.",
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
        synopsis: "info subcommand ?arg ...?",
        summary: "Interpreter introspection. Variables, procedure signatures and bodies, command names, the call frame's level and its locals, the math functions, whether text is a complete command, and the versions; the subcommands naming machinery this frontend has none of — `frame`, `errorstack`, `cmdcount`, `cmdtype`, the object-system queries, `constant`, `loaded` — are refused by name rather than mis-answered.",
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
        name: "namespace",
        synopsis: "namespace subcommand ?arg ...?",
        summary: "The namespace ensemble. A namespace is resolved while compiling, so its variables and procedures take qualified names; the queries are answered from interpreter state.",
    },
    Entry {
        name: "open",
        synopsis: "open fileName ?access? ?permissions?",
        summary: "Open a file and return the channel's name. The command-pipeline form is refused.",
    },
    Entry {
        name: "package",
        synopsis: "package option ?arg ...?",
        summary: "The package ensemble: what a name is provided at, what would load it, and TIP 268's version arithmetic over both.",
    },
    Entry {
        name: "proc",
        synopsis: "proc name args body",
        summary: "Define a procedure. Parameters and locals are frame slots; defaults and a trailing `args` are resolved at the call site.",
    },
    Entry {
        name: "puts",
        synopsis: "puts ?-nonewline? ?channel? string",
        summary: "Write the string to a channel, or to stdout when none is named.",
    },
    Entry {
        name: "pwd",
        synopsis: "pwd",
        summary: "The working directory, as the last cd was told it rather than as getcwd reports it.",
    },
    Entry {
        name: "read",
        synopsis: "read channel ?numChars?",
        summary: "Read the whole channel, or that many characters; -nonewline drops the trailing newlines.",
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
        name: "rename",
        synopsis: "rename oldName newName",
        summary: "Rename a command, or delete it when the new name is empty. A call the same chunk compiled is guarded so a deleted command still refuses.",
    },
    Entry {
        name: "return",
        synopsis: "return ?-code code? ?result?",
        summary: "Return from the enclosing procedure with the result. `-code ok` and `-code error` are the codes implemented.",
    },
    Entry {
        name: "scan",
        synopsis: "scan string format ?varName ...?",
        summary: "Read the string the way `sscanf` does, with Tcl's conversion set. With variable names the result is how many conversions were assigned, and a variable whose conversion never ran is left untouched; with none, the converted values come back as a list.",
    },
    Entry {
        name: "seek",
        synopsis: "seek channel offset ?origin?",
        summary: "Move the device's position, discarding what was buffered and clearing end of file.",
    },
    Entry {
        name: "set",
        synopsis: "set varName ?newValue?",
        summary: "Read or write a variable; yields its value. A procedure's variables are slots, a script's are VM globals.",
    },
    Entry {
        name: "source",
        synopsis: "source ?-encoding encoding? fileName",
        summary: "Read a file and evaluate it against this interpreter; yields the value of its last command.",
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
        name: "tcl_findLibrary",
        synopsis: "tcl_findLibrary basename version patch initScript enVarName varName",
        summary: "Tcl's own library-directory search, ported from `library/auto.tcl`: find the initialisation script, set the library variable and source it.",
    },
    Entry {
        name: "tell",
        synopsis: "tell channel",
        summary: "The device's position, less whatever was read ahead of the script.",
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
        synopsis: "uplevel ?level? arg ?arg ...?",
        summary: "Concatenate the arguments and run the result as a script in the frame of a caller: #0 is the global level, a bare number counts calls outwards from this one. Which word is the level is decided when the command runs, so uplevel $n works. Only a procedure call is a level, so uplevel 1 at a script's top level is a bad level.",
    },
    Entry {
        name: "upvar",
        synopsis: "upvar ?level? otherVar localVar ?otherVar localVar ...?",
        summary: "Bind a local name to a variable at another level. The level and the target may both be computed, and an array element may be the target.",
    },
    Entry {
        name: "variable",
        synopsis: "variable ?name value ...? name ?value?",
        summary: "Declare a namespace variable. In a procedure body it links the local name to the namespace's variable rather than creating one.",
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
        "info" => cmd_info::SUBCOMMANDS,
        "namespace" => crate::cmd_namespace::SUBCOMMANDS,
        "package" => crate::cmd_package::SUBCOMMANDS,
        "clock" => cmd_clock::SUBCOMMANDS,
        "file" => cmd_file::SUBCOMMANDS,
        "encoding" => crate::cmd_encoding::SUBCOMMANDS,
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
        "clock" => CLOCK_CORPUS,
        "file" => FILE_CORPUS,
        "encoding" => ENCODING_CORPUS,
        "info" => INFO_CORPUS,
        "namespace" => NAMESPACE_CORPUS,
        "package" => PACKAGE_CORPUS,
        _ => &[],
    }
}

/// The `encoding` subcommands, in the order [`subcommands`] lists them.
const ENCODING_CORPUS: &[Entry] = &[
    Entry {
        name: "convertfrom",
        synopsis: "encoding convertfrom ?-profile profile? ?-failindex var? encoding data",
        summary: "A byte string in some encoding, as a string. The profile decides what an invalid sequence becomes; strict is the default.",
    },
    Entry {
        name: "convertto",
        synopsis: "encoding convertto ?-profile profile? ?-failindex var? encoding data",
        summary: "A string as a byte string in some encoding. The profile decides what an unrepresentable character becomes.",
    },
    Entry {
        name: "dirs",
        synopsis: "encoding dirs ?dirList?",
        summary: "The encoding search path. Starts empty here: the tables are inside the binary, so there is no directory to search.",
    },
    Entry {
        name: "names",
        synopsis: "encoding names",
        summary: "Every encoding this frontend can convert with, sorted. What it lists, it converts.",
    },
    Entry {
        name: "profiles",
        synopsis: "encoding profiles",
        summary: "The error profiles: replace, strict and tcl8.",
    },
    Entry {
        name: "system",
        synopsis: "encoding system ?encoding?",
        summary: "The encoding used for system calls, utf-8 unless it is set to another.",
    },
    Entry {
        name: "user",
        synopsis: "encoding user",
        summary: "The encoding the user prefers, which off Windows is the system encoding.",
    },
];

/// The `clock` subcommands, in the order [`subcommands`] lists them.
const CLOCK_CORPUS: &[Entry] = &[
    Entry {
        name: "add",
        synopsis: "clock add clockval ?number units?... ?-option value?",
        summary: "Move an instant by calendar or fixed units; a day past the end of a month is clamped to it.",
    },
    Entry {
        name: "clicks",
        synopsis: "clock clicks ?-switch?",
        summary: "A high-resolution counter, in microseconds unless -milliseconds is given.",
    },
    Entry {
        name: "format",
        synopsis: "clock format clockval ?-format string? ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?",
        summary: "An instant as text, in the root locale's catalogue.",
    },
    Entry {
        name: "microseconds",
        synopsis: "clock microseconds",
        summary: "The current time in microseconds since the epoch.",
    },
    Entry {
        name: "milliseconds",
        synopsis: "clock milliseconds",
        summary: "The current time in milliseconds since the epoch.",
    },
    Entry {
        name: "scan",
        synopsis: "clock scan string ?-format string? ?-gmt boolean? ?-locale LOCALE? ?-timezone ZONE?",
        summary: "Text as an instant. The -format form only; the free-form parser is refused.",
    },
    Entry {
        name: "seconds",
        synopsis: "clock seconds",
        summary: "The current time in seconds since the epoch.",
    },
];

/// The `file` subcommands, in the order [`subcommands`] lists them. The ones
/// this frontend refuses are listed for the same reason the `string` ensemble's
/// are: their presence is what makes an abbreviation ambiguous.
const FILE_CORPUS: &[Entry] = &[
    Entry {
        name: "atime",
        synopsis: "file atime name",
        summary: "The last access time, in seconds since the epoch.",
    },
    Entry {
        name: "attributes",
        synopsis: "file attributes name",
        summary: "Refused: the platform attribute set is not built.",
    },
    Entry {
        name: "channels",
        synopsis: "file channels",
        summary: "Refused: this frontend has no channels.",
    },
    Entry {
        name: "copy",
        synopsis: "file copy ?-force? ?--? source ?source ...? target",
        summary: "Copy files or whole directories; an existing target is an error without -force.",
    },
    Entry {
        name: "delete",
        synopsis: "file delete ?-force? ?--? ?name ...?",
        summary: "Remove names; one that does not exist is not an error.",
    },
    Entry {
        name: "dirname",
        synopsis: "file dirname name",
        summary: "Everything but the last path component.",
    },
    Entry {
        name: "executable",
        synopsis: "file executable name",
        summary: "1 when the name can be executed by this process.",
    },
    Entry {
        name: "exists",
        synopsis: "file exists name",
        summary: "1 when the name resolves to something.",
    },
    Entry {
        name: "extension",
        synopsis: "file extension name",
        summary: "From the last dot at or after the last separator to the end.",
    },
    Entry {
        name: "home",
        synopsis: "file home ?user?",
        summary: "A home directory, this process's own when no user is named.",
    },
    Entry {
        name: "isdirectory",
        synopsis: "file isdirectory name",
        summary: "1 when the name resolves to a directory.",
    },
    Entry {
        name: "isfile",
        synopsis: "file isfile name",
        summary: "1 when the name resolves to an ordinary file.",
    },
    Entry {
        name: "join",
        synopsis: "file join name ?name ...?",
        summary:
            "Join path elements; an element that is itself absolute discards the ones before it.",
    },
    Entry {
        name: "link",
        synopsis: "file link ?-linktype? linkName ?target?",
        summary: "Refused: creating links is not built.",
    },
    Entry {
        name: "lstat",
        synopsis: "file lstat name varName",
        summary: "Refused: it writes an array this frontend does not build for it.",
    },
    Entry {
        name: "mkdir",
        synopsis: "file mkdir ?dir ...?",
        summary:
            "Create directories and every missing parent; an existing directory is not an error.",
    },
    Entry {
        name: "mtime",
        synopsis: "file mtime name",
        summary: "The last modification time, in seconds since the epoch.",
    },
    Entry {
        name: "nativename",
        synopsis: "file nativename name",
        summary: "The path in the platform's own form, which on unix drops duplicate separators.",
    },
    Entry {
        name: "normalize",
        synopsis: "file normalize name",
        summary: "The absolute path with links resolved, except in the last component.",
    },
    Entry {
        name: "owned",
        synopsis: "file owned name",
        summary: "1 when this process's effective user owns the name.",
    },
    Entry {
        name: "pathtype",
        synopsis: "file pathtype name",
        summary: "absolute or relative; unix has no volume-relative paths.",
    },
    Entry {
        name: "readable",
        synopsis: "file readable name",
        summary: "1 when the name can be read by this process.",
    },
    Entry {
        name: "readlink",
        synopsis: "file readlink name",
        summary: "What a symbolic link points at, as it was written.",
    },
    Entry {
        name: "rename",
        synopsis: "file rename ?-force? ?--? source ?source ...? target",
        summary: "Move names; an existing target is an error without -force.",
    },
    Entry {
        name: "rootname",
        synopsis: "file rootname name",
        summary: "The path with its extension cut off.",
    },
    Entry {
        name: "separator",
        synopsis: "file separator ?name?",
        summary: "The path separator, which on unix is always a slash.",
    },
    Entry {
        name: "size",
        synopsis: "file size name",
        summary: "The size in bytes.",
    },
    Entry {
        name: "split",
        synopsis: "file split name",
        summary: "The path's elements as a list; the root, when there is one, is a single element.",
    },
    Entry {
        name: "stat",
        synopsis: "file stat name varName",
        summary: "Refused: it writes an array this frontend does not build for it.",
    },
    Entry {
        name: "system",
        synopsis: "file system name",
        summary: "Refused: there is one filesystem here and no way to name another.",
    },
    Entry {
        name: "tail",
        synopsis: "file tail name",
        summary: "The last path component.",
    },
    Entry {
        name: "tempdir",
        synopsis: "file tempdir ?template?",
        summary: "Refused: it is built on the channel layer.",
    },
    Entry {
        name: "tempfile",
        synopsis: "file tempfile ?nameVar? ?template?",
        summary: "Refused: it returns an open channel.",
    },
    Entry {
        name: "tildeexpand",
        synopsis: "file tildeexpand name",
        summary: "The one command in Tcl 9 that expands a leading ~ or ~user.",
    },
    Entry {
        name: "type",
        synopsis: "file type name",
        summary: "file, directory, link, fifo, socket, blockSpecial or characterSpecial.",
    },
    Entry {
        name: "volumes",
        synopsis: "file volumes",
        summary: "Refused: there is no volume table here.",
    },
    Entry {
        name: "writable",
        synopsis: "file writable name",
        summary: "1 when the name can be written by this process.",
    },
];

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
        summary: "The index just past the last character of the word containing charIndex — a word being a run of letters, digits and underscores, or a single other character. The index is clamped rather than refused: a negative one is read as 0 and one past the end as the length. Characters outside ASCII raise instead of answering, because the word classes rest on Unicode tables this build carries at a different revision than the reference interpreter's.",
    },
    Entry {
        name: "wordstart",
        synopsis: "string wordstart string charIndex",
        summary: "The index of the first character of the word containing charIndex, with the same clamping and the same refusal beyond ASCII as `string wordend`.",
    },
];

const ARRAY_CORPUS: &[Entry] = &[
    Entry {
        name: "anymore",
        synopsis: "array anymore arrayName searchId",
        summary: "Whether a search opened by `array startsearch` has elements left to hand out — 1 or 0. The four search-token subcommands need a cursor held across commands and keyed by the token `startsearch` returned, which is state this frontend does not keep, so all four are refused together.",
    },
    Entry {
        name: "default",
        synopsis: "array default subcommand arrayName ?value?",
        summary: "Tcl 9's per-array default: `array default set a 0` makes every unset element of `a` read as 0, and `get`, `exists` and `unset` inspect and clear that. It is a property of the variable rather than of a value, and nothing in this frontend's array storage carries one.",
    },
    Entry {
        name: "donesearch",
        synopsis: "array donesearch arrayName searchId",
        summary: "Close a search and release its token, returning the empty string. Refused with the rest of the search-token family.",
    },
    Entry {
        name: "exists",
        synopsis: "array exists arrayName",
        summary: "Whether the variable exists and holds an array — 1 or 0. A peek, not a read: asking does not create the variable, which is what lets it be asked about an unset name without raising.",
    },
    Entry {
        name: "for",
        synopsis: "array for {keyVar valueVar} arrayName script",
        summary: "Run the script once per element with the two variables bound, the way `dict for` does, yielding the empty string. `dict for` is built here and this is not: iterating an array needs a snapshot of a *variable's* elements taken before the body can modify them, which is a different lowering from walking a value.",
    },
    Entry {
        name: "get",
        synopsis: "array get arrayName ?pattern?",
        summary: "The array as a flat list of alternating names and values, restricted to the names matching a glob pattern when one is given. The order is the storage's, not sorted.",
    },
    Entry {
        name: "names",
        synopsis: "array names arrayName ?mode? ?pattern?",
        summary: "The element names, filtered by a glob pattern by default. `-exact` and `-glob` are accepted as the mode; `-regexp` is recognised and refused, because the filter runs where the regular-expression engine is not reachable.",
    },
    Entry {
        name: "nextelement",
        synopsis: "array nextelement arrayName searchId",
        summary: "The next element name a search has to give, or the empty string when it is exhausted. Refused with the rest of the search-token family.",
    },
    Entry {
        name: "set",
        synopsis: "array set arrayName list",
        summary: "Set elements from a flat list of alternating names and values; an odd-length list is an error. Which name the error quotes follows the *command's* scope rather than the variable's — inside a procedure body tclsh names the variable, at the top level it names the first element it was about to write, and this reproduces that.",
    },
    Entry {
        name: "size",
        synopsis: "array size arrayName",
        summary: "How many elements the array has; 0 for a variable that is unset or is not an array.",
    },
    Entry {
        name: "startsearch",
        synopsis: "array startsearch arrayName",
        summary: "Open a traversal of the array and return a token naming it, such as `s-1-a`. The token identifies a cursor the interpreter holds until `array donesearch`, which is the state this frontend does not have.",
    },
    Entry {
        name: "statistics",
        synopsis: "array statistics arrayName",
        summary: "A multi-line report on the hash table behind the array — entries, buckets, the bucket-occupancy histogram and the average search distance. Every number in it describes the reference interpreter's own `Tcl_HashTable`, which is not the structure this frontend stores an array in, so there is nothing truthful to answer.",
    },
    Entry {
        name: "unset",
        synopsis: "array unset arrayName ?pattern?",
        summary: "Remove the elements whose names match the glob pattern, or the whole array when no pattern is given.",
    },
];

const DICT_CORPUS: &[Entry] = &[
    Entry {
        name: "append",
        synopsis: "dict append dictVarName key ?string ...?",
        summary: "Append the strings to the value already at the key in the dict the variable holds, creating the key when it is absent, and yield the new dict.",
    },
    Entry {
        name: "create",
        synopsis: "dict create ?key value ...?",
        summary: "A dict of the given pairs; an odd number of arguments is an error. With no arguments it is the empty dict, which is the empty string.",
    },
    Entry {
        name: "exists",
        synopsis: "dict exists dictionary key ?key ...?",
        summary: "Whether the whole key path resolves — 1 or 0. A path that runs into a value which is not itself a dict answers 0 rather than raising, which is what separates it from `dict get`.",
    },
    Entry {
        name: "filter",
        synopsis: "dict filter dictionary filterType arg ?arg ...?",
        summary: "The pairs kept by one of three filters: `key` and `value` take glob patterns and are built here — any of several patterns matching keeps the pair, and no pattern keeps nothing. The `script {k v} body` form is not: it needs a body run per pair against caller variables, which is the same machinery `dict map` wants.",
    },
    Entry {
        name: "for",
        synopsis: "dict for {keyVarName valueVarName} dictionary script",
        summary: "Run the script for each pair with the two variables bound, in insertion order, and yield the empty string. Lowered as a cursor over a flattened pair list, so the dict is snapshotted before the first iteration.",
    },
    Entry {
        name: "get",
        synopsis: "dict get dictionary ?key ...?",
        summary: "The value at the key path; the whole dict when no key is given. A key that is absent raises, unlike `dict exists`.",
    },
    Entry {
        name: "getdef",
        synopsis: "dict getdef dictionary ?key ...? key default",
        summary: "The value at the key path, or the trailing default argument when any step of the path is missing — the total-function form of `dict get`. A dict that does not parse is still an error, as it is for `dict get`.",
    },
    Entry {
        name: "getwithdefault",
        synopsis: "dict getwithdefault dictionary ?key ...? key default",
        summary: "The long spelling of `dict getdef`, identical in behaviour. Both are listed because either one being present is what makes `dict get` an unambiguous abbreviation of nothing else.",
    },
    Entry {
        name: "incr",
        synopsis: "dict incr dictVarName key ?increment?",
        summary: "Add the increment (1 by default) to the value at the key in the dict the variable holds, treating an absent key as 0, and yield the new dict. The variable may be a frame slot or a global; a *variable* naming an array element — `dict incr a(k) …` — is refused instead.",
    },
    Entry {
        name: "info",
        synopsis: "dict info dictionary",
        summary: "A multi-line report on the hash table behind the dict: entries, buckets, the bucket-occupancy histogram and the average search distance. Every number in it describes the reference interpreter's `Tcl_HashTable`, which is not what stores a dict here.",
    },
    Entry {
        name: "keys",
        synopsis: "dict keys dictionary ?pattern?",
        summary: "The keys in insertion order, restricted to those matching a glob pattern when one is given.",
    },
    Entry {
        name: "lappend",
        synopsis: "dict lappend dictVarName key ?value ...?",
        summary: "Append the values as list elements to the value at the key in the dict the variable holds, and yield the new dict.",
    },
    Entry {
        name: "map",
        synopsis: "dict map {keyVarName valueVarName} dictionary script",
        summary: "`dict for` that collects each iteration's result as the new value for that key, yielding a dict of the same keys. Not built here — it wants the per-iteration result plumbing `lmap` has and the pair cursor `dict for` has, together.",
    },
    Entry {
        name: "merge",
        synopsis: "dict merge ?dictionary ...?",
        summary: "One dict of all the arguments, a later occurrence of a key winning. With no arguments it is the empty dict.",
    },
    Entry {
        name: "remove",
        synopsis: "dict remove dictionary ?key ...?",
        summary: "The dict without the named keys; a key that is not present is not an error. Only top-level keys — there is no path form.",
    },
    Entry {
        name: "replace",
        synopsis: "dict replace dictionary ?key value ...?",
        summary: "The dict with the given pairs set, as a value rather than through a variable — `dict set` without the assignment.",
    },
    Entry {
        name: "set",
        synopsis: "dict set dictVarName key ?key ...? value",
        summary: "Set the value at a key path in the dict the variable holds, creating the variable and any intermediate dicts, and yield the new dict. The read of the current value tolerates an unset variable where a bare `$d` would refuse it; a variable naming an array element is refused.",
    },
    Entry {
        name: "size",
        synopsis: "dict size dictionary",
        summary: "How many pairs the dict has.",
    },
    Entry {
        name: "unset",
        synopsis: "dict unset dictVarName key ?key ...?",
        summary: "Remove the key path from the dict the variable holds and yield the new dict. Removing the last key of the path is not an error when it is absent; a key the path has to walk *through* must exist.",
    },
    Entry {
        name: "update",
        synopsis: "dict update dictVarName key varName ?key varName ...? script",
        summary: "Bind each named key's value to a variable, run the script, then write the variables back into the dict — and yield the script's own result. Not built here: it needs writes to caller variables to survive the body and be copied back afterwards.",
    },
    Entry {
        name: "values",
        synopsis: "dict values dictionary ?pattern?",
        summary: "The values in insertion order, restricted to those whose string form matches a glob pattern when one is given.",
    },
    Entry {
        name: "with",
        synopsis: "dict with dictVarName ?key ...? script",
        summary: "`dict update` over *every* key at once: each key becomes a variable of that name for the duration of the script, and the variables are written back after. Not built here, for the same reason.",
    },
];

const INFO_CORPUS: &[Entry] = &[
    Entry {
        name: "args",
        synopsis: "info args procname",
        summary: "The parameter names of a procedure, in order, as a list. Answered from the signature table the compiler already builds to check call arity, so a computed procedure name works as well as a literal one.",
    },
    Entry {
        name: "body",
        synopsis: "info body procname",
        summary: "The source text a procedure's body was written as, answered from the same table `info args` reads, so a computed procedure name works. A procedure whose body the script computed has no text and is reported as no procedure, which is what tclsh reports for a name that is none.",
    },
    Entry {
        name: "class",
        synopsis: "info class subcommand class ?arg ...?",
        summary: "Introspection on a TclOO class — its instances, methods, superclasses and mixins, selected by a second subcommand word. TclOO is not implemented here, so the whole family is refused by name.",
    },
    Entry {
        name: "cmdcount",
        synopsis: "info cmdcount",
        summary: "How many commands the interpreter has evaluated. Refused: nothing counts them. A compiled frontend does not execute commands one at a time — a loop body becomes bytecode and, above the first tier, native code — so there is no place the count could be kept without slowing down what it measures.",
    },
    Entry {
        name: "cmdtype",
        synopsis: "info cmdtype commandName",
        summary: "What kind of command a name is — `native`, `proc`, `alias`, `ensemble`, `import` or `object` in the reference interpreter. Refused: commands are resolved while compiling and no per-name kind survives into the run.",
    },
    Entry {
        name: "commands",
        synopsis: "info commands ?pattern?",
        summary: "The command names matching the glob pattern — every name the frontend answers to, the command modules' included, together with the script-defined procedures — sorted. With no pattern, every name.",
    },
    Entry {
        name: "complete",
        synopsis: "info complete command",
        summary: "1 when the text is a whole command, 0 when more input could finish it. Only genuinely unterminated input answers 0 — an open brace, bracket or quote. Text that is closed but malformed is complete: `puts }extra`, `{a}x`, `set x \"a\"b` and a trailing backslash are all 1, matching tclsh. This is what a REPL asks to decide whether to keep reading.",
    },
    Entry {
        name: "constant",
        synopsis: "info constant varName",
        summary: "Whether a variable was made read-only by Tcl 9's `const`. Refused: constant variables are not implemented, so no variable can answer 1 and answering 0 for all of them would be a claim rather than a report.",
    },
    Entry {
        name: "consts",
        synopsis: "info consts ?pattern?",
        summary: "The constant variable names matching the pattern. Refused for the same reason as `info constant`.",
    },
    Entry {
        name: "coroutine",
        synopsis: "info coroutine",
        summary: "The name of the coroutine whose context is running, or the empty string outside one. The one `info` subcommand answered from the coroutine machinery rather than from the interpreter's tables.",
    },
    Entry {
        name: "default",
        synopsis: "info default procname arg varname",
        summary: "1 when the named parameter has a default, writing the default into varname; 0 otherwise, writing the empty string. The third argument may itself be an array element, which is what a script reading a whole signature into an array writes.",
    },
    Entry {
        name: "errorstack",
        synopsis: "info errorstack ?interp?",
        summary: "The `CALL`/`INNER` trace of the most recent error, as a list. Refused: errors here carry a message and a line, not a captured call chain.",
    },
    Entry {
        name: "exists",
        synopsis: "info exists varName",
        summary: "1 when the variable or array element is set, 0 otherwise. A peek rather than a read — reaching the variable's place without growing storage — because an unset read raises here and asking whether a variable is set must not create it.",
    },
    Entry {
        name: "frame",
        synopsis: "info frame ?number?",
        summary: "The depth of the call stack, or a dictionary describing one frame — its type, the file and line it came from, and the command being run. Refused: this frontend does not expose the running call frame.",
    },
    Entry {
        name: "functions",
        synopsis: "info functions ?pattern?",
        summary: "The `expr` math function names matching the pattern — `abs`, `sin`, `pow` and the rest — read from the same table that lowers a call to one, so the list cannot fall behind what `expr` accepts.",
    },
    Entry {
        name: "globals",
        synopsis: "info globals ?pattern?",
        summary: "The global variable names matching the glob pattern, sorted. Global here means the VM's own global table, which is where a script's top-level variables live.",
    },
    Entry {
        name: "hostname",
        synopsis: "info hostname",
        summary: "This host's name, from `gethostname`; the empty string when the call fails. Read at run time rather than compiled in, so an AOT-built binary reports the machine it runs on and not the one that built it.",
    },
    Entry {
        name: "level",
        synopsis: "info level ?number?",
        summary: "How deep the current procedure call is — 0 at a script's own level, one more per activation. Only a call is counted, not the frames the VM pushes for a scope or after a JIT side exit. The form taking a level number is refused: a call site pushes the actual arguments and nothing naming the command, so there is no record of what entered a level.",
    },
    Entry {
        name: "library",
        synopsis: "info library",
        summary: "The directory of Tcl's script library. Compiles and runs, then raises `no library has been specified for Tcl` — tclsh's own message for an interpreter whose `tcl_library` is gone, which is the state this frontend is permanently in because nothing here reads an `init.tcl`. A raise, not a refusal, so a `catch` around it behaves as it does under tclsh.",
    },
    Entry {
        name: "loaded",
        synopsis: "info loaded ?interp? ?prefix?",
        summary: "The binary extension packages loaded into an interpreter. Refused: `load` is not implemented, so the list would be empty by construction rather than by observation.",
    },
    Entry {
        name: "locals",
        synopsis: "info locals ?pattern?",
        summary: "The local variable names of the running procedure that are set, matching the pattern. A local is a frame slot addressed by index, so which names the frame has is settled while compiling and which of them hold anything is settled by the frame — the answer is the two halves met. A local whose only mention stands after the `info locals` is not listed.",
    },
    Entry {
        name: "nameofexecutable",
        synopsis: "info nameofexecutable",
        summary: "The full path of the running binary, from the operating system. For an AOT-compiled script this is the standalone executable, not the tclrs that built it.",
    },
    Entry {
        name: "object",
        synopsis: "info object subcommand object ?arg ...?",
        summary: "Introspection on a TclOO object — its class, methods, variables and mixins, selected by a second subcommand word. Refused with `info class`, because TclOO is not implemented.",
    },
    Entry {
        name: "patchlevel",
        synopsis: "info patchlevel",
        summary: "The full `major.minor.patch` version of the *Tcl language* this frontend implements — 9.0.4 — not the crate's own version, which `tclrs --version` reports. A script branching on it is asking about the language.",
    },
    Entry {
        name: "procs",
        synopsis: "info procs ?pattern?",
        summary: "The script-defined procedure names matching the glob pattern, sorted; builtins are excluded, which is the difference from `info commands`.",
    },
    Entry {
        name: "script",
        synopsis: "info script ?filename?",
        summary: "The file being evaluated, or the empty string when the script came from `-c` or stdin. With an argument it sets the name and returns that same new name — tclsh answers identically, so the one-line save-and-restore idiom does not work in either.",
    },
    Entry {
        name: "sharedlibextension",
        synopsis: "info sharedlibextension",
        summary: "The suffix a loadable library has on this platform — `.dylib`, `.dll` or `.so`. Decided by the build target, so it is the platform's answer and not a probe of the filesystem.",
    },
    Entry {
        name: "tclversion",
        synopsis: "info tclversion",
        summary: "The `major.minor` version of the Tcl language implemented — 9.0. As with `info patchlevel`, the language's version and not the crate's.",
    },
    Entry {
        name: "vars",
        synopsis: "info vars ?pattern?",
        summary: "The visible variable names matching the glob pattern, sorted. At a script's own level that is every variable the interpreter holds, including the `argc`, `argv` and `argv0` it sets up; inside a procedure it is the frame's own — its set locals plus the names `global`, `variable` and `upvar` bound into it, which is what tclsh answers.",
    },
];

/// `namespace`'s subcommands, in the order `cmd_namespace::SUBCOMMANDS` lists
/// them — which is the order tclsh's own `unknown or ambiguous subcommand`
/// message lists them in, so the two agree.
const NAMESPACE_CORPUS: &[Entry] = &[
    Entry {
        name: "children",
        synopsis: "namespace children ?name? ?pattern?",
        summary: "The child namespaces of a namespace, fully qualified.",
    },
    Entry {
        name: "code",
        synopsis: "namespace code script",
        summary: "The script wrapped so that evaluating it later runs it in this namespace.",
    },
    Entry {
        name: "current",
        synopsis: "namespace current",
        summary: "The namespace the command was written in. Folded while compiling, since that is where a namespace is decided.",
    },
    Entry {
        name: "delete",
        synopsis: "namespace delete ?name name...?",
        summary: "Remove namespaces, their child namespaces and their commands.",
    },
    Entry {
        name: "ensemble",
        synopsis: "namespace ensemble subcommand ?arg ...?",
        summary: "`exists`, `create` and `configure`. Dispatching *through* an ensemble is not implemented: a call resolves its command while compiling.",
    },
    Entry {
        name: "eval",
        synopsis: "namespace eval name arg ?arg...?",
        summary: "Lower the body with this namespace current, which is what gives its `proc`, `variable` and `$v` the namespace's names. The name and the body have to be written out.",
    },
    Entry {
        name: "exists",
        synopsis: "namespace exists name",
        summary: "1 when the namespace exists.",
    },
    Entry {
        name: "export",
        synopsis: "namespace export ?-clear? ?pattern pattern...?",
        summary: "Add patterns to the namespace's export list, or report it when no pattern is given.",
    },
    Entry {
        name: "forget",
        synopsis: "namespace forget ?pattern pattern...?",
        summary: "Remove the imports of the commands a pattern names — not the commands themselves.",
    },
    Entry {
        name: "import",
        synopsis: "namespace import ?-force? ?pattern pattern...?",
        summary: "Bring exported commands into this namespace under their tail names. A pattern that matches nothing is not an error.",
    },
    Entry {
        name: "inscope",
        synopsis: "namespace inscope ns script ?arg...?",
        summary: "Evaluate the script in a namespace, with the extra arguments appended as list elements.",
    },
    Entry {
        name: "origin",
        synopsis: "namespace origin name",
        summary: "Where an imported command was originally defined; the command's own name when it was not imported.",
    },
    Entry {
        name: "parent",
        synopsis: "namespace parent ?name?",
        summary: "The namespace containing this one; empty for the root.",
    },
    Entry {
        name: "path",
        synopsis: "namespace path ?namespaceList?",
        summary: "Refused: it changes how a later name resolves, which this frontend resolved while compiling.",
    },
    Entry {
        name: "qualifiers",
        synopsis: "namespace qualifiers string",
        summary: "Everything before the last `::`, a port of `NamespaceQualifiersCmd`. Folded when the argument is written out.",
    },
    Entry {
        name: "tail",
        synopsis: "namespace tail string",
        summary: "Everything after the last `::`, a port of `NamespaceTailCmd`.",
    },
    Entry {
        name: "unknown",
        synopsis: "namespace unknown ?script?",
        summary: "Refused, for the same reason `namespace path` is.",
    },
    Entry {
        name: "upvar",
        synopsis: "namespace upvar ns ?otherVar myVar ...?",
        summary: "Refused, for the same reason `namespace path` is.",
    },
    Entry {
        name: "which",
        synopsis: "namespace which ?-command? ?-variable? name",
        summary: "The qualified name a command or variable resolves to, or the empty string.",
    },
];
/// `package`'s subcommands, in `pkgOptions`' order
/// (`generic/tclPkg.c:1067-1071`) — which is [`crate::cmd_package`]'s order,
/// and the order the `bad option` message lists them in.
const PACKAGE_CORPUS: &[Entry] = &[
    Entry {
        name: "files",
        synopsis: "package files package",
        summary: "The files a package was loaded from. Always empty here: nothing records one, because this frontend has no package index.",
    },
    Entry {
        name: "forget",
        synopsis: "package forget ?package ...?",
        summary: "Drop everything known about each package — its version and every script that would load it. A name that is not known is not an error.",
    },
    Entry {
        name: "ifneeded",
        synopsis: "package ifneeded package version ?script?",
        summary: "Register the script that loads a version, or, with no script, report the one registered for exactly that version.",
    },
    Entry {
        name: "names",
        synopsis: "package names",
        summary: "Every package that is provided or has a loading script, in the order each was first mentioned.",
    },
    Entry {
        name: "prefer",
        synopsis: "package prefer ?latest|stable?",
        summary: "Whether an unqualified require takes the newest version or the newest stable one. Starts at stable and only ever moves to latest.",
    },
    Entry {
        name: "present",
        synopsis: "package present ?-exact? package ?requirement ...?",
        summary: "Like require for a package already provided, and an error rather than a load attempt for one that is not.",
    },
    Entry {
        name: "provide",
        synopsis: "package provide package ?version?",
        summary: "Declare this package present at a version, or report the version it was declared at. A second, different version is a conflict.",
    },
    Entry {
        name: "require",
        synopsis: "package require ?-exact? package ?requirement ...?",
        summary: "Make a package present, loading it if it is not, and yield the version that ended up provided.",
    },
    Entry {
        name: "unknown",
        synopsis: "package unknown ?command?",
        summary: "The script run when a required package is not known; the empty string clears it.",
    },
    Entry {
        name: "vcompare",
        synopsis: "package vcompare version1 version2",
        summary: "-1, 0 or 1 by TIP 268's ordering, in which 9.0 and 9.0.0 are equal and 1.2a3 sorts below 1.2.",
    },
    Entry {
        name: "versions",
        synopsis: "package versions package",
        summary: "The versions a loading script has been registered for.",
    },
    Entry {
        name: "vsatisfies",
        synopsis: "package vsatisfies version ?requirement ...?",
        summary: "Whether the version meets any of the requirements, each a version, a versionMin-versionMax range, or a versionMin- open range.",
    },
];

/// The `expr` binary operators, one entry per operator in
/// [`crate::expr::LEVELS`] and in that table's order.
///
/// Pinned to the parser's own ladder by a test, so an operator added to a level
/// without a description here fails the build rather than going undocumented.
/// Where Tcl's arithmetic differs from C's — and it differs in most of the
/// integer operators — the entry says how.
pub const BINARY_OPERATORS: &[Entry] = &[
    Entry {
        name: "||",
        synopsis: "a || b",
        summary: "Logical or over Tcl booleans, yielding 1 or 0. Short-circuits: a true left operand means the right is never evaluated, so `1 || [error x]` is 1.",
    },
    Entry {
        name: "&&",
        synopsis: "a && b",
        summary: "Logical and over Tcl booleans, yielding 1 or 0. Short-circuits the same way — `0 && [error x]` is 0.",
    },
    Entry {
        name: "|",
        synopsis: "a | b",
        summary: "Bitwise or. Integers are not a fixed width here: when either side has been promoted past 64 bits the operation is performed over an infinite two's-complement sign extension, so the sign of an arbitrarily large operand is respected rather than truncated.",
    },
    Entry {
        name: "^",
        synopsis: "a ^ b",
        summary: "Bitwise exclusive or, over the same infinite sign extension as `|`.",
    },
    Entry {
        name: "&",
        synopsis: "a & b",
        summary: "Bitwise and, over the same infinite sign extension as `|`. `-1 & 3` is 3, because -1 is all ones however far it is extended.",
    },
    Entry {
        name: "==",
        synopsis: "a == b",
        summary: "Equality that prefers a numeric reading and falls back to string comparison when either operand is not a number. `10 == \"10.0\"` is 1 — both are numbers and both are ten — while `\"abc\" == \"abc\"` is 1 by string.",
    },
    Entry {
        name: "!=",
        synopsis: "a != b",
        summary: "The negation of `==`, with the same numeric-preferring rule. A NaN operand makes both false, so `nan != nan` is 1.",
    },
    Entry {
        name: "eq",
        synopsis: "a eq b",
        summary: "String equality on the operands **as written**. No numeric reading is attempted, so `1.0 eq 1` is 0 where `1.0 == 1` is 1. A numeric literal carries its own spelling for exactly this comparison.",
    },
    Entry {
        name: "ne",
        synopsis: "a ne b",
        summary: "String inequality — the negation of `eq`, and equally blind to numeric value.",
    },
    Entry {
        name: "in",
        synopsis: "a in b",
        summary: "Whether the left operand equals one of the right operand's list elements, compared as strings. String equality is the whole rule, so `1 in {01}` is 0.",
    },
    Entry {
        name: "ni",
        synopsis: "a ni b",
        summary: "Whether the left operand is *not* one of the right's list elements — the negation of `in`, with the same string rule.",
    },
    Entry {
        name: "<=",
        synopsis: "a <= b",
        summary: "Less-or-equal, preferring a numeric reading and falling back to string order, like `==`.",
    },
    Entry {
        name: ">=",
        synopsis: "a >= b",
        summary: "Greater-or-equal, with the same numeric-preferring rule.",
    },
    Entry {
        name: "<",
        synopsis: "a < b",
        summary: "Less-than, with the same numeric-preferring rule.",
    },
    Entry {
        name: ">",
        synopsis: "a > b",
        summary: "Greater-than, with the same numeric-preferring rule. `\"abc\" > \"abd\"` is 0 by string order; `inf > 1` is 1, because `inf` is a floating-point literal here and not a word.",
    },
    Entry {
        name: "lt",
        synopsis: "a lt b",
        summary: "String less-than, never numeric: `\"10\" lt \"9\"` is 1 because `1` sorts before `9`.",
    },
    Entry {
        name: "gt",
        synopsis: "a gt b",
        summary: "String greater-than, never numeric.",
    },
    Entry {
        name: "le",
        synopsis: "a le b",
        summary: "String less-or-equal, never numeric.",
    },
    Entry {
        name: "ge",
        synopsis: "a ge b",
        summary: "String greater-or-equal, never numeric.",
    },
    Entry {
        name: "<<",
        synopsis: "a << b",
        summary: "Left shift that **grows** rather than dropping the bits that leave a word: `1 << 64` is 18446744073709551616, not 0. A negative shift count is an error.",
    },
    Entry {
        name: ">>",
        synopsis: "a >> b",
        summary: "Arithmetic right shift, and saturating rather than wrapping: `1 >> 200` is 0 and `-1 >> 200` is -1, because the sign bit is what is left after every value bit has been shifted out.",
    },
    Entry {
        name: "+",
        synopsis: "a + b",
        summary: "Addition. Integer operands promote past 64 bits rather than wrapping, up to `MAX_INT_BITS`; a floating-point operand makes the result floating-point.",
    },
    Entry {
        name: "-",
        synopsis: "a - b",
        summary: "Subtraction, with the same promotion rule as `+`.",
    },
    Entry {
        name: "*",
        synopsis: "a * b",
        summary: "Multiplication, with the same promotion rule as `+`.",
    },
    Entry {
        name: "/",
        synopsis: "a / b",
        summary: "Integer division **flooring toward negative infinity**, not truncating toward zero as C does: `-57 / 10` is -6 and `7 / -2` is -4. Division by zero is an error.",
    },
    Entry {
        name: "%",
        synopsis: "a % b",
        summary: "The remainder that pairs with Tcl's flooring division, so it takes the sign of the *divisor*: `-57 % 10` is 3, `5 % -3` is -1 and `-5 % 3` is 1.",
    },
    Entry {
        name: "**",
        synopsis: "a ** b",
        summary: "Exponentiation — the one right-associative level, so `2 ** 3 ** 2` is 512. Integral for integral operands including a negative exponent, which floors: `2 ** -1` is 0. `0 ** -1` is `exponentiation of zero by negative power`.",
    },
];

/// The `expr` operators that are not one of [`BINARY_OPERATORS`]' levels: the
/// four unary prefixes, which bind tighter than every binary level, and the
/// ternary, which binds looser.
pub const OTHER_OPERATORS: &[Entry] = &[
    Entry {
        name: "-",
        synopsis: "- a",
        summary: "Arithmetic negation. Unary operators bind tighter than every binary level and are parsed right to left, so `- - 1` is 1.",
    },
    Entry {
        name: "+",
        synopsis: "+ a",
        summary: "Unary plus. It asserts that the operand is a number — a non-numeric operand is an error — and otherwise changes nothing.",
    },
    Entry {
        name: "~",
        synopsis: "~ a",
        summary: "Bitwise complement over the infinite two's-complement sign extension, so `~5` is -6. A floating-point operand is an error.",
    },
    Entry {
        name: "!",
        synopsis: "! a",
        summary: "Logical not over a Tcl boolean, yielding 1 or 0 — `!yes` is 0. Matched only when the next character is not `=`, so `!=` is still the inequality operator.",
    },
    Entry {
        name: "?:",
        synopsis: "test ? a : b",
        summary: "The conditional. It binds looser than every binary operator, is right-associative, and evaluates only the arm it takes — the other arm's command substitutions never run.",
    },
];

/// What may stand where `expr` wants an operand, as
/// `ExprParser::parse_operand` accepts it.
///
/// The name of each entry is the shape rather than a spelling, because these
/// are grammar productions and not names a script writes literally.
pub const OPERANDS: &[Entry] = &[
    Entry {
        name: "integer literal",
        synopsis: "123   0xff   0o17   0b101   0d19   1_000_000",
        summary: "An integer, in decimal or with a `0x`, `0o`, `0b` or `0d` radix prefix. `_` may separate digits but must have one on each side, so `1_000_000` is a million while `0x_10` and `1_` are not numbers at all and are read as barewords.",
    },
    Entry {
        name: "floating-point literal",
        synopsis: "1.5   1.5e3   .5",
        summary: "A double, in the usual C spellings. A literal carries the text the script wrote as well as its value, so `expr {007.0}` prints `007.0` rather than `7.0`.",
    },
    Entry {
        name: "inf / nan",
        synopsis: "inf   infinity   nan",
        summary: "Floating-point literals in any case, not function names — `expr {inf > 1}` is 1 and `expr {nan == nan}` is 0. Exactly these three words and no others, and each keeps its spelling, so `inf eq \"Inf\"` is 0.",
    },
    Entry {
        name: "boolean word",
        synopsis: "true   false   yes   no   on   off",
        summary: "A unique, case-insensitive prefix of one of the six words. `t`, `fals`, `y`, `n` and `of` are booleans; `o` is not, because `on` and `off` both begin with it. The word is an operand carrying its own spelling — `expr {ON}` is `ON` — so `eq` compares text and arithmetic refuses it with tclsh's non-numeric wording.",
    },
    Entry {
        name: "variable",
        synopsis: "$name   $arr(index)",
        summary: "The variable's value, or an array element's. The index is itself substituted, so `$arr($i)` works.",
    },
    Entry {
        name: "command substitution",
        synopsis: "[script]",
        summary: "The result of running the script, as an operand. Inside a short-circuited `&&`, `||` or `?:` arm it is never run.",
    },
    Entry {
        name: "quoted string",
        synopsis: "\"text\"",
        summary: "A double-quoted operand with the usual substitutions performed inside it — variables, command substitutions and backslash escapes.",
    },
    Entry {
        name: "braced string",
        synopsis: "{text}",
        summary: "A braced operand taken literally: nothing inside it is substituted. `\"a\" eq {a}` is 1 because both produce the one character.",
    },
    Entry {
        name: "grouping",
        synopsis: "( expression )",
        summary: "Parentheses override precedence. Nesting is bounded by `MAX_EXPR_DEPTH`, past which the expression is refused rather than overflowing the stack.",
    },
    Entry {
        name: "function call",
        synopsis: "name(arg, ...)",
        summary: "Parsed as a call and then refused: `math function \"sin\" is not supported yet`. The refusal is deferred, so an unevaluated call — in a short-circuited arm, or an untaken branch — costs nothing and a `catch` can trap it.",
    },
];

/// The `string is` classes, one entry per name in
/// [`crate::cmd_string::CLASSES`] and in that table's order.
///
/// Pinned to that table by a test. Every class but `list` and `dict` answers
/// true for the empty string unless `-strict` is given.
pub const CLASS_CORPUS: &[Entry] = &[
    Entry {
        name: "alnum",
        synopsis: "string is alnum ?-strict? ?-failindex var? str",
        summary: "Every character is a letter or a decimal digit — the Unicode letter categories plus `Nd`.",
    },
    Entry {
        name: "alpha",
        synopsis: "string is alpha ?-strict? ?-failindex var? str",
        summary: "Every character is a letter: `Lu`, `Ll`, `Lt`, `Lm` or `Lo`. This is the *general category* union Tcl uses, not Rust's `char::is_alphabetic`, which is the wider Alphabetic property.",
    },
    Entry {
        name: "ascii",
        synopsis: "string is ascii ?-strict? ?-failindex var? str",
        summary: "Every character is below U+0080. Answered arithmetically, so it never needs the Unicode tables and never refuses.",
    },
    Entry {
        name: "control",
        synopsis: "string is control ?-strict? ?-failindex var? str",
        summary: "Every character is in category `Cc` or `Cf` — the C0/C1 controls and the format characters.",
    },
    Entry {
        name: "boolean",
        synopsis: "string is boolean ?-strict? ?-failindex var? str",
        summary: "The whole string is a Tcl boolean: a number in any radix, or a unique case-insensitive prefix of `true`/`false`/`yes`/`no`/`on`/`off`.",
    },
    Entry {
        name: "dict",
        synopsis: "string is dict ?-strict? ?-failindex var? str",
        summary: "Structural rather than a character class: the string is a well-formed list with an even number of elements. `{a 1 b}` is 0 and `{}` is 1. `-strict` is ignored, as it is for `list`.",
    },
    Entry {
        name: "digit",
        synopsis: "string is digit ?-strict? ?-failindex var? str",
        summary: "Every character is in category `Nd`. This is any Unicode decimal digit, not only `0`–`9`.",
    },
    Entry {
        name: "double",
        synopsis: "string is double ?-strict? ?-failindex var? str",
        summary: "The string, less surrounding ASCII whitespace, reads as a floating-point number — integers included, since every integer is a valid double.",
    },
    Entry {
        name: "entier",
        synopsis: "string is entier ?-strict? ?-failindex var? str",
        summary: "The string reads as an integer of any size, with no upper bound: `99999999999999999999` is 1. Identical to `integer` here, which is also unbounded.",
    },
    Entry {
        name: "false",
        synopsis: "string is false ?-strict? ?-failindex var? str",
        summary: "The string is a Tcl boolean *and* that boolean is false — `0`, `no`, `off`, `false` and their prefixes.",
    },
    Entry {
        name: "graph",
        synopsis: "string is graph ?-strict? ?-failindex var? str",
        summary: "Every character is printable and not a space: word characters, punctuation, marks, the non-decimal numbers and the symbol categories. `string is graph \"€\"` is 1 where `string is punct \"€\"` is 0, because the currency symbols are here and not there.",
    },
    Entry {
        name: "integer",
        synopsis: "string is integer ?-strict? ?-failindex var? str",
        summary: "The string, less surrounding ASCII whitespace, reads as an integer in any radix — `0xff` is 1. Unbounded, so it does not narrow to 32 bits the way the reference interpreter's documentation describes.",
    },
    Entry {
        name: "list",
        synopsis: "string is list ?-strict? ?-failindex var? str",
        summary: "The string parses as a well-formed Tcl list — the test is whether the list splitter accepts it, so unbalanced braces are 0. `-strict` is ignored: the empty string is a well-formed list.",
    },
    Entry {
        name: "lower",
        synopsis: "string is lower ?-strict? ?-failindex var? str",
        summary: "Every character is in category `Ll`. Title-case letters are not lower case.",
    },
    Entry {
        name: "print",
        synopsis: "string is print ?-strict? ?-failindex var? str",
        summary: "`graph` plus the three Unicode separator categories — and *not* plus the ASCII control whitespace, so `string is print \\t` is 0 while `string is space \\t` is 1.",
    },
    Entry {
        name: "punct",
        synopsis: "string is punct ?-strict? ?-failindex var? str",
        summary: "Every character is in one of the seven punctuation categories. The symbol categories are deliberately excluded; they belong to `graph`.",
    },
    Entry {
        name: "space",
        synopsis: "string is space ?-strict? ?-failindex var? str",
        summary: "Every character is Tcl whitespace: the six ASCII space characters below U+0080, then the Unicode whitespace plus U+180E, U+200B, U+2060 and U+FEFF. Verified equal to the reference interpreter over every code point up to U+2FFFF, and answered without the general-category tables so it never refuses.",
    },
    Entry {
        name: "true",
        synopsis: "string is true ?-strict? ?-failindex var? str",
        summary: "The string is a Tcl boolean *and* that boolean is true — `1`, `yes`, `on`, `true` and their prefixes.",
    },
    Entry {
        name: "upper",
        synopsis: "string is upper ?-strict? ?-failindex var? str",
        summary: "Every character is in category `Lu`. Title-case letters are not upper case.",
    },
    Entry {
        name: "wideinteger",
        synopsis: "string is wideinteger ?-strict? ?-failindex var? str",
        summary: "The string reads as an integer *and* fits in 64 bits. This is the one integer class with a bound, so `99999999999999999999` is 0 here and 1 for `integer` and `entier`.",
    },
    Entry {
        name: "wordchar",
        synopsis: "string is wordchar ?-strict? ?-failindex var? str",
        summary: "Every character is a letter, a decimal digit, or connector punctuation — which is what makes `_` a word character.",
    },
    Entry {
        name: "xdigit",
        synopsis: "string is xdigit ?-strict? ?-failindex var? str",
        summary: "Every character is an ASCII hexadecimal digit, either case. Answered without the general-category tables, so it never refuses.",
    },
];

/// The `format` conversions, in the order `format_string` matches them.
///
/// Pinned by a test that runs one of each: a documented conversion that the
/// runtime calls a bad field specifier, or an accepted letter with no entry
/// here, fails the build.
pub const CONVERSION_CORPUS: &[Entry] = &[
    Entry {
        name: "%s",
        synopsis: "format %s value",
        summary: "The argument as a string. A precision truncates it to that many *characters*, not bytes — `format %.3s abcdef` is `abc`.",
    },
    Entry {
        name: "%c",
        synopsis: "format %c codepoint",
        summary: "The character with that code point — `format %c 65` is `A`. A number that is not a code point yields U+FFFD rather than an error.",
    },
    Entry {
        name: "%d",
        synopsis: "format %d integer",
        summary: "A signed decimal integer, truncated to the size modifier's width (32 bits by default). `#` prefixes `0d`, which is Tcl's spelling and not C's.",
    },
    Entry {
        name: "%i",
        synopsis: "format %i integer",
        summary: "Identical to `%d` in every respect; both are signed decimal.",
    },
    Entry {
        name: "%u",
        synopsis: "format %u integer",
        summary: "An unsigned decimal integer. The value is reinterpreted at the size modifier's width, so `format %u -1` is 4294967295 at the default 32 bits. An untruncated width (`ll` or `L`) is `unsigned bignum format is invalid`, because there is no width to reinterpret at.",
    },
    Entry {
        name: "%o",
        synopsis: "format %o integer",
        summary: "Unsigned octal. `#` prefixes `0o` — Tcl 9's spelling, where C writes a bare leading `0`.",
    },
    Entry {
        name: "%x",
        synopsis: "format %x integer",
        summary: "Unsigned hexadecimal in lower case. `#` prefixes `0x`.",
    },
    Entry {
        name: "%X",
        synopsis: "format %X integer",
        summary: "Unsigned hexadecimal in upper case. The `#` prefix stays lower-case `0x`, matching the reference interpreter rather than C's `0X`.",
    },
    Entry {
        name: "%b",
        synopsis: "format %b integer",
        summary: "Unsigned binary. `#` prefixes `0b`. Tcl has this conversion and C does not.",
    },
    Entry {
        name: "%p",
        synopsis: "format %p integer",
        summary: "Hexadecimal over the whole 64-bit word, always prefixed: `format %p -1` is `0xffffffffffffffff` where `%#x -1` is `0xffffffff`, and `format %p 0` is `0x0` where `%#x 0` is `0`. Those two differences are the whole of it.",
    },
    Entry {
        name: "%e",
        synopsis: "format %e double",
        summary: "Scientific notation with a lower-case `e` and a two-digit exponent; six digits of precision by default.",
    },
    Entry {
        name: "%E",
        synopsis: "format %E double",
        summary: "`%e` with an upper-case `E` in the exponent.",
    },
    Entry {
        name: "%f",
        synopsis: "format %f double",
        summary: "Fixed-point notation, six digits after the point by default. `%.f` with no digits is precision 0.",
    },
    Entry {
        name: "%g",
        synopsis: "format %g double",
        summary: "The shorter of `%e` and `%f` for the value, with trailing zeroes removed.",
    },
    Entry {
        name: "%G",
        synopsis: "format %G double",
        summary: "`%g` with an upper-case `E` when it chooses the exponential form.",
    },
    Entry {
        name: "%a",
        synopsis: "format %a double",
        summary: "C99's hexadecimal floating-point form. Recognised and refused — `the \"%a\" conversion is not supported yet` — rather than being reported as a bad field specifier, so the message distinguishes a conversion that exists from one that does not.",
    },
    Entry {
        name: "%A",
        synopsis: "format %A double",
        summary: "The upper-case spelling of `%a`, refused with the same message.",
    },
];

/// The `format` size modifiers, consumed between the field and the conversion
/// character, in the order `format_string` tests them.
///
/// A modifier is not a conversion: `format %h 1` is `format string ended in
/// middle of field specifier`, because the `h` was taken and the string then
/// ran out. Only the integer conversions read the width.
pub const MODIFIER_CORPUS: &[Entry] = &[
    Entry {
        name: "ll",
        synopsis: "format %lld integer",
        summary: "No truncation and no reinterpretation: even the unsigned conversions keep the value's sign, so `format %llx -1` is `-1` where `%lx -1` is `ffffffffffffffff`. Tested before the single-letter modifiers, so `ll` never reads as two `l`s.",
    },
    Entry {
        name: "h",
        synopsis: "format %hd integer",
        summary: "Truncate to 16 bits before converting.",
    },
    Entry {
        name: "l",
        synopsis: "format %ld integer",
        summary: "Truncate to 64 bits before converting, and reinterpret rather than keep the sign for an unsigned conversion: `format %lx -1` is `ffffffffffffffff`.",
    },
    Entry {
        name: "j",
        synopsis: "format %jd integer",
        summary: "C's `intmax_t` width — 64 bits here, the same as `l`.",
    },
    Entry {
        name: "q",
        synopsis: "format %qd integer",
        summary: "The BSD quad width — 64 bits, the same as `l`.",
    },
    Entry {
        name: "z",
        synopsis: "format %zd integer",
        summary: "C's `size_t` width. 64 bits on every target this crate builds for.",
    },
    Entry {
        name: "t",
        synopsis: "format %td integer",
        summary: "C's `ptrdiff_t` width. 64 bits, like `z`.",
    },
    Entry {
        name: "L",
        synopsis: "format %Ld integer",
        summary: "No truncation, identical to `ll`. With `%u` it is an error — `unsigned bignum format is invalid` — because an unsigned conversion has no width to reinterpret at.",
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

    /// Every ensemble's documented entries have to be exactly the names it
    /// resolves, in the same order — the reference page pairs the two by
    /// position, so a drift would print one subcommand's description under
    /// another's heading.
    #[test]
    fn subcommand_corpora_match_the_ensemble_tables() {
        for ensemble in ["string", "array", "dict", "info"] {
            let documented: Vec<&str> =
                subcommand_corpus(ensemble).iter().map(|e| e.name).collect();
            assert_eq!(
                documented,
                subcommands(ensemble),
                "{ensemble}'s corpus and its subcommand table disagree"
            );
            assert!(
                subcommand_corpus(ensemble)
                    .iter()
                    .all(|e| e.synopsis.starts_with(&format!("{ensemble} {}", e.name))),
                "a {ensemble} synopsis has to start with the subcommand it is for"
            );
        }
    }

    /// The operator corpus is pinned to the ladder the parser binds with, so an
    /// operator added to a level cannot reach a release undocumented.
    #[test]
    fn binary_operator_corpus_matches_the_parser_ladder() {
        let ladder: Vec<&str> = crate::expr::LEVELS
            .iter()
            .flat_map(|level| level.iter().map(|(text, _)| *text))
            .collect();
        let documented: Vec<&str> = BINARY_OPERATORS.iter().map(|e| e.name).collect();
        assert_eq!(documented, ladder, "BINARY_OPERATORS and LEVELS disagree");
        assert!(
            BINARY_OPERATORS.iter().all(|e| e.synopsis.contains(e.name)),
            "an operator synopsis has to show the operator it is for"
        );
    }

    /// Same pinning for `string is`: the classes documented are the classes the
    /// subcommand resolves against.
    #[test]
    fn class_corpus_matches_the_resolver_table() {
        let documented: Vec<&str> = CLASS_CORPUS.iter().map(|e| e.name).collect();
        assert_eq!(documented, crate::cmd_string::CLASSES);
        assert!(
            CLASS_CORPUS
                .iter()
                .all(|e| e.synopsis.starts_with(&format!("string is {}", e.name))),
            "a class synopsis has to start with the class it is for"
        );
    }

    /// `format` has no table to pin against — the conversions are match arms —
    /// so the corpus is checked by *running* one of each. A documented
    /// conversion the runtime calls a bad field specifier is a lie on the
    /// reference page, and a letter the runtime accepts with no entry here is a
    /// gap in it.
    ///
    /// Three answers are possible for `format %X 1` and each means something
    /// different: `bad field specifier` is not a conversion at all, `format
    /// string ended in middle of field specifier` means the letter was eaten as
    /// a size modifier, and anything else means the conversion exists.
    #[test]
    fn conversion_corpus_matches_what_format_accepts() {
        let conversions: Vec<char> = CONVERSION_CORPUS
            .iter()
            .map(|e| e.name.chars().nth(1).expect("a %x name"))
            .collect();
        let modifiers: Vec<char> = MODIFIER_CORPUS
            .iter()
            .map(|e| e.name.chars().next().expect("a non-empty name"))
            .collect();
        for letter in ('a'..='z').chain('A'..='Z') {
            let answer = crate::eval(&format!("format %{letter} 1"));
            let (is_conversion, is_modifier) = match &answer {
                Ok(_) => (true, false),
                Err(e) if e.contains("bad field specifier") => (false, false),
                Err(e) if e.contains("ended in middle of field specifier") => (false, true),
                Err(_) => (true, false),
            };
            assert_eq!(
                conversions.contains(&letter),
                is_conversion,
                "%{letter}: documented conversions and what format accepts disagree"
            );
            assert_eq!(
                modifiers.contains(&letter),
                is_modifier,
                "%{letter}: documented size modifiers and what format eats disagree"
            );
        }
    }

    #[test]
    fn ensembles_offer_their_subcommands_and_others_offer_none() {
        assert!(subcommands("string").contains(&"toupper"));
        assert!(subcommands("array").contains(&"names"));
        assert!(subcommands("dict").contains(&"keys"));
        assert!(subcommands("info").contains(&"coroutine"));
        assert!(subcommands("namespace").contains(&"eval"));
        assert!(subcommands("puts").is_empty());
    }
}
