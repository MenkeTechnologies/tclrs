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
- **Commands.** `set`, `puts` (with `-nonewline`), `expr`, `incr`, `if` /
  `elseif` / `else`, `while`, `foreach`, `break`, `continue`, and command
  substitution of any of them (`src/compiler.rs`).
- **Lists.** List parsing and canonical quoting ported from `TclFindElement` and
  `TclScanElement` / `TclConvertElement` (`src/list.rs`), plus `list`,
  `llength`, `lindex`, `lappend`, `lrange`, `lreverse`, `linsert`, `lreplace`,
  `lsearch`, `lsort`, `join`, `split` and `concat` (`src/cmd_list.rs`). `in` and
  `ni` test string membership. Index expressions (`end`, `end±n`, `m±n`) follow
  `Tcl_GetIntForIndex`.
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

## Not implemented

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
  command in the resumer's context, which needs the runtime evaluator that
  arrives with `eval`.
- **`info`, apart from `info coroutine`.** Every other subcommand is refused by
  name rather than mis-answered.
- **Every command outside those above.** `for`, `switch`, `string`, `regexp`,
  `catch` / `error`, `lassign`, `lset`, `lrepeat`, `lremove`, `lpop`, `ledit`,
  `lmap`, `lseq`, `dict`, `open` / `read` / `close`, `source`, `eval`, `format`,
  `array`, … An unknown command name is `invalid command name "…"` at compile
  time rather than at run time, which is where a later phase's runtime command
  table will move it.
- **`{*}` expansion.** The parser records `{*}` on the word and the list
  splitter it needs now exists, but the compiler still refuses it. Phase 3.
- **List command options.** `lsearch -regexp`, `-sorted`, `-bisect`,
  `-dictionary`, `-nocase`, `-index`, `-stride`, `-subindices` and `lsort
  -command`, `-dictionary`, `-nocase`, `-index`, `-stride` are recognised by the
  option parser — so abbreviation and ambiguity behave as tclsh does — and then
  refused. `-nocase` waits on a case-folding table that matches Tcl's, which
  Rust's `to_lowercase` does not: it is a full case mapping and can produce more
  than one character where Tcl maps one to one.
- **Indices outside `i64`.** Tcl computes index arithmetic in arbitrary
  precision and truncates; tclrs saturates at the `i64` ends instead. Both
  produce an index far outside any list, so no case is known where the two
  differ, but the mechanism is not the same one.
- **Arrays.** `$name(index)` parses into a `Part::Elem`, but the compiler
  refuses it: there are no array variables. Phase 4.
- **Math functions.** `sin(x)`, `sqrt(x)`, `int(x)`, `rand()` and the rest parse
  into an `Expr::Call` that the compiler refuses. Phase 5.
- **Non-literal variable and body words.** A variable name or a body that is
  itself the result of substitution (`set $name 1`, `while $cond $body`) is
  refused — those need the runtime evaluator that arrives with `eval`.
- **Arbitrary-precision integers.** Tcl promotes an overflowing integer to a
  bignum. tclrs has no bignum, so an operation that overflows `i64` is
  `integer value too large to represent` rather than a silent wrap. Phase 6.
- **`tclsh` binary.** The crate is a library; there is no CLI, no REPL, and no
  script driver. Phase 4.
- **JIT / AOT.** `fusevm` is pulled with its default features, so the VM's
  interpreter tier executes the chunk and no `cranelift-*` crate is linked. The
  `jit` / `jit-disk-cache` / `aot` features arrive in phase 6, with the
  benchmarks that justify them.
- **Editor tooling.** No LSP, no DAP, no zsh completion, no man pages, no
  `reference.html`, no inline `rust {}` FFI, no `--dump-tokens` /`--dump-ast` /
  `--disasm`. Phase 7 is toolchain parity with the sibling `fusevm` frontends.

## Divergences from tclsh where behavior *is* implemented

None known. Every implemented construct is covered by a differential test that
runs the same source through `tclsh` 9.0.4 and tclrs and compares the output
byte for byte (`tests/execution_differential.rs`, `tests/list_differential.rs`,
`tests/coroutine_differential.rs`, `tests/differential_tclsh.rs`). A divergence found here is a bug to fix, not a
documented difference.

## Defects in the reference implementation

- **`lsearch -start` on an empty list crashes tclsh 9.0.4.** A script whose only
  line is `puts [lsearch -start -1 {} e1]` exits with SIGSEGV: a negative index
  against an empty list resolves to the most negative `Tcl_Size`, and the scan
  loop starts there. tclrs treats it as a start of 0 and reports no match, which
  is what tclsh does for the same index against a non-empty list. The
  combination is excluded from the generated index matrix in
  `tests/list_differential.rs`, since there is no reference output to compare
  against.
