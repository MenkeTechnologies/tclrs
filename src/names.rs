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
        .chain(crate::cmd_channel::COMMANDS.iter().copied())
        .chain(crate::regexp::COMMANDS.iter().copied())
        .chain(cmd_clock::COMMANDS.iter().copied())
        .chain(cmd_file::COMMANDS.iter().copied())
        .chain(crate::cmd_encoding::COMMANDS.iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// Whether the compiler lowers a command of this name.
///
/// [`commands`] answers the same question by building the whole sorted
/// vocabulary, which is what a completion menu wants and what a dispatch decision
/// must not do: [`crate::procs::expand_call_op`] asks this per call, for a name
/// only the running script knows. The sources are the same constants, so a
/// command added to one of them is answered for here without being listed twice.
pub fn is_command(name: &str) -> bool {
    Compiler::BUILTINS.contains(&name)
        || cmd_list::COMMANDS.contains(&name)
        || crate::cmd_channel::COMMANDS.contains(&name)
        || crate::regexp::COMMANDS.contains(&name)
        || cmd_clock::COMMANDS.contains(&name)
        || cmd_file::COMMANDS.contains(&name)
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
        synopsis: "info subcommand ?argument ...?",
        summary: "Interpreter introspection. The subcommands naming machinery this frontend has none of — `frame`, `errorstack`, `cmdcount`, the object-system queries, `library`, `loaded` — are refused by name rather than mis-answered.",
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
        "info" => INFO_SUBCOMMANDS,
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
        assert!(subcommands("namespace").contains(&"eval"));
        assert!(subcommands("puts").is_empty());
    }
}
