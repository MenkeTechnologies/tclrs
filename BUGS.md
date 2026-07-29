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
  `lsearch`, `lsort`, `join`, `split`, `concat`, `lassign`, `lset`, `lpop`,
  `ledit`, `lrepeat`, `lremove`, `lseq` and `lmap` (`src/cmd_list.rs`). `in` and
  `ni` test string membership. Index expressions (`end`, `end±n`, `m±n`) follow
  `Tcl_GetIntForIndex`. `lappend` reaches its variable itself instead of taking
  the value through `GetVar`, so the elements go onto the list's own string and
  growing a list is linear rather than quadratic; a list another variable holds
  is copied instead of extended, which is what keeps that invisible to a script.
- **Associative data.** Array variables (`a(k)`), `array` — `exists`, `get`,
  `names`, `set`, `size`, `unset` — and `dict` — `create`, `exists`, `get`,
  `for`, `keys`, `merge`, `remove`, `set`, `size`, `values` (`src/assoc.rs`).
  `dict for` is emitted by the same `Compiler::rotated_loop` every other loop
  goes through, over a cursor the VM's own `ArrayLen` / `ArrayGet` walk.
- **Strings.** `format` and the `string` ensemble — `cat`, `compare`, `equal`,
  `first`, `last`, `index`, `insert`, `is`, `length`, `map`, `match`, `range`,
  `repeat`, `replace`, `reverse`, `tolower`, `totitle`, `toupper`, `trim`,
  `trimleft`, `trimright` (`src/cmd_string.rs`). `append` reaches its variable
  itself instead of taking the value through `GetVar`, so the values go onto the
  string the variable already holds and growing a string is linear rather than
  quadratic; `set x "$x…"` is lowered as the same op when the word only grows
  `x` and nothing after the leading `$x` can run a script, which is the case
  where the two would read the variable at different times. A string another
  value holds is copied instead of extended.
- **`expr`.** The whole operator set of `expr(n)` with `expr(n)` precedence,
  compiled straight from a braced word with no runtime parse: `+ - * / % **`,
  unary `+ - ~ !`, `< > <= >= == !=`, `lt gt le ge eq ne`, `& ^ | << >>`,
  short-circuiting `&& ||`, and the ternary (`src/expr.rs`). `lt` … `eq` are
  always-string, on the operands as written: a numeric literal carries its
  spelling as well as its value, so `expr {1.0 eq 1}` is 0 like tclsh's. Nothing
  converts an `expr` *result* — it stays the integer, double or boolean the VM
  computed, and Tcl's string form is applied where a string is asked for, which
  is what leaves an arithmetic loop lowerable by the JIT and the ahead-of-time
  compiler.
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
  reaches. Every counted `while` / `for` loop reaches a compiled trace, whether
  its counter is a procedure's frame slot or a script's top-level variable: the
  loop is emitted rotated so fusevm's trace compiler accepts its shape
  (`Compiler::rotated_loop`), nothing in an `expr` is an extension op any more,
  and fusevm 0.15.0 promotes the globals a trace references to registers at
  entry and spills them at every exit. `--aot` lowers the same loops
  closed-world. What the tier report says today, and the numbers, are in the
  README.
- **Editor servers.** `tclrs --lsp` speaks the Language Server Protocol on
  stdio — diagnostics from the parser and then the compiler, completion and
  hover from the same tables the REPL completes from, signature help and
  document symbols (`src/lsp.rs`, driven end to end over the wire by
  `tests/lsp_session.rs`). `tclrs --dap` speaks the Debug Adapter Protocol:
  breakpoints, stepping, stack frame, variables and the program's output as
  events, stopping on `ext_wide::DBG_LINE` markers `compiler::compile_debug`
  emits and an ordinary compilation does not (`src/dap.rs`,
  `tests/dap_session.rs`).
- **Inline Rust.** A `rust { ... }` block is rewritten before parsing into
  `__rust_compile <base64> <line>`, compiled to a shared library through
  `fusevm::ffi` and cached by the hash of its body; its exports become Tcl
  commands, registered while the block is lowered rather than when the VM runs
  (`src/rust_ffi.rs`, `tests/rust_ffi.rs`). The signatures are fusevm's
  marshalling set: up to four `i64` returning `i64`, up to three `f64`
  returning `f64`, and `*const c_char` returning `i64` or `*const c_char`.
