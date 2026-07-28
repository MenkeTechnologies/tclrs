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
  integral operands — a negative exponent included, where the integral result
  truncates to 0 or ±1 and a zero base is an error — numeric-preferring comparison
  with string-order fallback, Tcl 9's integer grammar (the `0x` / `0o` / `0b` /
  `0d` prefixes and `_` as numeric whitespace), and Tcl's double formatting
  (`src/runtime.rs`).
- **Boolean conditions.** `ParseBoolean` and `Tcl_GetBoolFromObj` ported from
  `tclObj.c`: a condition is a number or one of `true` / `false` / `yes` / `no` /
  `on` / `off`, abbreviated to any unambiguous prefix, in any case — everything
  else is `expected boolean value but got …`, which the VM's own truthiness would
  have accepted. Reached from `if`, `while`, `for`, the ternary, `&&`, `||` and `!`
  through one extension op, emitted only where the value could be a string so that
  a counted loop's test stays native and traceable (`src/runtime.rs`,
  `src/compiler.rs`).
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

- **The JIT compiles nothing for a loop outside a procedure.** A `while` or `for`
  loop *inside* a `proc` does reach a compiled trace — `tclrs --tiers` reports
  `traced=true`, and the benchmark row is 37× the interpreter. Two things had to
  hold for that, and only one of them generalises:
  - **Shape — fixed.** Every loop is now emitted rotated: entered at its test,
    closed by a conditional backward branch (`Compiler::rotated_loop`,
    `src/compiler.rs`). The textbook `while` shape — a forward `JumpIfFalse` exit
    closed by an unconditional backward `Jump` — records an op sequence
    `is_trace_eligible` accepts and fusevm's trace compiler then declines, so
    nothing was ever installed. Both shapes are pinned by hand-built chunks with
    no Tcl involved in `src/tiers.rs`.
  - **Ops — still open at the top level.** A Tcl variable at a script's top level
    is a VM global, and `Op::GetVar` / `Op::SetVar` are absent from fusevm's
    `is_block_eligible_op_at` (`fusevm-0.14.20/src/jit.rs:4249`), which both the
    block tier (`:4419`) and the tracing tier (`is_trace_op_allowed_at`, `:6180`)
    require. Rotation does not touch this: a top-level loop reports
    `trace-eligible=false` before its shape is consulted. Slot-allocating a
    top-level variable whose name is known at compile time would fix it.
  - **`foreach` and `dict for` reach no tier in any spelling**, procedure locals
    included. Their loop state is carried by frontend extension ops
    (`FOREACH_INIT` / `MORE` / `TAKE` / `ADVANCE`, `DICT_PAIRS`) and
    `is_trace_op_allowed_at` rejects `Op::Extended` outright — an extension
    handler is arbitrary Rust with no Cranelift lowering. Lowering their state to
    native ops is the fix; rotation is not.
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
  divided by `-1` is the same case, and so is an integer *literal* or operand that
  does not fit at all — `expr {99999999999999999999 + 1}` is the overflow, not the
  `1e+20` a fall-through to the double parser used to answer.
- **Editor tooling.** No LSP, no DAP, no zsh completion, no man pages, no
  `reference.html`, no inline `rust {}` FFI, no `--dump-tokens` / `--dump-ast`.
  `--disasm` exists.

## Divergences from tclsh where behavior *is* implemented

Found by `scripts/fuzz_parity.sh`, the differential fuzzer: it generates seeded
random Tcl programs, runs each under both `tclsh` 9.0.4 and tclrs, and minimises
whatever diverges. One run of 400 programs (seed 1, depth 3) put 181 in parity,
170 in divergence, 16 in skip, 32 in the allowlist and 1 outside comparison
because tclsh did not terminate. Each entry below is a **reproduced** divergence
with the reducer's own one-statement case; every one is pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh, and the committed corpus of
minimised cases is `tests/fuzz_corpus/`.

Repro helper:

```sh
T=./target/debug/tclrs
tref() { printf '%s\n' "$1" >/tmp/c.tcl; tclsh /tmp/c.tcl; }   # ground truth
```

- **`expr` coerces a non-numeric string to zero outside a boolean position.**
  `expr {"b" >> 1}` answers 0, `expr {~"b"}` answers -1, and `&`, `|`, `^`, `<<`
  likewise; tclsh raises `cannot use non-numeric string "b" as left operand of
  ">>"`. In a boolean position the coercion is gone — see the fixed list below —
  and `!` now refuses the operand too, only in the older wording of the next
  entry.
