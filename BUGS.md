# Known gaps

An honest list of what tclrs does **not** do yet. Every unsupported construct is
refused with a Tcl-shaped message — at compile time with a line number where the
script's shape decides it, at run time where a value does. Nothing is
approximated, and nothing is silently mis-run.

## Implemented

- **The parser.** All twelve syntax rules of `Tcl(n)`: command and word
  splitting, double quotes, `{*}` recording, brace nesting, command
  substitution, the four variable-substitution forms, the full backslash escape
  table (including the backslash-newline pre-pass), first-word comments, and the
  single-pass order guarantee (`src/parser.rs`).
- **Commands.** `set`, `puts` (with `-nonewline`), `expr`, `incr`, `unset`,
  `append`, `if` / `elseif` / `else`, `while`, `for`, `foreach`, `switch`,
  `break`, `continue`, `global`, and command substitution of any of them
  (`src/compiler.rs`, `src/control.rs`).
- **Procedures.** `proc` and `return`, with a procedure's parameters and locals
  as frame slots rather than entries in the global table (`src/procs.rs`).
  Signatures are collected before anything is emitted, so a procedure may call
  one the script defines further down; defaults and a trailing `args` are
  resolved at the call site.
- **Errors.** `catch` and `error`. A `catch` region is an extension-wide op whose
  payload is its handler's op index; the driver in `src/runtime.rs` unwinds the
  value stack and the call frames to the region's entry state and resumes at the
  handler, so an error raised inside a procedure the guarded script called is
  caught correctly (`src/control.rs`).
- **Lists.** List parsing and canonical quoting ported from `TclFindElement` and
  `TclScanElement` / `TclConvertElement` (`src/list.rs`), plus `list`,
  `llength`, `lindex`, `lappend`, `lrange`, `lreverse`, `linsert`, `lreplace`,
  `lsearch`, `lsort`, `join`, `split` and `concat` (`src/cmd_list.rs`). `in` and
  `ni` test string membership. Index expressions (`end`, `end±n`, `m±n`) follow
  `Tcl_GetIntForIndex`.
- **Associative data.** Array variables (`a(k)`), `array` — `exists`, `get`,
  `names`, `set`, `size`, `unset` — and `dict` — `create`, `exists`, `get`,
  `keys`, `merge`, `remove`, `set`, `values` (`src/assoc.rs`).
- **Strings.** `format` and the `string` ensemble — `cat`, `compare`, `equal`,
  `first`, `last`, `index`, `insert`, `is`, `length`, `map`, `match`, `range`,
  `repeat`, `replace`, `reverse`, `tolower`, `totitle`, `toupper`, `trim`,
  `trimleft`, `trimright` (`src/cmd_string.rs`).
- **`expr`.** The whole operator set of `expr(n)` with `expr(n)` precedence,
  compiled straight from a braced word with no runtime parse: `+ - * / % **`,
  unary `+ - ~ !`, `< > <= >= == !=`, the always-string `lt gt le ge eq ne`,
  `& ^ | << >>`, short-circuiting `&& ||`, and the ternary (`src/expr.rs`).
- **Tcl arithmetic.** Floored integer division and remainder, integral `**` for
  integral operands, numeric-preferring comparison with string-order fallback,
  and Tcl's double formatting (`src/runtime.rs`).
- **Coroutines.** `coroutine`, `yield`, `yieldto`, `info coroutine` and the
  lifecycle of a context command (`src/coro.rs`). A coroutine is a second
  `fusevm::VM` over the same chunk, suspended by the halt-and-request mechanism
  fusevm's scheduler is built on; the driver in `src/runtime.rs` owns the
  transfer and the one global variable table every context shares. A body may
  suspend at any depth, inside a loop, and inside an open `catch` region; an
  error that escapes a body deletes the coroutine and is reported to whatever
  resumed it.
- **Interpreter state and `eval`.** `Interp` holds the variables of a session
  between evaluations, keyed by name, with a source-keyed cache of the chunks
  compiled for it (`src/cache.rs`). `eval` compiles and runs a script built at
  run time against that same state (`src/runtime.rs`).
- **The binary and the REPL.** A script file, `-c script`, or stdin — a REPL
  when stdin is a terminal — with tclsh's exit statuses and stderr wording
  (`src/main.rs`, `src/repl.rs`).
- **JIT and ahead-of-time compilation.** `fusevm` is pulled with `jit`,
  `jit-disk-cache` and `aot`. Every VM this crate builds arms the tracing JIT;
  `src/aot.rs` lowers a script to a native object and links it into a standalone
  binary; `src/tiers.rs` reports which tiers a script's bytecode actually
  reaches. What that report says today, and why, is in the README.

## Not implemented

- **The JIT compiles nothing for a Tcl script.** Two independent blockers, both
  measured rather than assumed, both outside this crate's lowering:
  - A Tcl variable at a script's top level is a VM global, and `Op::GetVar` /
    `Op::SetVar` are absent from fusevm's `is_block_eligible_op_at`
    (`fusevm-0.14.20/src/jit.rs:4249`), which both the block tier (`:4419`) and
    the tracing tier (`is_trace_op_allowed_at`, `:6180`) require. Slot-allocating
    a top-level variable whose name is known at compile time would fix this
    half.
  - Inside a procedure the counter *is* a slot and the loop body *is* reported
    trace-eligible, and no trace is still installed: fusevm's trace installer
    takes a do-while whose conditional backward branch closes the loop and
    declines the while-do shape — a forward conditional exit closed by an
    unconditional backward `Jump` — that `while` and `for` lower to. Reproduced
    directly against fusevm 0.14.20 with the same bytecode and no Tcl involved.