- **The rest of the toolchain.** `--disasm`, `--dump-tokens` and `--dump-ast`
  print the bytecode, the lexical output and the parse tree; the zsh completion
  is `completions/_tclrs`; the manual pages are `man/man1/tclrs.1` and the
  all-in-one `man/man1/tclrsall.1`; and `docs/reference.html` is generated from
  the compiler's own tables by `cargo run --bin gen-docs` — every command, every
  ensemble subcommand with the compiler's own answer for whether it is
  implemented, the `expr` ladder as the parser binds it, and the `format`
  conversions the runtime answers to.

## Not implemented

- **`foreach` and `dict for` reach no tier in any spelling**, procedure locals
  included. Their loop state is carried by frontend extension ops
  (`FOREACH_INIT` / `MORE` / `TAKE` / `ADVANCE`, `DICT_PAIRS`) and
  `is_trace_op_allowed_at` rejects `Op::Extended` outright — an extension handler
  is arbitrary Rust with no Cranelift lowering. Lowering their state to native
  ops is the fix. A counted `while` or `for` loop does reach a compiled trace
  now, wherever its variables live — see the "Implemented" entry above.
- **Ahead-of-time compilation of `catch` or a coroutine.** Both are driven from
  outside `VM::run`, and fusevm's ahead-of-time entry owns the run, so `--aot`
  refuses the script rather than compiling one that would turn a caught error
  into a fatal one.
- **`eval` inside a procedure body.** A procedure's locals are frame slots and
  the nested script is a chunk of its own that addresses globals, so it could not
  see them. Refused rather than run against the wrong variables.
- **Procedures across an `eval`.** An evaluated script shares the interpreter's
  variables but not its procedures: it is a chunk of its own, and a call site
  resolves its command while compiling against that chunk's own `proc`
  definitions. So `eval {proc twice {x} {…}}` followed by `twice 21` is
  `invalid command name "twice"`, and so is `eval {twice 21}` for a procedure the
  outer script defined — both run in tclsh. A runtime command table shared across
  chunks is the fix; the same one that would move an unknown command name from
  compile time to run time.
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
- **Every command outside those above.** `regexp`, `open` / `read` / `close`,
  `source`, `upvar`, `uplevel`, `rename`, `namespace`, `apply`, `clock`,
  `encoding`, `binary`, … An unknown command name is `invalid command name "…"`
  at compile time rather than at run time, which is where a runtime command
  table would move it.
- **An array element as the variable a list command names.** `lappend a(x) v`,
  `lassign {1 2} a(x) a(y)`, `lset a(x) 0 v`, `lpop a(x)` and `ledit a(x) 0 0 v`
  are all `this command does not take an array element yet`, from the one
  `Compiler::var_name_of` that resolves the name for each of them. `foreach`
  refuses one the same way. tclsh takes them.
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
  `1e+20` a fall-through to the double parser used to answer. `<<` is the one
  operator that does *not* report and does wrap; it is a defect, recorded below.
- **Editor tooling.** No LSP, no DAP, no inline `rust {}` FFI. `--disasm`,
  `--dump-tokens` and `--dump-ast` exist, the zsh completion is
  `completions/_tclrs` and the man page is `man/man1/tclrs.1`, and
  `docs/reference.html` is generated from the compiler's own tables by
  `cargo run --bin gen-docs` — every command, every ensemble subcommand with the
  compiler's own answer for whether it is implemented, the `expr` ladder as the
  parser binds it, and the `format` conversions the runtime answers to.

## Divergences from tclsh where behavior *is* implemented

Found by `scripts/fuzz_parity.sh`, the differential fuzzer: it generates seeded
random Tcl programs, runs each under both `tclsh` 9.0.4 and tclrs, and minimises
whatever diverges. One run of 400 programs (`-n 400 -s 1`) puts 105 in parity,
225 in divergence, 50 in skip, 18 in the allowlist and 2 outside comparison
because tclsh did not terminate. **209 of the 225 are the one class below that is
not a defect** — a script's shape refused while compiling, which lands as a
message on a channel tclsh never reached — so 16 are behavior. The same command
against the generator as it was before the reach work put 182 in parity and 150
in divergence: a wider generator writes programs with more places to disagree,
not a worse implementation.