- **`expr`'s operand errors use Tcl 8's wording.** tclsh 9.0.4 names the value and
  which side of the operator it was on — `cannot use non-numeric string "abc" as
  right operand of "+"` — where tclrs says `can't use non-numeric string as
  operand of "+": "abc"`. Same for `cannot use floating-point value "1.0" as left
  operand of "%"`. `format` differs the same way: `expected integer but got a
  list` against `expected integer but got "{a b} c"` — `format` is the one place
  left that quotes a list-shaped value, since `incr` and the boolean rule both go
  through `list::looks_like_a_list` now.
- **`incr` on a non-integer *variable* reports an `expr` operand error.**
  `set x abc; incr x` says `can't use non-numeric string as operand of "+": "abc"`
  where tclsh says `expected integer but got "abc"`. An increment the script wrote
  out (`incr x abc`) is checked while compiling and does report `incr`'s own
  wording; the variable's value cannot be, because the check would have to be an
  extension op in the `incr` lowering and `is_trace_op_allowed_at` rejects
  `Op::Extended` — every loop that counts with `incr` would lose its compiled
  trace, which is the one thing this frontend has that reaches native code.
  Deliberately not taken.
- **Parse errors inside `expr` are worded differently.** `missing operand at _@_`
  against `premature end of expression`, and `invalid bareword "end"` against
  `invalid bare word "end" in expression`. The *character* diagnostic agrees now.
- **`expr` does not take Tcl 9's `0d` prefix in a literal.** `expr {0d9 + 1}` is
  `extra characters after expression` where tclsh answers 10. The runtime number
  parser does take it, so `set x 0d9; expr {$x + 1}` is right; it is the literal
  path in `expr::parse_number` that is short.
