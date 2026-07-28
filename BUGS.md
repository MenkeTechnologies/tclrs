# Known gaps

An honest list of what tclrs does **not** do yet. Every unsupported construct is
refused at compile time with a Tcl-shaped message and a line number — nothing is
approximated, and nothing is silently mis-run.

## Implemented

- **The parser.** All twelve syntax rules of `Tcl(n)`: command and word
  splitting, double quotes, `{*}` recording, brace nesting, command
  substitution, the four variable-substitution forms, the full backslash escape
  table (including the backslash-newline pre-pass), first-word comments, and the
  single-pass order guarantee (`src/parser.rs`).
- **Commands.** `set`, `puts` (with `-nonewline`), `expr`, `incr`, `if` /
  `elseif` / `else`, `while`, `break`, `continue`, and command substitution of
  any of them (`src/compiler.rs`).
- **`expr`.** The whole operator set of `expr(n)` with `expr(n)` precedence,
  compiled straight from a braced word with no runtime parse: `+ - * / % **`,
  unary `+ - ~ !`, `< > <= >= == !=`, the always-string `lt gt le ge eq ne`,
  `& ^ | << >>`, short-circuiting `&& ||`, and the ternary (`src/expr.rs`).
- **Tcl arithmetic.** Floored integer division and remainder, integral `**` for
  integral operands, numeric-preferring comparison with string-order fallback,
  and Tcl's double formatting (`src/runtime.rs`).

## Not implemented

- **`proc`.** No procedure definition, no `return`, no `upvar` / `global`, no
  call frames. Phase 4.
- **Every command outside the eight above.** `foreach`, `for`, `switch`,
  `string`, `regexp`, `catch` / `error`, `lindex` / `llength` / `lappend`,
  `list`, `open` / `read` / `close`, `source`, `eval`, `format`, `array`, … An
  unknown command name is `invalid command name "…"` at compile time rather than
  at run time, which is where a later phase's runtime command table will move
  it.
- **Lists.** No list values, no list parsing, and therefore no `{*}` expansion —
  the parser records `{*}` on the word, but the compiler refuses it. `in` and
  `ni` parse and lower to extension ops that report that list support is not
  built yet. Phase 3.
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
byte for byte (`tests/execution_differential.rs`, `tests/differential_tclsh.rs`).
A divergence found here is a bug to fix, not a documented difference.