A later, wider run (`-n 2000 -s 1 -d 4`, 410 s) puts 399 in parity, 1272 in
divergence, 215 in skip, 107 in the allowlist and 7 outside comparison. 1192 of
the 1272 are again the compile-time class; 80 are behavior. The same command
against the generator as it was before the reach work put 880 in parity, 860 in
divergence and 99 in skip — more passes, because a narrower generator writes
programs with fewer places to disagree.

Mutation mode reaches the same buckets from the other direction: `-M -n 500 -s 21
-m` recombines the committed corpus and puts 68 in parity, 368 in divergence, 20
in skip and 44 in the allowlist, with **no case in `CRITICAL` and none in
`EXCLUDED`** — which is the evidence that the mutator's termination guard holds,
since a mutant that failed to terminate would be a timeout in one bucket or the
other.

Each entry below is a **reproduced** divergence with the reducer's own
one-statement case; every one is pinned in `tests/parity_fuzz_findings.rs` against
a live tclsh, and the committed corpus of minimised cases is `tests/fuzz_corpus/`.
The divergences that *were* here and are now parity are listed under "Fixed by the
fuzzer's own findings" at the end of the section.

Repro helper:

```sh
T=./target/debug/tclrs
tref() { printf '%s\n' "$1" >/tmp/c.tcl; tclsh /tmp/c.tcl; }   # ground truth
```

- **`incr` on a non-integer *variable* reports an `expr` operand error.**
  `set x abc; incr x` says `cannot use non-numeric string "abc" as left operand
  of "+"` where tclsh says `expected integer but got "abc"`. An increment the script wrote
  out (`incr x abc`) is checked while compiling and does report `incr`'s own
  wording; the variable's value cannot be, because the check would have to be an
  extension op in the `incr` lowering and `is_trace_op_allowed_at` rejects
  `Op::Extended` — every loop that counts with `incr` would lose its compiled
  trace, which is the one thing this frontend has that reaches native code.
  Deliberately not taken.
- **A double *literal* written in exponential form is quoted by its value.**
  `expr {1e300 % 2}` names `1e+300` where tclsh names `1e300`, and `2.5e-3`
  becomes `0.0025`. tclsh keeps an operand's original string representation and
  quotes that; a literal here is an `Op::LoadFloat` with no spelling left by the
  time an operator refuses it, so it is named by `runtime::format_double` — which
  is exactly right for a *computed* double, and for every spelling that is
  already canonical (`1.0`, `0.5`, `-0.0`, `1.0e-7`). An operand read from a
  variable is a `Value::Str` and is quoted verbatim, so only a literal is
  affected: 82 of the 7730 divergences in the four-run campaign.