- **Unreachable code is still compiled**, so a script tclsh runs to completion can
  be refused outright: `if {0} {incr}` is `wrong # args`, `if {0} {puts [expr {1
  +}]}` is `premature end of expression`, and `if {0} {nosuchcommand}` is
  `invalid command name`, and a `switch` arm that is never selected is parsed too:
  `switch -- x {*b {puts "a}}` is `missing "` where tclsh never looks inside the
  braced body. The mechanism is documented (README [0x05], errors "at compile time
  where the script's shape decides it"); this consequence is not.

  **This is the largest single class by a wide margin: 106 of the 170 divergences
  in the 400-program run (seed 1, depth 3) are it**, because any dead branch a
  generated program happens to contain takes the whole script down. The minimal
  case is one line:

  ```tcl
  if {0} {nosuchcommand}
  ```

  tclsh runs that script to completion and prints nothing; tclrs refuses it with
  `invalid command name "nosuchcommand"` before running anything. It is not a
  patchable defect — resolving a command name while compiling is what makes a
  call a `Op::Call` to a known sub instead of a runtime table lookup, and it is
  the same mechanism behind the arity and `expr`-shape refusals above. Changing it
  is an architectural decision about compile-time dispatch resolution, not a bug
  fix, and it is left as it is.
- **Arbitrary-precision integers, seen from the fuzzer.** An integer beyond `i64`
  is refused with `integer value too large to represent` where tclsh promotes and
  answers exactly, so `expr {99999999999999999999 + 1}` is an error against
  `100000000000000000000`. The report counts those as skips, not divergences,
  because the refusal is the documented behavior — what was a divergence, and is
  fixed, was answering `1e+20` instead of refusing at all.
- **`format`'s floating-point conversions lose precision on an integer past
  `i64`.** `format %.2f 99999999999999999999` prints
  `100000000000000016384.00` against tclsh's `100000000000000000000.00`:
  `cmd_string::parse_double` accumulates the digits in an `f64`, and tclsh
  converts the bignum. The same missing bignum as above, in the one place that
  answers rather than refusing.

### Fixed by the fuzzer's own findings

Each of these was a divergence in the run above and is now parity, pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh:

- A **float literal keeps its spelling**: `puts 3.0` prints `3.0`. It was interned
  as a `Value::Float`, which `puts` stringifies through fusevm's `as_str_cow`
  rather than Tcl's formatter.
- **A condition is a Tcl boolean**: `if {"b"}` is `expected boolean value but got
  "b"`, and so are `while`, `for`, the ternary, `&&` and `||`. `!` refuses the
  operand rather than answering 0.
- **Integral `**` stays integral for a negative exponent**: `expr {2 ** -1}` is 0,
  and a zero base is `exponentiation of zero by negative power`.
- **An out-of-`i64` integer is refused** rather than silently becoming a double.
- **`format %.2f -0`** prints `0.00`; the double `-0.0` still keeps its sign.
- **`incr x abc`** reports `expected integer but got "abc"`.
- **A character `expr` cannot use** is `invalid character "Ü"`, not the lead byte
  of its UTF-8 encoding.
- **A failure inside a body** is located at the script's own command, which is the
  line tclsh's `(file "…" line N)` names.
- **Input nesting is bounded** by `parser::MAX_NESTING_DEPTH` (64_000, measured),
  so the deepest input reports a Tcl error instead of aborting the process. The
  limit sits above every depth tclsh survives — it segfaults on 30_000 nested `[`
  — so nothing tclsh can parse became a refusal. Found by the `parse` cargo-fuzz
  target (`fuzz/fuzz_targets/parse.rs`), not by the differential fuzzer: no
  generated *program* has fifty thousand open brackets. A host embedding the
  library on a stack smaller than `runtime::RECOMMENDED_STACK` still has to give
  the parser the stack this crate documents; the limit is calibrated for that one.

The five divergences the fuzzer's report allowlists rather than counting are the
documented ones, and each is pinned in `tests/parity_fuzz_findings.rs` too, so an
entry cannot outlive the behavior it excuses: an unset variable reading as `""`,
an unterminated brace located where the input ran out, `array names` / `array
get` sorted where tclsh hashes (order is unspecified in `array(n)`), arity
refused before anything runs, and a message carrying ` (line N)` through the
library. `scripts/fuzz/classify.pl` holds them with their reasons, and every run
prints a hit count per entry.

## Crashes reachable from a script

A crash is worse than any divergence: the differential harness calls it
`CRITICAL` and never suppresses one, and none of these can be caught by `catch` —
the interpreter thread unwinds or the process aborts, so the script's own error
handling never sees it. Each was found by auditing for panics on the class the
boolean rule exposed (`&body[..2]` in the number parser), each is measured, and
each is still open. `format`'s are the ones a generated program is likeliest to
reach, since the generator draws widths and precisions from a pool.

- **`format`'s floating-point precision above 65535 panics.** Rust's formatter
  holds precision in a `u16`, and the four sites that call it take the number
  straight from the script: `format %.65536f 1.0`, `format %.65536e 1.0`,
  `format %.65535g 0.0001` and `format %.70000g 1e-5` are
  `Formatting argument out of range` (`src/cmd_string.rs:1651`, `:1671`, `:1693`,
  `:1711`). tclsh produces the digits — `string length [format %.65536f 1.0]` is
  65538 there — so matching it means generating them rather than bounding the
  precision, which Rust's formatter will not do.
- **`format`'s field width is unbounded.** `format %9223372036854775807d 1`
  is `memory allocation of 9223372036854775806 bytes failed`, an abort rather than
  a panic (`src/cmd_string.rs:1500`, `push_padded`). tclsh reports
  `max size for a Tcl value exceeded`. `string repeat` already has a 2 GiB guard
  (`src/cmd_string.rs:511`); `format` has none, and the two should share one.
- **`expr`'s parser recursion is unbounded.** `expr {((((…1…))))}` overflows the
  stack between 5_000 and 10_000 parentheses on the stack the binary gives it, and
  a unary chain does the same at around 150_000. `src/parser.rs` bounds its own
  recursion at `MAX_NESTING_DEPTH` for exactly this reason and `src/expr.rs` does
  not, so the fix is the same mechanism in `ExprParser::parse_binary` /
  `parse_unary`. It fires while *compiling*, so `fuzz/fuzz_targets/compile.rs`
  reaches it with a 10 KB input.

## Defects in the reference implementation

- **`lsearch -start` on an empty list crashes tclsh 9.0.4.** A script whose only
  line is `puts [lsearch -start -1 {} e1]` exits with SIGSEGV: a negative index
  against an empty list resolves to the most negative `Tcl_Size`, and the scan
  loop starts there. tclrs treats it as a start of 0 and reports no match, which
  is what tclsh does for the same index against a non-empty list. The
  combination is excluded from the generated index matrix, since there is no
  reference output to compare against.
- **Deep nesting segfaults tclsh 9.0.4.** A script of 50_000 `[` exits on a signal
  under tclsh while tclrs reports `missing close-bracket`, and tclsh is already
  gone at 30_000. tclrs bounds its parser at `MAX_NESTING_DEPTH` and reports
  `too many nested substitutions (infinite loop?)` past it, so it no longer dies at
  100_000 either. The differential fuzzer counts a case tclsh cannot survive as
  `EXCLUDED` — there is no reference behavior to compare with — and never charges
  it against tclrs.
- **`expr {2 ** 123456789}` does not finish in any useful time.** tclsh computes
  the bignum; the fuzzer's ten-second per-process timeout ends the run and
  classifies the case as `EXCLUDED`. tclrs reports the overflow immediately. This
  is why both sides of the harness are timed, not only the subject.