- **Ahead-of-time compilation of `catch` or a coroutine.** Both are driven from
  outside `VM::run`, and fusevm's ahead-of-time entry owns the run, so `--aot`
  refuses the script rather than compiling one that would turn a caught error
  into a fatal one.
- **`eval` inside a procedure body.** A procedure's locals are frame slots and
  the nested script is a chunk of its own that addresses globals, so it could not
  see them. Refused rather than run against the wrong variables.
- **`coroprobe` and `coroinject`.** Inspecting or injecting a command into a
  suspended coroutine is not implemented; both are `invalid command name`.
  Deleting a coroutine by destroying its command needs `rename`, which is not
  implemented either — a coroutine goes away when its body ends.
- **Coroutines of anything but a procedure of the script.** `coroutine`'s name
  and command are literals, its command is one of the script's own procedures,
  and the command appears at the top level of a script or in a command
  substitution in one, because the name has to be known to every call site and
  the body is entered through the chunk's sub table. `yieldto` at a command that
  is not a coroutine of the script is refused: it would have to evaluate that
  command in the resumer's context, which this frontend cannot do.
- **`info`, apart from `info coroutine`.** Every other subcommand is refused by
  name rather than mis-answered.
- **Every command outside those above.** `regexp`, `lassign`, `lset`, `lrepeat`,
  `lremove`, `lpop`, `ledit`, `lmap`, `lseq`, `open` / `read` / `close`,
  `source`, `upvar`, `uplevel`, `rename`, `namespace`, `apply`, `clock`,
  `encoding`, `binary`, … An unknown command name is `invalid command name "…"`
  at compile time rather than at run time, which is where a runtime command
  table would move it.
- **`{*}` expansion.** The parser records `{*}` on the word and the list splitter
  it needs exists, but the compiler still refuses it.
- **Subcommands and options recognised and then refused.** `array startsearch`
  and the other search subcommands; `dict` subcommands outside the implemented
  set, and `dict set` into an array element; `string` subcommands outside the
  implemented set, and `string is -failindex`; `format` conversions outside the
  implemented set; `lsearch -regexp`, `-sorted`, `-bisect`, `-dictionary`,
  `-nocase`, `-index`, `-stride`, `-subindices`; `lsort -command`,
  `-dictionary`, `-nocase`, `-index`, `-stride`; `catch`'s options variable;
  `error`'s `info` and `code` arguments; `return`'s options other than
  `-code ok` / `-code error`. They go through the reference option parser first,
  so abbreviation and ambiguity behave as tclsh does, and are then refused.
  `-nocase` waits on a case-folding table that matches Tcl's, which Rust's
  `to_lowercase` does not: it is a full case mapping and can produce more than
  one character where Tcl maps one to one.
- **`array` and `dict` on a procedure-local variable.** An array lives in the
  global table keyed by a name index; a procedure's locals live in the frame's
  slots, which no name index reaches. Refused rather than silently made global —
  unless `global` already said that is what it is.
- **An array variable in a `foreach` variable list.** Refused.
- **Indices outside `i64`.** Tcl computes index arithmetic in arbitrary
  precision and truncates; tclrs saturates at the `i64` ends instead. Both
  produce an index far outside any list, so no case is known where the two
  differ, but the mechanism is not the same one.
- **Math functions.** `sin(x)`, `sqrt(x)`, `int(x)`, `rand()` and the rest parse
  into an `Expr::Call` that the compiler refuses.
- **Non-literal variable and body words.** A variable name or a body that is
  itself the result of substitution (`set $name 1`, `while $cond $body`) is
  refused.
- **Arbitrary-precision integers.** Tcl promotes an overflowing integer to a
  bignum. tclrs has no bignum, so an operation that overflows `i64` is
  `integer value too large to represent` rather than a silent wrap. `i64::MIN`
  divided by `-1` is the same case.
- **Editor tooling.** No LSP, no DAP, no zsh completion, no man pages, no
  `reference.html`, no inline `rust {}` FFI, no `--dump-tokens` / `--dump-ast`.
  `--disasm` exists.

## Divergences from tclsh where behavior *is* implemented

None known. Every implemented construct is covered by a differential test that
runs the same source through `tclsh` 9.0.4 and tclrs and compares the output
byte for byte. A divergence found there is a bug to fix, not a documented
difference.

## Defects in the reference implementation

- **`lsearch -start` on an empty list crashes tclsh 9.0.4.** A script whose only
  line is `puts [lsearch -start -1 {} e1]` exits with SIGSEGV: a negative index
  against an empty list resolves to the most negative `Tcl_Size`, and the scan
  loop starts there. tclrs treats it as a start of 0 and reports no match, which
  is what tclsh does for the same index against a non-empty list. The
  combination is excluded from the generated index matrix, since there is no
  reference output to compare against.