- **Unreachable code is still compiled**, so a script tclsh runs to completion can
  be refused outright: `if {0} {incr}` is `wrong # args`, `if {0} {puts [expr {1
  +}]}` is `missing operand at _@_`, and `if {0} {nosuchcommand}` is
  `invalid command name`, and a `switch` arm that is never selected is parsed too:
  `switch -- x {*b {puts "a}}` is `missing "` where tclsh never looks inside the
  braced body. The mechanism is documented (README [0x05], errors "at compile time
  where the script's shape decides it"); this consequence is not.

  **This is the largest single class by a wide margin: 105 of the 150 divergences
  in the 400-program run (seed 1, depth 3) are it** — the harness names them
  `…-compile-time`, decided by re-running the case under `--disasm`, so the count
  is measured rather than read off the wording — because any dead branch a
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

  The sharp case is `expr {-9223372036854775808}`, where the *value* fits and the
  spelling does not: `expr(n)` reads it as unary minus applied to
  `9223372036854775808`, which is one past `i64::MAX`, so the operand is refused
  before the negation can bring it back. tclsh answers `-9223372036854775808`.
  Folding a leading sign into the literal in `expr::parse_number` would close it
  without a bignum.
- **`format`'s floating-point conversions lose precision on an integer past
  `i64`.** `format %.2f 99999999999999999999` prints
  `100000000000000016384.00` against tclsh's `100000000000000000000.00`:
  `cmd_string::parse_double` accumulates the digits in an `f64`, and tclsh
  converts the bignum. The same missing bignum as above, in the one place that
  answers rather than refusing.
- **`format`'s size limit is not checked for `%s` and `%c`.** Both refuse a field
  *width* past the limit, like every other conversion, but a *precision* past it
  is accepted: `format %.9223372036854775807s abc` is `abc` here and
  `max size for a Tcl value exceeded` under tclsh. Neither allocates — a `%s`
  precision truncates and a `%c` ignores it — so this is a message tclsh produces
  and tclrs does not, not a crash. Found while closing the size crashes below.
- **A field width too large for an `i64` reports the wrong message.**
  `format %99999999999999999999d 1` is `integer value too large to represent`
  here and `max size for a Tcl value exceeded` under tclsh. The *precision* in the
  same position saturates and reports tclsh's message
  (`cmd_string::format`); the width still parses and fails.

### Reached by the widened generator

Seven more, from the 2000-program run above. Each is pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh, and each is reachable only
because the generator now builds `format`'s specifier matrix, draws shift counts
with a sign, and carries `nan` / `inf` in its value pools.

- **A left shift past the word width is still the missing bignum.**
  `expr {1 << 63}` and `expr {1 << 64}` report `integer value too large to
  represent` where tclsh promotes and answers `9223372036854775808` and
  `18446744073709551616`. It used to *wrap* — `i64::MIN` and 1 — which was the
  one place a value silently changed instead of being refused; now it is the same
  documented refusal as every other overflow, and the remaining gap is the bignum
  itself.
- **`format`'s `-` flag does not override `0`.** `format %-08.2f 1.5` is
  `00001.50` against tclsh's `1.50    `, and `format %-08s ab` is `000000ab`
  against `ab000000`. The integer conversions already agree — `format %-08d 5` is
  `00000005` in both — so this is the `-`-against-`0` rule for `e`, `f`, `g` and
  `s`, not the padding as a whole. Reached only because the generator builds the
  specifier from its axes rather than drawing a fixed spelling.
- **A refusal decided at run time is catchable, so `catch` sees a message where
  tclsh saw an answer.** `catch {lsearch -sorted {a} b} m` leaves `m` as
  `lsearch -sorted -increasing is not supported yet` and the script runs on, where tclsh
  leaves `-1`; the same for `lsort -nocase`. The refusals decided while
  *compiling* — `string is punct`, `string wordstart` — are not catchable and do
  take the whole case out of comparison as a skip. The two halves are pinned
  together, because which side a refusal falls on is what decides whether the
  harness counts it as a skip or as a divergence.

### Fixed by the four-run campaign

Four runs of 4000 programs — seeds 1001 and 2002 at depth 4, 3003 and 4004 at
depth 6 — produced 7730 divergences, and these came out of grouping them by
signature. Each is pinned in `tests/parity_fuzz_findings.rs` against a live
tclsh, with the seed and case number of the divergence it was reduced from.

- **An `expr` operand refusal names the value and the side.** `cannot use
  non-numeric string "a" as left operand of "+"`, `cannot use floating-point
  value "1.0" as right operand of "%"`, and `as operand of` with no side for a
  unary operator. tclrs used Tcl 8's wording, which carried neither. 494 of the
  1420 run-time `message` divergences and 123 of the 413 `stdout` ones.
- **An operand that could be a list is named `a list`**, in `expr` as well as in
  `incr` and `format`: `expr {"a b c" + 1}` is `cannot use a list as left operand
  of "+"`, and `format %X [list {a b c} {}]` is `expected integer but got a
  list`. The screen is `list::looks_like_a_list`, so a one-element value is still
  quoted. 233 `message` divergences and 58 `stdout` ones.
- **The bitwise operators take integers.** `expr {1.5 | 2}` answered 3 and
  `expr {"abc" & 1}` answered 0, because fusevm's `Op::BitAnd` and friends coerce
  through `Value::to_int`; both are refusals now. These were wrong *answers*, not
  wrong wording. `&`, `|` and `^` keep the native op — and with it the tracing
  JIT — when the compiler can prove both operands integral
  (`Compiler::yields_integer`); the shifts never can, because neither the
  distance nor the overflow is knowable from an operand's shape.
- **A shift distance is checked and a right shift saturates.** `1 << -1` is
  `negative shift argument`, `1 >> 200` is 0 and `-1 >> 200` is -1, where
  fusevm's six-bit distance mask answered 0, 1 and 1.

  **What that costs, measured.** A shift is an extension op wherever it appears,
  and an extension op in a loop body costs that loop its trace and deopts its
  ahead-of-time compile. `bench/integer_arith.tcl`, whose body is
  `$sum + $i * $i - ($i >> 3)`, went from 6.4 ms to 187.5 in the JIT column and
  from 4.2 ms to 172.6 ahead-of-time — a 29× and 41× regression on that row,
  against tclsh's 284.7. `counted_loop` and `counted_loop_expr` are unaffected
  (5.8 and 5.4 ms JIT, 4.0 and 4.0 AOT) because neither shifts.

  The correctness is not in question and the answers are tclsh's now; what is
  missing is a lowering that keeps them without an extension op. Two candidates:
  prove the *left* operand integral the way `yields_integer` proves a literal —
  a variable assigned only from integer expressions in the same chunk is
  provable, and `$i` in that loop is — or lower the check into a guard fusevm's
  tracing tier accepts. A guard that *branches* is not one of them: a forward
  conditional that is taken on the recorded path costs the trace outright,
  measured on a loop whose body carried a never-entered `if`.
- **`%` checks its left operand first**, so `expr {1.5 % "a"}` names the float
  rather than the string, which is the order tclsh checks them in.
- **An exponent past what can be applied** is `exponent too large`, not the
  overflow the product would have reported.
- **`incr` on a variable that does not exist counts from zero.** `proc p {} {incr
  n; return $n}` was an operand refusal and is 1. `incr` keeps its native
  `Op::Add` — an extension op there costs `bench/counted_loop_proc.tcl` its trace
  — so the zero is read in the numeric hook, where an absent variable arrives as
  `Value::Undef`; no assignment produces that, since `set x ""` stores
  `Value::Str("")`, so the reading is exactly the `incr` case.
- **`expr` answers with the number an operand spells.** `expr {007}` is 7 and
  `expr {0x10}` is 16; the text used to pass through. An integer past `i64` still
  passes through, because its text is the only representation there is.
- **A NaN is reported rather than answered.** A NaN *result* is `domain error:
  argument not in valid range`; a NaN *operand* is `cannot use non-numeric
  floating-point value "nan" as left operand of "+"`.
- **`inf`, `infinity` and `nan` are expression literals**, in any case, and
  nothing that merely starts with one is: the set is what `f64::from_str` takes,
  which is the set tclsh's lexer takes.
- **`lsearch -increasing` and `-decreasing` are accepted.** They describe the
  order `-sorted` and `-bisect` binary-search in and change no answer without
  them, so refusing them turned a working search into an error. The two options
  that *would* read the order still say so, and now name the order they would
  have used.
- **`expr`'s compile-time diagnostics are the reference interpreter's.** tclsh
  lexes before it parses, so its refusals name the token: `invalid bareword "a"`,
  `missing operand at _@_` (the marker is literal on the first line; the position
  is on the second), `missing operator at _@_`, `missing operator ":" at _@_`,
  `empty expression`, `unbalanced open paren`, `unbalanced close paren` and
  `incomplete operator "="`, with `invalid character "@"` kept for a character
  that really is no token. 227 of the compile-time `message` divergences.
- **`#` starts a comment inside an expression**, running to the end of the line —
  which is why `expr {#1}` is `empty expression` in tclsh.

### Fixed by the fuzzer's own findings

Each of these was a divergence in the run above and is now parity, pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh:

- A **float literal keeps its spelling**: `puts 3.0` prints `3.0`. It was interned
  as a `Value::Float`, which `puts` stringifies through fusevm's `as_str_cow`
  rather than Tcl's formatter.
- **The always-string operators compare as written**: `expr {1.0 eq 1}`,
  `expr {010 eq 10}` and `expr {1e3 eq 1000.0}` are all 0. A numeric literal now
  carries the text the script wrote next to its value (`expr::Expr::Int` /
  `Float`), and the comparison is a frontend op over Tcl's string form of each
  operand rather than fusevm's `StrEq`, whose string form is the VM's.
- **`expr`'s literal number grammar is the whole integer grammar**: `expr {0d9}`,
  `expr {1_0}`, `expr {0x1_0}`, `expr {0b1_0}` and `expr {1_0.5}` answer 9, 10,
  16, 2 and 10.5. `_` is scanned as part of the literal and dropped before the
  parse, and `radix_literal` advances by the characters it consumed rather than
  the digits it kept.
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

## What the differential fuzzer cannot reach

The generator's own blind spots, so a gap in the report is a known gap rather
than an unexamined one. Measured against the 2000-program run above.

- **Commands tclrs does not have.** `{*}` expansion, `regexp`, `upvar`,
  `uplevel`, `namespace`, `apply`, `rename`, `source` and file I/O are outside
  the command set entirely, so a generated use of one is `invalid command name`
  and says nothing about parity. They are deliberately not generated, and belong
  in the generator on the day the commands exist. `lassign`, `lset`, `lpop`,
  `ledit`, `lrepeat`, `lremove`, `lseq` and `lmap` exist now and are not
  generated yet, so the run above says nothing about them either; what does is
  `tests/list_commands_differential.rs`.
- **`array` on a procedure local, `unset` of one, and `eval` inside a procedure
  body** *are* generated now, at `REFUSAL_RATE` — so are `lsort -command`,
  `lsearch -regexp`, `string wordstart`, `string is -failindex`, the `string is`
  classes that need the Unicode tables, and the `dict` subcommands outside the
  implemented set. Each lands in the skip bucket under the refusal's own wording,
  which is coverage waiting for the refusal to go rather than a hole. The rate is
  low because these refusals are decided while compiling, so one of them anywhere
  takes the whole case out of comparison: at 8 percent the run is 215 skips of
  2000; at roughly one in two it was 44 percent skips.
- **The two `format` crashes are out of the value pools on purpose.** An
  unbounded field width aborts the process on the allocation and a precision
  above 65535 panics; both are recorded under "Crashes reachable from a script"
  below and both are pinned. Drawing them would spend a run re-finding the same
  two aborts, so the generator bounds width and precision at two digits and the
  run's report prints that bound. Nothing about the classification changes: a
  case that reaches either crash from any other route is still `CRITICAL`.
- **Anything that needs a value the pools do not hold.** `format %c 55296` is a
  lone surrogate: tclsh fails the write with `invalid or incomplete multibyte or
  wide character` and tclrs prints U+FFFD. That was found by hand, not by the
  fuzzer, because no pool holds 55296.
- **Depth beyond the corpus contract.** One statement per line is what lets the
  shrinker reduce by deleting a line, so a body is inlined inside braces rather
  than spread over lines. A program whose *structure* spans lines — a procedure
  written across ten of them — is not generated, and a parse error that needs one
  is out of reach.

## Crashes reachable from a script

A crash is worse than any divergence: the differential harness calls it
`CRITICAL` and never suppresses one, and none of these can be caught by `catch` —
the interpreter thread unwinds or the process aborts, so the script's own error
handling never sees it. The first three were found by auditing for panics on the
class the boolean rule exposed (`&body[..2]` in the number parser), the rest by
the cargo-fuzz targets. Each is measured.

All of them are now closed, each pinned by a test in
`tests/parity_fuzz_findings.rs` that measures tclsh's own answer rather than
quoting one, and each with its reproducer in the seed corpus of the target that
reaches it.

- **`string replace` on an empty subject aborted the process.**
  `string replace {} -5 3` was `attempt to add with overflow`. The subject's
  `end` is -1 when it is empty, `last` clamps to that, and the cast to `usize`
  before the `+ 1` made the tail index wrap. **Fixed** by computing the tail
  signed and clamping it, which also brings the whole first/last matrix into
  agreement with tclsh — including `string replace {} -5 3 X`, which is `X`.
  Found by the four-run campaign (seed 3003 case 02453) and still reachable at
  v0.2.0.

- **`format`'s floating-point precision above 65535 panicked.** Rust's formatter
  holds precision in a `u16`, and the four sites that call it take the number
  straight from the script: `format %.65536f 1.0`, `format %.65536e 1.0`,
  `format %.65535g 0.0001` and `format %.70000g 1e-5` were
  `Formatting argument out of range`. **Fixed** by producing the digits Rust will
  not: a double's decimal expansion is finite — at most 1_074 fraction digits, for
  the smallest subnormal — so every digit past it is a zero, and formatting at the
  highest precision Rust accepts and appending zeroes is exact
  (`cmd_string::extend_exact`). tclrs now agrees with tclsh digit for digit:
  `string length [format %.65536f 1.0]` is 65538 on both, and `%#.70000g` keeps
  the trailing zeroes the plain form strips, at 70005 on both.
- **`format`'s field width was unbounded.** `format %9223372036854775807d 1`
  was `memory allocation of 9223372036854775806 bytes failed`, an abort rather
  than a panic. **Fixed**: `push_padded` and `extend_exact` check the running
  total against `cmd_string::MAX_VALUE_BYTES` and report
  `max size for a Tcl value exceeded`, which is tclsh's own message for the same
  input. The *size* is not tclsh's: tclsh 9.0's `Tcl_Size` is 64-bit and
  `format %4294967296d 1` really does build a 4 GiB string there, where tclrs
  refuses above 2 GiB — the size `string repeat` already refuses above. Below
  that the two agree, and no width a script writes is near it.
- **`format`'s integer precision was unbounded too**, which the entry above did
  not name. An integer conversion pads on the left, so
  `format %.9223372036854775807d 1` aborted in the same way and from a different
  line (`cmd_string::integer`). Found by probing the whole conversion table
  against tclsh rather than by the fuzzer. **Fixed** by the same check, and the
  precision now saturates instead of reading as zero when its spelling is too
  long for an `i64`: `format %.99999999999999999999d 1` was `1` and is now
  tclsh's `max size for a Tcl value exceeded`.
- **`expr`'s parser recursion was unbounded.** In an unoptimized build,
  `expr {((((…1…))))}` overflowed the stack between 7_500 and 8_000 parentheses
  on the stack the binary gives it, and a unary chain did the same between
  100_000 and 150_000. **Fixed** by the mechanism `src/parser.rs` already used:
  `expr::MAX_EXPR_DEPTH` (5_000, measured) bounds every descent that opens a
  subexpression — a parenthesized operand, a function argument, both arms of a
  ternary, the right operand of `**`, and a unary operand — and past it the
  answer is `too many nested subexpressions (infinite loop?)`.

  The limit is calibrated against the unoptimized build on purpose: that is the
  weakest one this crate is built as, and it is what `cargo test` runs. An
  optimized build has frames small enough to survive 32_000 parentheses on the
  same stack, so setting the limit from *its* floor would leave a debug build
  aborting where a release build only reported an error.

  Unlike the command parser's `MAX_NESTING_DEPTH`, this limit is **below** what
  the reference interpreter survives, so it is a divergence and a deliberate one:
  tclsh parses expressions with an explicit stack (`tclCompExpr.c`) rather than by
  recursion, and answers 1_000_000 nested parentheses without complaint. Matching
  that would mean an iterative parser *and* an iterative lowering pass *and* an
  iterative drop for the tree, since each recurses on the same nesting. A Tcl
  error for an input no script writes is the trade.

A fourth was not in the list above, because nothing had found it yet:

- **The "followed by junk" diagnostic panicked on a split character.** The
  reference implementation quotes twenty *bytes* of whatever followed a
  close-brace or close-quote where a separator belonged (`TclFindElement`'s
  `while ((p2 < limit) && !TclIsSpaceProc(*p2) && (p2 < p+20))`), and a
  continuation byte is not a space — so the walk runs through a multi-byte
  character and the cap can land inside one. Slicing there is
  `byte index N is not a char boundary`, a panic on the interpreter thread that
  no `catch` sees, and `llength {"a"xxxxxxxxxxxxxxxxxxxé}` was enough to reach
  it. Both copies of the walk had it (`src/list.rs`, `src/assoc.rs`), so
  `llength`, `dict` and `array set` all died on their own version.

  Found by the `vm` cargo-fuzz target, which was not looking for it: a generated
  `dict merge` whose argument the fuzzer had filled with high bytes.
  **Fixed** with one implementation for both callers (`list::junk_prefix`),
  which backs the cap up to the character boundary — dropping the partial
  character, which is what tclsh prints for the same script, measured byte for
  byte.

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
- **tclsh 9.0.4 answers two different things for the same `string replace`,**
  depending on whether it compiled the command. `string replace {} 0 0 X` is `{}`
  through `Tcl_StringObjCmd` — at a script's top level — and `X` through the
  `INST_STR_REPLACE` bytecode, which is what a procedure body or a braced `catch`
  script goes through. tclrs compiles everything, so it gives the compiled
  answer, and agrees with tclsh wherever tclsh agrees with itself. Measured while
  closing the `string replace` abort above; the whole first/last matrix is pinned
  in `tests/parity_fuzz_findings.rs` against the compiled path.
- **The same split for a NaN condition.** `if {"nan"} {puts a}` at a script's top
  level is `domain error: argument not in valid range`, and inside a `catch` body
  or a procedure it is `floating point value is Not a Number`. tclrs gives the
  second everywhere. Pinned as `bug_a_nan_condition_has_two_diagnostics`, which
  asserts both of tclsh's spellings so the disagreement cannot be mistaken for a
  change on this side.
