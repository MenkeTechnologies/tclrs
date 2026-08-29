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
- **Return codes.** Tcl's `ok` / `error` / `return` / `break` / `continue` — and
  any other integer — as one mechanism rather than five special cases. `return
  ?-code c? ?-level n?` raises one; `catch script ?result? ?options?` reports the
  code, the result and a `-code`/`-level` dictionary; a loop absorbs a `break` or
  a `continue` that reaches it, and a procedure call spends one `-level` on the
  way out. So a code crosses every boundary it does in tclsh: out of a script
  `eval`, `uplevel` or `source` ran, and out of a procedure — which is what makes
  `proc stop {} {return -code break}` end the loop that called `stop`, and what
  makes `catch {break}` answer 3 while `while {1} {catch {break}}` does not end
  the loop. A code nothing absorbs is reported at the outermost level by what it
  was: `invoked "break" outside of a loop`. Compiled as a loop region
  (`ext::LOOP_ENTER`) the driver in `src/runtime.rs` resumes at, alongside the
  `catch` regions; the direct jump a `break` in its own loop's body compiles to
  is unchanged, and so is the traced body between them.
- **Procedures.** `proc` and `return`, with a procedure's parameters and locals
  as frame slots rather than entries in the global table (`src/procs.rs`).
  Signatures are collected before anything is emitted, so a procedure may call
  one the script defines further down; defaults and a trailing `args` are
  resolved at the call site.
- **`proc` in any position.** A `proc` inside an `if`, a loop, a command
  substitution or another procedure's body binds its name when the defining code
  *runs*, which is what tclsh does: `if {0} {proc f {} {}}` leaves `f` an
  `invalid command name`, a definition in a taken branch replaces whatever the
  name meant before it, and a procedure that defines another defines it for good
  once it has run. The body is compiled where it stands with the same prologue
  and the same slots; only the binding moved, to `ext::PROC_DEFINE`. Calls to
  such a name go through `ext::DYN_CALL`, which resolves in the interpreter's
  run-time command table and does the argument adapting — defaults, the `args`
  tail, the `wrong # args` wording — that a compile-time call site does for
  itself. Names the compiler *can* resolve keep their direct `Op::Call`: the
  first compilation pass records which names a conditional `proc` defines and
  only those become dynamic, so `bench/counted_loop_proc.tcl` still lowers with
  one `Op::Call` and still reports `traced=true`.
- **A procedure callable from any chunk.** `source`, `eval`, an `after` script and
  a Tk binding script are each a chunk of their own, and a body's entry point is
  an op index that means nothing in another chunk. Every `proc` therefore binds
  its name in the interpreter's run-time table as well as in its own chunk's
  address book, and the table holds the chunk along with the entry point: a call
  from the chunk that owns the body jumps to it, and a call from anywhere else runs
  the body on a VM of its own over the owning chunk, against the same interpreter
  variables. That is what makes `proc f {} {…}` visible inside `eval {f}` and a
  procedure a `source`d file defines visible to the file that sourced it, both
  directions, as they are in tclsh. `rename` moves the run-time entry with the
  registry's, so a name taken away stops answering everywhere.
- **`{*}` argument expansion.** Rule 5 of the dodekalogue. A command with an
  expanded word has no callee and no argument count until it runs, so it is
  lowered whole — the line, then a flag and a value per word, then
  `ext::EXPAND_CALL` — and the flagged words are spliced by list rules when the op
  runs (`crate::procs::expand_call_op`). The name may be expanded too: `{*}{n x}
  y` calls `n` with `x y`. Three kinds of callee, in tclsh's resolution order: a
  procedure of the interpreter; a command this frontend compiles, reached by
  rebuilding the words as a *list* and evaluating it, which is why `set {*}{a b}`
  assigns and `if {*}{1 {puts yes}}` runs its body; and a command Tk registered,
  or nothing, which is `invalid command name`. A command whose words all expand to
  nothing runs nothing and answers the empty string, as tclsh does. Only a command
  that has a `{*}` pays anything. 41 programs against tclsh in
  `tests/expand_differential.rs`.
- **Namespaces.** `namespace` — `eval`, `current`, `qualifiers`, `tail`,
  `parent`, `children`, `exists`, `delete`, `code`, `inscope`, `export`,
  `import`, `forget`, `origin`, `which` and `ensemble exists` / `create` /
  `configure` — plus `variable` and `rename` (`src/cmd_namespace.rs`). A
  namespace is resolved where everything else in this frontend is resolved,
  while compiling: a variable of `::foo` is the interpreter variable `foo::v`, a
  procedure of `::foo` is registered as `foo::p`, and an unqualified name
  written inside `::foo` reaches the namespace's before the root's — the
  two-step search of `TclGetNamespaceForQualName`. A *qualified* name is never a
  procedure's local, which is the same rule read the other way: `TclLookupSimpleVar`
  consults a frame's compiled locals only for a name with no `::` in it. Handing
  one a frame slot instead made `proc f {} {return $::a}` answer the empty string
  for a global the script had set, with nothing to show that it had — tclsh 9.0.4
  answers the value. `namespace qualifiers` and
  `namespace tail` are ports of `NamespaceQualifiersCmd` and `NamespaceTailCmd`
  and are folded when their argument is written out. The queries — `exists`,
  `children`, `which`, `origin`, `parent` — read a registry the interpreter
  holds and the compiled code fills in as it runs. 55 programs are compared
  against tclsh byte for byte in `tests/namespace_differential.rs`.
- **`source` and `tcl_findLibrary`.** `source` reads a file and evaluates it
  against the interpreter that asked for it, through the same chunk cache every
  other script goes through, with `Tcl_PosixError`'s wording for a file it
  cannot read (`src/cmd_source.rs`). `tcl_findLibrary` is a port of the Tcl
  procedure of the same name (`library/auto.tcl:55-218`), so an installed Tk is
  found where tclsh finds one; `crate::cmd_source::seed_library_environment`
  sets the `tcl_library`, `tcl_libPath` and `auto_path` that Tcl's own
  `init.tcl` sets from C state. 15 programs against tclsh in
  `tests/source_differential.rs`.
- **Errors.** `catch` and `error`. A `catch` region is an extension-wide op whose
  payload is its handler's op index; the driver in `src/runtime.rs` unwinds the
  value stack and the call frames to the region's entry state and resumes at the
  handler, so an error raised inside a procedure the guarded script called is
  caught correctly (`src/control.rs`).
- **`subst`.** `subst ?-nobackslashes? ?-nocommands? ?-novariables? string`
  (`src/cmd_subst.rs`, `parser::subst_parts`). The value is read as one word's
  worth of parts running to the end of the input — `ParseTokens` with an empty
  stop mask, so a space, a `;`, a `"` and a `]` are all ordinary text — and each
  option makes its introducer one character of text rather than the start of a
  construct, which is where `subst -nobackslashes {a\[set b]c}` still substitutes
  the command. Nothing is settled while compiling, because nothing can be: the
  string is a value, and `Tcl_NRSubstObj` compiles it at the moment the command
  runs too.

  Two things it would be easy to get quietly wrong, and neither is:

  * **Which frame.** A `$name` inside the value is the *calling* frame's
    variable and a `[cmd]` runs there, so the whole substitution happens inside
    the projection `runtime::in_frame` opens — the same one `uplevel` and an
    `eval` in a body run inside. Running it against the interpreter's globals
    would read and write the wrong variables inside a procedure and never say so.
  * **What a substitution's failure does.** `TclSubstCompile` puts a command
    substitution inside a `catch` range and a plain variable read outside one, so
    `subst {$nosuch}` is an error while `subst {x[break]y}` is `x`,
    `subst {x[continue]y}` is `xy`, and `subst {x[return Q]y}` is `xQy`. A syntax
    error is reported *after* everything before it has substituted and run —
    `subst {[puts hi][}` writes `hi` and then fails — and an unterminated `[`
    still runs the complete commands inside it, which is `TclSubstParse`'s own
    recovery. 74 programs against tclsh in `tests/subst_differential.rs`.
- **`throw`.** `throw type message`, with the type checked to be a list of at
  least one element when the command runs (`Tcl_ThrowObjCmd`). The
  `-errorcode` the type becomes is part of the options dictionary, whose error
  entries are the gap recorded below for `return -errorcode`.
- **Lists.** List parsing and canonical quoting ported from `TclFindElement` and
  `TclScanElement` / `TclConvertElement` (`src/list.rs`), plus `list`,
  `llength`, `lindex`, `lappend`, `lrange`, `lreverse`, `linsert`, `lreplace`,
  `lsearch`, `lsort` (every option, `-command` included — the comparison is
  invoked as a command, so it sees the interpreter's variables and not the
  caller's locals, which is where `Tcl_EvalObjv` leaves it),
  `join`, `split`, `concat`, `lassign`, `lset`, `lpop`,
  `ledit`, `lrepeat`, `lremove`, `lseq` and `lmap` (`src/cmd_list.rs`). `in` and
  `ni` test string membership. Index expressions (`end`, `end±n`, `m±n`) follow
  `Tcl_GetIntForIndex`. `lappend` reaches its variable itself instead of taking
  the value through `GetVar`, so the elements go onto the list's own string and
  growing a list is linear rather than quadratic; a list another variable holds
  is copied instead of extended, which is what keeps that invisible to a script.
- **Associative data.** Array variables (`a(k)`), `array` — `exists`, `get`,
  `names`, `set`, `size`, `unset` — and `dict` — `append`, `create`, `exists`,
  `filter`, `for`, `get`, `getdef`, `getwithdefault`, `incr`, `keys`, `lappend`,
  `map`, `merge`, `remove`, `replace`, `set`, `size`, `unset`, `update`, `values`,
  `with`
  (`src/assoc.rs`). `dict incr` counts a missing key as zero and promotes past
  an `i64` as Tcl's integers do, and it refuses a non-integer with `incr`'s own
  wording rather than an `expr` operand error.
  `dict for`, `dict map` and `dict filter … script` are one walk under three
  endings, emitted by the same `Compiler::rotated_loop` every other loop goes
  through. The walk's state — the flattened pairs, the cursor, and what has been
  collected — rides the VM stack, pushed before the loop and read through the top
  of it, the way `lmap`'s accumulator does: with the cursor in a hidden global
  instead, a `dict for` whose body re-entered the same `dict for` clobbered the
  outer walk's position and the outer loop stopped after one pair with nothing to
  show for it. The three endings are the reference implementation's and they are
  not the same: `dict for` answers the empty string, `dict filter` keeps what it
  collected however the walk ended, and `dict map` throws its accumulation away
  when a `break` ended the walk (`DictMapLoopCallback`,
  `generic/tclDictObj.c:2992-3013`). `dict map` reads the key from the *variable*
  after the body has run, so a body that reassigns `$k` moves the pair, and
  `dict filter` keeps the dictionary's own key and value instead.
  `dict update` and `dict with` share the `finally` region below, and `dict with`
  adds the one thing no other command here needs: variables named by *values*.
  Each key is resolved to a home when the command runs — a global at a script's
  own level, interned past the chunk's name table when the table does not carry
  it, and a frame slot inside a procedure — which is the resolution a computed
  `upvar` target already gets (`crate::cmd_scope::dict_with_home`). A key written
  `a(i)` is one element of an array, because `Tcl_ObjSetVar2(keyPtr, NULL, …)`
  parses it that way (`generic/tclDictObj.c:3810`). The write-back puts back the
  keys the *binding* recorded rather than whatever the dictionary holds at the
  end, so a body that empties the dictionary or deletes one of those keys does
  not lose them, and only unsetting a bound variable removes one — all three
  measured against tclsh 9.0.4. A missing dictionary variable and a path that
  stopped leading anywhere both drop the write-back silently, as
  `TclDictWithFinish` does (`:3875-3877`, `:3912-3917`). The one case it does not
  cover is the entry below.
  An array works inside a procedure as well as at the top level: every one of its
  ops takes the variable's place — a name index in the global table, or a frame
  slot written as `-(slot + 1)` — so a local array belongs to its activation and
  two frames of a recursive procedure do not share one.
- **Strings.** `format` and the `string` ensemble — `cat`, `compare`, `equal`,
  `first`, `last`, `index`, `insert`, `is`, `length`, `map`, `match`, `range`,
  `repeat`, `replace`, `reverse`, `tolower`, `totitle`, `toupper`, `trim`,
  `trimleft`, `trimright` (`src/cmd_string.rs`). `string is`'s character classes
  read Unicode general categories — Tcl's own `ALPHA_BITS` / `PUNCT_BITS` /
  `GRAPH_BITS` unions from `tclUtf.c`, not the derived properties Rust's std
  exposes — so `graph`, `print` and `punct` are answered rather than refused, and
  every class answers beyond ASCII. `-failindex` writes the index of the first
  character that failed, which is one rule for every class: the length of the
  longest prefix that still belongs to it. `append` reaches its variable
  itself instead of taking the value through `GetVar`, so the values go onto the
  string the variable already holds and growing a string is linear rather than
  quadratic; `set x "$x…"` is lowered as the same op when the word only grows
  `x` and nothing after the leading `$x` can run a script, which is the case
  where the two would read the variable at different times. A string another
  value holds is copied instead of extended.
- **Regular expressions.** `regexp` and `regsub` with `-nocase`, `-all`,
  `-inline`, `-indices`, `-line`, `-lineanchor`, `-linestop`, `-expanded`,
  `-start`, `-command` and `--`, plus `switch -regexp` — with `-matchvar` and
  `-indexvar` — `lsearch -regexp` and `array names -regexp`
  (`src/regexp.rs`). Match variables, `regsub`'s `&` / `\0` / `\1`…`\9`
  replacement, and Tcl's character — not byte — indices.

  `regsub -command` invokes the third word as a *command prefix* rather than
  expanding it as a template: the whole match and every subexpression are
  appended to it and `Tcl_EvalObjv` runs the lot, once per match, and its
  result is the replacement verbatim. `switch`'s two variables are filled from
  the same capture information, and `-indexvar` follows `Tcl_SwitchObjCmd`'s
  own rule for an empty match rather than `regexp -indices`'s — the two really
  do differ, and `switch -indexvar i -regexp abc {{} {}}` is `-1 -1` where
  `regexp -indices -inline {} abc` is `0 -1`.

  The engine underneath is the `regex` crate, not Henry Spencer's ARE, and the
  two are not the same language. Three differences are corrected in the
  translation and pinned by `tests/regexp_differential.rs` against tclsh: `.`
  matches a newline in ARE and not in Rust, so every pattern is prefixed
  `(?s)`; `-line` is `-lineanchor` *and* `-linestop`, so it moves both the
  anchors and what `.` will cross; and the empty-match loop is Tcl's, where
  `regexp -all {x*} ab` counts 2 but `regsub -all {x*} ab -` substitutes 3
  times, and the literally empty pattern — not `(?:)` or `a{0}` — stops where
  `regexp` stops.

  Four ARE constructs are **refused by name** rather than approximated, because
  a finite-automaton matcher cannot express them at any price: back-references
  (`(a+)\1`), look-ahead (`(?= )` and `(?! )`), the word-start and word-end
  boundaries `\m` and `\M`, and collating elements and equivalence classes
  (`[[. .]]`, `[[= =]]`). tclsh matches all of them. Look-*behind* is not on the
  list because ARE has none either — tclsh answers `invalid quantifier operand`
  for `(?<=a)b`. The refusal names the construct and is raised where the pattern
  is used, so a script can catch it.

  A pattern neither the translation nor `regex` accepts reports
  `cannot compile regular expression pattern: …`, and the detail is translated
  back to `regcomp`'s own `REG_*` wording (`generic/regex/regerrs.h`): the two
  engines detect the same defects but name them differently — the interpreter
  names the construct, `regex` names the parse state it was in — so `(a` is
  `parentheses () not balanced` here as it is there, rather than
  `unclosed group`. Two grammar rules ARE has and `regex` does not are enforced
  in the translation for the same reason, because without them the engine
  answers where tclsh refuses: a `{` that does not begin a bound is an ordinary
  character (`regexp {a{} "a{"` is 1, and `a{ 2}` is *not* two `a`s), and an
  atom takes one quantifier plus an optional `?` meaning non-greedy, so `a**`,
  `a?*`, `a{2}{3}` and `a*??` are all `invalid quantifier operand`.

  **Not closed**: three patterns are still classified differently, because the
  two engines detect them at different points rather than wording them
  differently — `(?` is `invalid quantifier operand` in tclsh and `parentheses
  () not balanced` here, `[a-\` is `brackets [] not balanced` there and
  `invalid character range` here, and `[[:bogus:]]` is `invalid character
  class` there and compiles here.
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

  **A boolean word is an operand**, not a bare word: `expr {yes}` is `yes`,
  `expr {true && false}` is 0, `expr {!yes}` is 0, `expr {on ? 1 : 2}` is 1, and
  `expr {yes + 1}` is refused by the numeric rule with tclsh's own wording rather
  than by the parser. The word carries its own spelling, so it behaves as the
  quoted form does. The table is `ParseBoolean`'s, shared with the condition rule
  rather than copied — which is why `o` is still a bare word, `on` and `off`
  both starting with it — and it is why `expr {1 y}` is `missing operator at _@_`
  where `expr {1 x}` names `x`.

  **A refusal carries the context tclsh gives it**: the message, then
  `in expression "…"` quoting the source verbatim with `_@_` at the parse
  position, then for a bare word the `should be "$w" or "{w}" or "w(...)" or …`
  hint. Those three lines are what `catch` yields, so a script that prints a
  caught message now sees what tclsh's prints; the marker appears only on
  diagnostics whose own text ends in `at _@_`, and the `;` after the quote only
  when the hint follows. Measured line by line against tclsh 9.0.4 in
  `tests/expr_diagnostics_differential.rs`, including the function-call parens:
  an unterminated argument list is the open paren, a promised argument that never
  arrives is `missing function argument at _@_`, and a second operand inside the
  list is the missing operator between them.
- **Arbitrary-precision integers.** Tcl 9's integers are unbounded and these
  are too: an operation that leaves `i64` promotes rather than wrapping or
  refusing, so `expr {9223372036854775807 + 1}` is `9223372036854775808`,
  `2 ** 100` is exact, `1 << 200` grows, and `i64::MIN / -1` answers instead of
  reporting an overflow. A spelling wider than `i64` is a value in every radix,
  and keeps the text it was written with where `eq` can see it.

  The fast path is untouched, which is the point of the shape: fusevm computes
  on `i64` in registers and calls the frontend's `NumericHook` only when its
  *checked* arithmetic overflows, so a loop that never leaves the word never
  builds a `BigInt`. A promoted value travels as its canonical decimal string —
  Tcl's own model, and one that needs no new `fusevm::Value` variant — and comes
  back down to `Value::Int` the moment a result fits again. Ordering an integer
  against a double is exact at every width rather than through a double, which
  is observable twice over: `expr {99999999999999999999 < 1e20}` is 1 while
  `== 1e20` is 0, and `expr {3**34 == double(3**34)}` is 0 with `>` 1 even
  though `3**34` fits an `i64` — past 2^53 a machine integer rounds on the way
  to a double exactly as a bignum does, so width is not the test. Tcl's
  *arithmetic* on that same pair does promote (`expr {3**34 - double(3**34)}`
  is `0.0`, not `1`), and only the comparison is exact (`runtime::big_cmp`,
  `runtime::numeric`, `tests/bignum_differential.rs`).

  Two bounds are this frontend's own. A promoted integer may reach 2^20 bits,
  a little over 315,000 digits, and a wider one is refused: tclsh has no bound
  and `expr {10 ** 123456789}` sits there computing — measured, still running
  after 30 seconds — where a catchable error is the better answer, the same
  trade `expr::MAX_EXPR_DEPTH` already makes. And `format`'s integer
  conversions still refuse a value past `i64` rather than narrowing to one:
  tclsh answers `format %d 99999999999999999999` with `1661992959`, its low 32
  bits, and narrowing silently is the one thing this frontend will not do.
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
- **Channels.** The generic layer of `generic/tclIO.c` — the name table, the
  reference count, the encoding, the end-of-line translation, the buffering
  mode and the end-of-file rule — with one driver per channel
  (`src/cmd_channel.rs`). `open`, `close`, `gets`, `read`, `puts` to a channel,
  `flush`, `eof`, `seek`, `tell` and `fconfigure`, plus `stdin`, `stdout` and
  `stderr`. A file channel's name is `file` followed by its descriptor number,
  as `unix/tclUnixChan.c:1845` builds it. `-translation` takes `auto`, `binary`,
  `cr`, `lf`, `crlf` and `platform` on each side independently; `-encoding`
  takes every name `encoding names` lists, through the tables in
  `src/cmd_encoding.rs`, holding an incomplete multi-byte sequence until the
  rest of it arrives; `-buffering` takes `full`, `line` and `none`.
  The C side is `src/tk/channel.rs`: thirty-seven `TclStubs` slots including
  `Tcl_CreateChannel`, which takes a `Tcl_ChannelType` — Tk's own table of
  driver procs — and calls into it thereafter.
- **Transcoding.** The `encoding` ensemble, whole: `convertfrom`, `convertto`,
  `dirs`, `names`, `profiles`, `system`, `user`, with `-profile` (`tcl8`,
  `strict`, `replace` — `strict` is the default, as tclsh 9.0.4's is) and
  `-failindex` (`src/cmd_encoding.rs`). None of the tables is typed here: each is
  a byte-for-byte copy of a `library/encoding/*.enc` file from the
  checksum-verified source release, vendored into `src/encodings/` by
  `scripts/gen_encoding_tables.py`, and read by a port of `LoadTableEncoding`
  (`generic/tclEncoding.c`) — so `diff -r src/encodings
  conformance/vendor/tcl*/library/encoding` is the whole provenance check, and
  there is no new dependency. `TableToUtfProc` and `TableFromUtfProc` are ported
  with them, which is what makes the double- and multi-byte CJK encodings
  (`big5`, `cp932`, `cp936`, `cp949`, `cp950`, the `euc-*` set, `gb2312`,
  `gb12345`, `jis0208`, `jis0212`, `ksc5601`, `macJapan`, `shiftjis`,
  `cns11643`) work through the same prefix-byte machinery rather than an
  approximation of it, along with the symbol-font page rule and the trailing
  reverse-mapping section four of the Japanese tables carry. `utf-8`, `cesu-8`,
  the `utf-16`/`unicode` family, `ucs-2` and `utf-32` are ports of
  `UtfToUtfProc`, `Utf16ToUtfProc`, `UtfToUtf16Proc`, `UtfToUcs2Proc`,
  `Utf32ToUtfProc` and `UtfToUtf32Proc`. `tests/encoding_differential.rs` puts
  every single byte through every encoding under every profile, and every
  two-byte sequence through the encodings where the second byte decides the
  character, and compares with tclsh line for line.
- **The rest of the toolchain.** `--disasm`, `--dump-tokens` and `--dump-ast`
  print the bytecode, the lexical output and the parse tree; the zsh completion
  is `completions/_tclrs`; the manual pages are `man/man1/tclrs.1` and the
  all-in-one `man/man1/tclrsall.1`; and `docs/reference.html` is generated from
  the compiler's own tables by `cargo run --bin gen-docs` — every command, every
  ensemble subcommand with the compiler's own answer for whether it is
  implemented, the `expr` ladder as the parser binds it, and the `format`
  conversions the runtime answers to.
- **The event loop.** `after ms`, `after ms script`, `after idle script`,
  `after cancel` (by id and by script text), `after info`, `update`,
  `update idletasks` and `vwait` (`src/cmd_after.rs`). The registry of pending
  scripts is per interpreter, as Tcl's is (`Tcl_SetAssocData(interp,
  "tclAfter", …)`), so handles are numbered from `after#0` per interpreter. One
  timer event runs every handler already due, in deadline order, and not the
  ones registered while it runs — the generation rule of
  `TimerHandlerEventProc` (`generic/tclTimer.c:606-694`). A default build's only
  event sources are those scripts; a `--features tk` build also pumps the
  ported notifier, where Tk's window and file events arrive.
- **Reaching another scope.** `uplevel`, `upvar`, `variable` and `apply`
  (`src/cmd_scope.rs`). Both commands resolve their level when they run, against
  the same count `info level` answers with, and a target that is a *procedure
  activation* is served through the per-procedure slot-name table the compiler
  now publishes: `chunk.ops[frame.return_ip - 1]` attributes a live frame to a
  body, and the table says which name each of that body's slots was written as.
  A script running against a frame sees the frame's variables and nothing else,
  which is what makes a bare read of an undeclared global refuse there exactly as
  it refuses in the body — while a `::`-qualified name still names the
  *interpreter's* variable, in both directions. The two are told apart by the
  spelling the chunk keeps (`cmd_namespace::chunk_key`), which is a name of its
  own only in a script lowered for a projection (`Compiler::projected`), because
  everywhere else `::g` and a bare `g` are one variable and must share one name.
  Every procedure activation is projected, including one whose body declares no
  local of its own: a name its script assigns is a local of that activation even
  when a global already wears the name.

  `upvar` at any level, with a computed level, a computed name, or an array
  element as its target all work — and an element target *creates* the array in
  the frame it points into, before anything is written through the link and
  whether or not anything ever is, as `TclObjLookupVar`'s `createPart1` does;
  `upvar #0 other local` written out is still a
  compile-time binding through `Compiler::var_place`, so the common case costs
  the body nothing. A name a body itself bound with `upvar` is published among
  that body's slot names (`Compiler::publish_slot_names`), so a *second* `upvar`
  through it, a `dict with` key naming it and a computed `set $n` all find it —
  and each follows the descriptor in the slot rather than reading it, which is
  what makes `upvar 1 y q` in a body whose caller wrote `upvar 1 $v y` reach the
  caller's caller's variable. `upvar` outside a procedure makes two globals one variable,
  in the interpreter's own table (`runtime::alias_global`). `apply` of a lambda
  written out is compiled as an anonymous procedure, with its own frame slots
  and entered by `Op::Call`, so it costs a call and nothing else.
- **`info`.** `args`, `body`, `commands`, `complete`, `coroutine`, `default`,
  `exists`, `globals`, `hostname`, `level`, `locals`, `nameofexecutable`,
  `patchlevel`, `procs`, `script`, `tclversion` and `vars`
  (`src/cmd_info.rs`). Most are answered while compiling; `exists`, the name
  lists and `level` are ops, because each is a question about the running
  program.

## Not implemented

- **`string is graph`, `print` and `punct`, beyond nothing.** All three rest on
  Unicode general categories Tcl builds its own tables for: `punct` spans the
  seven punctuation categories *and* the four symbol ones, and `graph` and
  `print` are defined from that set. They are refused rather than answered from
  Rust's tables, which track a different Unicode revision — the same rule the
  rest of `string is` follows beyond ASCII. `string is dict` used to be refused
  alongside them by mistake; it is structural, not a character class, and now
  answers.
- **`string wordend` and `string wordstart` beyond ASCII.** A word is a run of
  letters, decimal digits and connector punctuation, which is three general
  categories, so the two subcommands answer for ASCII and refuse past it exactly
  as `string is wordchar` does. Measured against tclsh: `a²b` is three words
  because U+00B2 is `No` and not `Nd`, and `a‿b` is one because U+203F is `Pc` —
  neither is derivable from what Rust's standard library exposes.
- **Command pipelines and sockets.** `open |command` is refused rather than
  read as a file whose name begins with a pipe, and there is no socket driver,
  so `Tcl_OpenCommandChannel` and `Tcl_MakeTcpClientChannel` have no body.
- **Stacked channels.** `Tcl_StackChannel` (`generic/tclIO.c:1796`) puts one
  driver on top of another and is what `zlib push` and `tls` are built from;
  every `topChanPtr` / `bottomChanPtr` hop in `tclIO.c` exists for it. There is
  one driver per channel here, and the four stacking slots are traps.
- **Non-blocking channels.** `fconfigure -blocking 0` is refused rather than
  accepted and ignored: a channel that reports itself non-blocking and then
  blocks is worse than one that says it cannot. Background flushing and the
  `BG_FLUSH_SCHEDULED` machinery go with it.
- **`fconfigure -eofchar` and `-profile`,** which are reported at their defaults
  and refused when set. `-profile strict` is the default a channel reports and
  the one the encodings in `src/cmd_encoding.rs` are used at, so a byte sequence
  a channel cannot decode is an error rather than a substitution; the `utf-8` and
  `iso8859-1` arms of `src/cmd_channel.rs` predate that and still substitute.
- **The escape-sequence encodings `iso2022`, `iso2022-jp` and `iso2022-kr`.**
  These are not tables but state machines, with a second `.enc` file format of
  their own (`LoadEscapeEncoding`) and conversion procs to match. They are
  refused by name and are absent from `encoding names`, so a script can see
  before it converts that they are not there — an approximation of a stateful
  encoding is the kind of wrong answer a test suite does not reach.
- **A decode whose result would be an unpaired surrogate.** Only `-profile tcl8`
  produces one: `encoding convertfrom -profile tcl8 utf-8 \xED\xA0\x80` is
  U+D800 in tclsh. A `String` in this frontend cannot hold a surrogate, so the
  conversion stops with a message naming the code point rather than substituting
  something that would look like success. The same input under `strict` and
  `replace` is exact, since neither profile can produce one.
- **A non-literal option name in `encoding convertfrom` / `convertto`.** Which
  argument is an option, which is its value and which two are the encoding and
  the data is decided by the argument *count*, which is known while compiling;
  *which* option a word names is not, so `encoding convertfrom $opt tcl8 utf-8 x`
  is refused where a literal is required. The option's *value*, the encoding and
  the data may all be computed.
- **Half-closing a read-write channel.** `close $chan read` on a channel with
  both sides open needs a driver whose `close2Proc` honours `TCL_CLOSE_READ`
  (`generic/tcl.h:1369-1370`), and no device here has one — tclsh's own file
  driver has not either. `close $chan read` on a channel that only has a read
  side is a plain close, as it is in tclsh.
- **The POSIX list form of an access mode.** `open $f {WRONLY CREAT TRUNC}`
  (`generic/tclIOUtil.c:1540-1600`) is refused by name; the `r`/`r+`/`w`/`w+`/
  `a`/`a+` strings are implemented.
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
- **`yield` inside a script run by `eval`, `uplevel` or `apply`.** tclsh
  suspends the coroutine from inside the nested script and resumes into the
  middle of it. Here the nested script runs a machine of its own, several Rust
  frames below the VM that would have to park, and that VM saves only its own
  state — the nested machine is not part of it — so a resumption could not come
  back to where the script left off. Refused, in those words, rather than
  approximated: every approximation loses whatever the nested script had set, at
  a yield, silently. A `yield` that is in no coroutine at all still reports the
  reference interpreter's `yield can only be called in a coroutine`, and an
  `eval` inside a coroutine that does not yield is unaffected
  (`tests/frame_differential.rs`).
- **Return options beyond `-code` and `-level`.** The return-code system itself
  is implemented — see the entry in "Implemented" — but `return -errorcode`,
  `-errorinfo` and `-options` are refused, and `catch`'s options variable
  carries only the two options this frontend models. tclsh's dictionary for an
  *error* also has `-errorstack`, `-errorcode`, `-errorinfo` and `-errorline`,
  so `catch {error boom} m o` gives a shorter dictionary here; the `-code` and
  `-level` in it are exact, and `tests/proc_differential.rs` compares the whole
  dictionary for every outcome whose tclsh form has nothing else in it. The
  three commands that *set* one of the missing options — `throw`'s type word,
  and `error`'s `errorInfo` and `errorCode` arguments — take the word, evaluate
  it and drop it rather than refusing the command, because the message and the
  code they carry are right either way and the option they set is missing
  visibly: asking the dictionary for it fails. `::errorInfo` and `::errorCode`
  are not set either, and reading one is `no such variable`.
- **Procedures across an `eval`.** An evaluated script shares the interpreter's
  variables but not its procedures: it is a chunk of its own, and a call site
  resolves its command against that chunk. So `eval {proc twice {x} {…}}`
  followed by `twice 21` is `invalid command name "twice"`, and so is
  `eval {twice 21}` for a procedure the outer script defined — both run in
  tclsh. The run-time command table a conditional `proc` binds into *is* shared
  across evaluations, and this is the one thing it deliberately will not do: an
  entry records the chunk its body entry point indexes into (`op_hash` and op
  count), and a lookup from another chunk misses rather than jumping to whatever
  op sits at that index. `eval {if {1} {proc f {} {…}}}` followed by `f` is
  therefore `invalid command name "f"` as well. Carrying the callee's *chunk*
  through the call — not just its entry — is the fix.
- **`namespace path`, `namespace unknown` and `namespace upvar`.** All three
  change how a name resolves *after* the point this frontend resolved it, so
  honouring them would mean re-resolving names at run time. Refused where they
  are written.
- **A computed `namespace eval` name or body.** `namespace eval $n {…}` and
  `namespace eval foo $body` are refused: the namespace decides which variable
  every `$v` in the body reads and which procedure every call reaches, and both
  are decided while compiling. The same rule refuses a computed `namespace
  import` pattern.
- **Dispatching through an ensemble.** `namespace ensemble create` records that
  the namespace is one, so `namespace ensemble exists` and `configure` answer,
  but calling the ensemble command resolves a subcommand when it runs and this
  frontend resolves a call while compiling. The call is `invalid command name`
  rather than a guess.
- **Procedures across a `source`, as across an `eval`.** A sourced file's
  variables — including its namespace variables — are the interpreter's, and
  survive; its procedures are its own chunk's and do not, for exactly the reason
  the `eval` entry above gives. So `source` of a library file that defines
  procedures leaves them unreachable from the script that sourced it. The same
  runtime command table fixes both. This is why `tcl_findLibrary tk … tk.tcl`
  finds and reads the real `tk.tcl` but the procedures it defines are not yet
  callable.
- **`return` at the top level of a sourced file.** `return` is refused outside a
  procedure body, so a library file that ends early with a bare `return` — a
  common shape — fails rather than answering. This is `return`'s own gap, not
  `source`'s.
- **`coroprobe` and `coroinject`.** Inspecting or injecting a command into a
  suspended coroutine is not implemented; both are `invalid command name`.
  Deleting a coroutine by destroying its command is not either: `rename` is
  implemented now, but a coroutine's command is not in the registry `rename`
  operates on, so `rename c {}` for a coroutine `c` is `can't rename "c":
  command doesn't exist`. A coroutine goes away when its body ends.
- **Coroutines of anything but a procedure of the script.** `coroutine`'s name
  and command are literals, its command is one of the script's own procedures,
  and the command appears at the top level of a script or in a command
  substitution in one, because the name has to be known to every call site and
  the body is entered through the chunk's sub table. `yieldto` at a command that
  is not a coroutine of the script is refused: it would have to evaluate that
  command in the resumer's context, which this frontend cannot do.
- **Coroutines of a procedure a conditional `proc` defines.** `coroutine c gen
  3` needs `gen`'s signature while the `coroutine` command is lowered — the
  actual arguments are arranged by the call site — and needs a sub-table entry
  to position the fresh VM at. A `proc` away from the top level supplies
  neither: its signature is carried as text for run time, and it registers no
  sub entry, because two conditional definitions may share a name and
  `Chunk::find_sub` answers with whichever was registered first. So `if {1}
  {proc gen {n} {…}}` followed by `coroutine c gen 3` is `invalid command name
  "gen"`, raised when the `coroutine` command runs. It works in tclsh. The two
  halves have the same fix: a coroutine entered through the run-time command
  table rather than through the sub table.
- **A coroutine of a top-level procedure some conditional `proc` also
  redefines.** The coroutine is positioned at the *top-level* body, since that
  is the one with a sub-table entry, even when a conditional definition of the
  same name ran later and every ordinary call site would reach the newer one.
- **The `info` subcommands that name machinery this frontend has none of.**
  `frame`, `errorstack` and `cmdcount` need a record of each active *command*,
  where only the stack of call frames is kept; `class`, `object`, `consts`,
  `constant` and `cmdtype` need an object system. Each is `info frame is not
  supported yet` rather than mis-answered. `info level N` is refused separately:
  the *value* of a level is the command and arguments that entered it, and
  `Op::Call` pushes the actual arguments and nothing that names the command.
  `info level` with no argument is exact, and so are `body`, `locals`, `vars`
  inside a body, and `functions` — the last read out of `expr_math`'s own table,
  so the list cannot fall behind what `expr` accepts.

  `info loaded` is refused too, `load` not being implemented, so the list would
  be empty by construction rather than by observation.
- **`info library`, and the `auto_path` that follows from it.** tclsh has a
  script library and `init.tcl` sets `auto_path` and defines the `auto_*`
  procedures from it. tclrs has no script library, so `info library` raises `no
  library has been specified for Tcl` — which is tclsh's own message for the
  state tclrs is permanently in — `auto_path` does not exist, and `info procs`
  does not list the `auto_*` procedures that a bare `info procs` in tclsh does.
  `info globals` likewise omits the `tcl_*` variables `init.tcl` sets.
- **`info args`, `info body` and `info procs` answer for a procedure declared
  later in the script.** A whole script is compiled before any of it runs, and
  the signature table is filled on that pass, so `info args later` before `proc
  later {q r} {}` returns `q r` where tclsh raises `"later" isn't a procedure`.
  The same ordering is what lets a procedure call one defined below it, which
  tclsh also allows; only the introspection disagrees.
- **`info body` of a procedure whose body the script computed.** `proc p {} $b`
  records a signature and no text, so `info body p` answers `"p" isn't a
  procedure` — the same thing it answers for a name that is no procedure at all,
  because from the table's side the two are the same absence. A `proc` inside a
  `namespace eval` block is in the same position: its signature is prescanned
  and its body text is not.
- **An array element as the variable `dict incr` names.** `set a(1) x` followed
  by `dict incr a(1) k` is `array element is not supported yet`.
- **An `upvar` to a variable the procedure running there never names.** An
  `upvar` link is the *address* of one frame slot, and another frame's slots are
  addressed through a table of the names its procedure *wrote*
  (`src/cmd_scope.rs`), so `upvar 1 neverused z` has nowhere to point: no op in
  the already-built body could address a slot for it. tclsh creates it; here it
  is refused by name.

  `uplevel 1 {set neverused 1}` was refused with it and is not any more.
  `uplevel` does not address a slot: it projects the whole frame into the
  interpreter's variables, runs the script, and reads the named slots back
  (`runtime::run_in_frame`), so a name with no slot is simply not read back —
  which is what tclsh's own answer amounts to from the caller's side. Measured
  against tclsh 9.0.4 in `tests/event_differential.rs`.
- **An `upvar` outside a procedure whose names the script computes is seen at the
  next chunk boundary.** The pair is registered on the interpreter
  (`runtime::alias_global`) and `seed`/`write_back` resolve through it, so every
  later script sees one variable — which is what `uplevel #0 [list upvar #0
  ::tk::Priv.$disp ::tk::Priv]` (`library/tk.tcl:257`) needs. What it is not is
  coherent *inside* a chunk that already holds two projections of the two names:
  a chunk's variables are a slot vector taken on entry, and two entries of it
  cannot become one slot afterwards. Written out rather than computed, the pair is
  a compile-time binding (`Compiler::top_aliases`) and is coherent everywhere.
- **Every command outside those above.** `interp`, `socket`, `exec`, `trace`,
  `try`, … An unknown command name is `invalid command name
  "…"`, raised when the command runs — `puts [catch {nosuchcmd} m]` is `1` —
  because the compiler lowers that refusal as code rather than deciding it (see
  `Compiler::defer`).
- **An expanded command that assigns, inside a procedure body.** `{*}` itself is
  implemented (see the entry under "Implemented"), and a command it expands into
  that this compiler owns is reached by evaluating the words as a list — which is
  a chunk of its own, and a chunk addresses a procedure's locals as frame slots
  it cannot share. So `set {*}{a b}` inside a procedure body writes the *global*
  `a` where tclsh writes the local one. The words are already values by the time
  this happens, so only a command that names a variable can reach the difference:
  `set`, `incr`, `append`, `lappend`, `unset` and `upvar` written with an
  expansion, inside a procedure, and only for the variable they name. A procedure
  or a Tk command called with `{*}` — every use in `tk.tcl` — is entered directly
  and is unaffected. The fix is the same one `eval` inside a procedure needs: a
  variable table addressable by name at any level, which is the trade recorded at
  the end of this file.
- **A coroutine created or resumed with `{*}`.** A coroutine lives on the
  evaluation that created it — its context command is in that driver's table, not
  in the interpreter's — so both halves miss when the command is expanded.
  `coroutine {*}{c gen 3}` is `invalid command name "gen"`, because the command is
  reached by evaluating the rebuilt words and the procedure's entry point is in
  another chunk than the one the coroutine's VM would run; resuming with `{*}{c}`
  finds no procedure and no Tk command. Written out, both are compiled and work.
- **A cross-chunk call costs native stack, and is counted against the recursion
  limit.** A procedure reached from a chunk other than its own runs on a VM of its
  own, which is a nested evaluation like `eval`'s and spends the same kind of
  stack: measured on a 2 MB stack in a debug build, a mutually recursive pair
  split across two chunks survives 8 levels and not 12, where a chain of nested
  `eval`s survives 16 and not 24. Past `DEFAULT_RECURSION_LIMIT` it is `too many
  nested evaluations (infinite loop?)`, which is what tclsh answers for its own
  limit (measured), and the binary runs on `RECOMMENDED_STACK` so the limit is
  what stops it rather than the stack. A host embedding the library on a small
  stack should lower the limit, as it already should for `eval`.
- **An array element as the variable a `dict set` names.** `dict set a(1) k v` is
  `array element is not supported yet`. The list commands took this refusal until
  `Compiler::elem_store` landed: `lappend a(x) v`, `append a(x) v`,
  `lassign {1 2} a(x) a(y)`, `lset a(x) 0 v`, `lpop a(x)`, `ledit a(x) 0 0 v`,
  `foreach a(x) … ` and `lmap a(x) …` all take one now, and are byte-compared
  against tclsh in `tests/event_differential.rs`.
- **Code points tclsh 9.0.4 categorises and Unicode 16.0 does not.** The
  reference interpreter's character tables are ahead of the ones this build
  carries. Sweeping `string is` over every code point in both engines puts the
  difference at 4804: 4803 that tclsh assigns a category and Unicode 16.0 calls
  unassigned — corroborated against Python's `unicodedata` at 16.0.0 — and
  U+0295, which Unicode 16.0 calls `Ll` while tclsh answers 0 for `string is
  lower`, so its table must call it `Lo`. Everywhere else the two agree exactly:
  12,184,664 answers checked, none wrong. A class asked about one of the 4804
  refuses and names it rather than answering from a table that does not know it.
  The list is `BEYOND_UNICODE_16` in `src/cmd_string.rs`; regenerate it when the
  crate's Unicode version catches up, at which point it should be empty.
- **Subcommands and options recognised and then refused.** `array startsearch`
  and the other search subcommands; `dict info`;
  `dict set`, `dict incr`, `dict update` or `dict with` into an array element;
  `string`
  subcommands outside the
  implemented set; `format` conversions outside the
  implemented set; `regexp -about`;
  `return`'s options other than
  `-code` and `-level`. They go through the reference option parser first,
  so abbreviation and ambiguity behave as tclsh does, and are then refused.
  `lsort -command`, `dict map` and `dict filter … script` were on this list until
  the change that added `subst`, `dict update` until the change that built the
  `finally` region, and `dict with` until the change that resolved a key to a
  home when the command runs; what `dict info` waits on is named below.
- **`dict info`.** The answer is `Tcl_HashStats` (`generic/tclHash.c:602`) on the
  dictionary's own hash table: a bucket count, the distribution of chain lengths,
  and an average search distance.

  The *algorithm* is reproducible, and was reproduced to check. A dict's table is
  `TCL_CUSTOM_PTR_KEYS` over `TclHashObjKey` (`generic/tclObj.c:4245`), which is
  `result += (result << 3) + byte` over the key's string form; the type carries no
  `TCL_HASH_KEY_RANDOMIZE_HASH`, so the bucket is `hash & mask`; the table starts
  at 4 buckets with `rebuildSize` 12 and both multiply by 4 whenever
  `numEntries >= rebuildSize` (`tclHash.c:167-170`, `:357`, `:983-999`). A
  transcription of that reproduced tclsh 9.0.4's whole 12-line answer byte for
  byte on seven dictionaries, including a 200-key one that rebuilds twice.

  What is not reproducible is which table. `Tcl_HashStats` reports the table the
  `Tcl_Obj` *has*, and that depends on the object's history, not on its value —
  a dict grown and then shrunk in place keeps the buckets it grew:

      set d {}
      for {set i 0} {$i<20} {incr i} {dict set d k$i $i}
      for {set i 0} {$i<18} {incr i} {dict unset d k$i}
      dict info $d          →  2 entries in table, 16 buckets
      dict info {k18 18 k19 19}  →  2 entries in table, 4 buckets

  Both dictionaries are `k18 18 k19 19`, and `string equal` between them is 1 —
  re-measured against tclsh 9.0.4 rather than taken from the earlier reading. A
  dict here is its string and nothing
  else, so there is no history to consult and no way to tell the two apart — the
  answer would be exact for every dictionary built by inserting its own pairs
  (which is `dict create`, a literal, `dict get`, `dict merge`, `dict replace`
  and `dict remove`, all measured) and quietly wrong for one that shrank, with
  nothing marking which. A command whose entire purpose is to report a container's
  real internal state is the last one that should sometimes report a plausible
  one, so it stays refused.

  **What it would take is not a fusevm change.** This entry used to say a dict
  with an identity means a new `fusevm::Value` variant, and that is wrong:
  `Value::Obj(u32)` is already there, is already identity-comparable, and is
  already how several sibling frontends carry a heap object the VM never looks
  inside. Nothing in the interpreter, the Cranelift JIT or the AOT lowerer reads
  an `Obj`, and none of the ops this frontend emits stringify one behind its
  back, so a dict could be a handle into a heap this crate owns without any other
  frontend paying for it. (Two costs would be local and real: a builtin's
  `dispatch` answers a `String` today and would have to be able to answer a
  handle, and a handle in `chunk.constants` would be a dangling index in the next
  process — the AOT and JIT disk caches serialise `Value` — so a dict must never
  become a compile-time constant, or must carry a heap image the way elisprs
  does.)

  **What it really needs is `Tcl_IsShared`.** The bucket count is not a function
  of the value *or* of the object's history alone: it is a function of the
  object's history and of whether each write found the object shared. Measured
  against tclsh 9.0.4, over the 16-bucket dictionary above:

      set e $d;    dict info $e          →  2 entries, 16 buckets   (same object)
      dict set e zz 1; dict unset e zz   →  2 entries,  4 buckets   (shared: duplicated)
      dict info $d                       →  2 entries, 16 buckets   (untouched)
      dict set d zz 1; dict unset d zz   →  2 entries, 16 buckets   (unshared: in place)
      set L [list $d]                    →  (a list now holds it)
      dict set d q 1;  dict unset d q    →  2 entries,  4 buckets   (shared by the list)

  `DupDictInternalRep` builds the copy's table by inserting its entries, which is
  why a duplicate reports the natural count. A handle heap gives the identity
  those lines need but not the sharing: the last pair is a reference held by a
  *list*, and a list here is a `String` (`crate::list::split` / `join`), so the
  handle is not in it and nothing has counted. The same goes for an array element
  and for a dict nested in another dict. Answering `dict info` exactly therefore
  needs tclrs's own aggregate types to hold `Value`s rather than strings — its
  representation change, not fusevm's — and a copy-on-write rule keyed on that
  count, which is `Tcl_Obj` in full. Until then a handle heap would move the
  wrongness rather than remove it, so the refusal stands for the same reason it
  always did.
- **`regexp -about`.** The group count is easy; the second element is not. It is
  the reference engine's own compile-time telemetry — `REG_UUNPORT`,
  `REG_UNONPOSIX`, `REG_ULOCALE`, `REG_UEMPTYMATCH`, `REG_UBOUNDS` and the rest
  (`generic/tclRegexp.c:644-659`) — set from inside `regcomp.c` as it builds an
  NFA: `regexp -about {a*}` is `0 REG_UEMPTYMATCH` and `regexp -about {[a-b]}` is
  `0 REG_UUNPORT` (measured). This engine is a different one, so those bits would
  have to be *inferred* from the pattern rather than reported, and an inferred
  answer to "what did the compiler notice" is a guess wearing a measurement's
  clothes. The tractable half is not separable either: the result is one
  two-element list, so answering the count and guessing the flags is a wrong
  list rather than a partial one. What did change is the *wording*: `-about` is
  now named as unsupported rather than reported as a bad option, which said
  `bad option "-about": must be … -about …`. `regsub -about` is untouched — it
  really is a bad option there, and that message is already tclsh's.
- **`format %a` and `%A`.** These are the one conversion Tcl does not perform.
  Every other one is computed in `Tcl_AppendFormatToObj`; for `a`, `A`, `e`,
  `E`, `f`, `g` and `G` it rebuilds the C conversion specifier and calls the
  platform library — `snprintf(bytes, segment->length, spec, d)`,
  `generic/tclStringObj.c:2547`. For the decimal forms that is still a fixed
  answer, and they are implemented here. For `%a` it is not: what tclsh prints
  is the C library's, and the C libraries this crate's release matrix builds
  against — macOS, Linux glibc, Linux musl — need not agree.

  They do not. The C standard fixes only part of the form: ISO/IEC 9899:2011
  §7.21.6.1p8 says there is "one hexadecimal digit (which is nonzero if the
  argument is a normalized floating-point number and is otherwise unspecified)
  before the decimal-point character", so a *subnormal* has no defined leading
  digit — the tclsh measured here answers `0x1p-1074` for
  `format %a 4.9406564584124654e-324`, normalising it, where the standard
  permits `0x0.…p-1022`.

  Worse, the rounding is not the standard's either. Measured against tclsh
  9.0.4 on macOS:

      format %.0a 1.5        →  0x1p+0      (round-half-even gives 0x2p+0)
      format %.1a 1.09375    →  0x1.1p+0    (0x1.18 ties; even is 0x1.2p+0)

  That is a known macOS libc bug, not a Tcl one — Apple Developer Forums thread
  803076, "printf %a/%A misrounding (C99 compliance violation) when guard digit
  is 8", reports the same shape and adds that it is not even monotonic:
  `%.0a` of 1.5, 1.53, 1.55, 1.56 prints `0x1p+0 0x2p+0 0x1p+0 0x2p+0` where
  C99 requires `0x2p+0` throughout.

  So there is no single right answer to port. Writing the standard's `%a` makes
  the differential run *here* diverge; writing this libc's makes the crate wrong
  on the two Linux targets and bakes a documented libc bug into it. Neither is a
  port of Tcl. `%A` carries a second decision on top: tclsh 9.0.4 answers
  `-xX0p+0` for `format %A -0.0` and `IxF` for `format %A Inf` (measured) — its
  uppercasing walking over the `0x` prefix and the `inf` spelling — which is the
  same "reproduce an upstream bug?" question `regsub -all -expanded` records
  below, and one that belongs in its own change rather than as a side effect.

  What did change is the wording: the refusal names the C library rather than
  saying "not supported *yet*", which promised a port that is not writable.
- **A `dict` written into an array element.** `dict set a(1) k v` and
  `dict incr a(1) k` are refused: the target travels as a variable *place* — a
  name index or a frame slot — and an array element is neither. Both work on a
  procedure-local variable, which is what the place operand bought.
- **An array variable in a `foreach` variable list.** Refused.
- **Indices outside `i64`.** Tcl computes index arithmetic in arbitrary
  precision and truncates; tclrs saturates at the `i64` ends instead. Both
  produce an index far outside any list, so no case is known where the two
  differ, but the mechanism is not the same one.
- **Math functions a script defines.** The built-in set is complete —
  `src/expr_math.rs` carries every name tclsh 9.0.4 registers under
  `::tcl::mathfunc::` — but `expr` consults only that table. tclsh resolves
  `triple(2)` to the *command* `tcl::mathfunc::triple`, so a procedure of that
  name extends `expr`; here the name resolves to nothing and the call is
  `invalid command name "tcl::mathfunc::triple"`.
- **Two answers a math function gives that this build cannot reproduce
  exactly.** `sin`, `cos` and `tan` of a large argument differ from tclsh in the
  last unit in the last place, because the reference interpreter on this machine
  is an x86-64 binary and the C library it calls reduces the argument
  differently from the aarch64 one this crate links. It is a difference between
  two `libm`s rather than between two implementations of Tcl, and it moves with
  the platform rather than with the code. Separately, `expr {pow(2,64)}` prints
  `1.8446744073709552e+19` here and `1.844674407370955e+19` in tclsh; the
  shorter spelling does *not* read back as 2^64 (`expr {1.844674407370955e19 ==
  2.0**64}` is 0 in tclsh itself), so the divergence is in the reference
  interpreter's shortest-representation formatter, and `expr {2.0**64}` showed
  it before any math function existed.
- **Three `binary` answers this frontend does not reproduce, all of them the
  reference interpreter reading memory it does not own.**
  - `binary format` with an `X` field of count zero followed by any further
    field **crashes** tclsh 9.0.4 with a segmentation fault: `binary format X0c1
    1`, `binary format {X0 f1} 1.5` and `binary format c1X0c1 1 2` all die with
    signal 11. Any other count is fine (`binary format X2c1 1` answers), and so
    is `X0` as the last field (`binary format X0` is the empty string). This
    frontend treats `X0` as the no-op move the manual page describes.
  - `binary decode uuencode` of a line whose length character declares more
    bytes than its characters carry reads past the end of that line: `binary
    decode uuencode !` is one byte in tclsh where the line holds none, and
    `binary decode uuencode "616263"` ends in two bytes that are not in the
    input. This frontend decodes only what the line holds. The `-strict` refusal
    for the same inputs — `short uuencode data` — *is* reproduced, including its
    two different length rules for a line a terminator follows and the last line
    of a message.
  - `binary decode uuencode` of a **truncated final group** (fewer than four
    characters left) reads the characters that are not there. Tcl's own
    *encoder* emits such a group, so the round trip is affected: `binary encode
    uuencode ab` is `"9V(` and decoding it in tclsh reads a fourth character
    from beyond the string. The bytes the declared length asks for are the same
    in both, and the round trip agrees; what differs is the padding beyond them.
- **`clock` before the Gregorian changeover.** tclsh reckons a date earlier than
  its locale's `GREGORIAN_CHANGE_DATE` in the Julian calendar, and that date
  differs per locale — 2299161 for the root catalogue, 2361222 for `en`, later
  still for `ru`, `ro` and `el`. This frontend has one calendar, proleptic
  Gregorian, so an instant before 1752-09-14T00:00:00Z is refused rather than
  answered from a calendar the reference interpreter may not be using.
- **`clock scan` without `-format`.** tclsh's free-form parser is a grammar over
  relative words, month names, ISO forms and zone abbreviations; it is refused
  by name. `-base` is refused with it, and so is reading a time zone by
  abbreviation inside a `%Z` field, which would need the table tclsh builds from
  the whole zone database.
- **`clock`'s `-locale` outside the root catalogue.** The month and day names,
  the AM/PM words and the `%x` / `%X` / `%c` expansions come from `msgcat`;
  only the root catalogue is built in, so `-locale fr` is refused rather than
  answered in English. `%E` and `%O` are refused for the same reason.
- **A POSIX `TZ` rule string with no zone file.** `-timezone :America/New_York`
  and `-timezone +0530` both work — the first through the same `TZif` reader
  tclsh's `LoadZoneinfoFile` implements in Tcl — but `EST5EDT,M3.2.0,M11.1.0`
  spelled out as a rule is refused when no file of that name exists.
- **`file attributes`, `link`, `stat`, `lstat`, `channels`, `system`,
  `tempfile`, `tempdir` and `volumes`.** Each is recognised, so an abbreviation
  resolves as tclsh resolves it, and then refused by name. `glob -types` in its
  two-element attribute form is refused the same way.
- **Non-literal subcommand, body and variable-list words.** A word that is
  itself the result of substitution is refused where the lowering needs it while
  compiling. What remains is three groups:
  - an ensemble *subcommand* — `string $sub x`, `info $sub v`, `array $sub a`,
    and the same for `clock`, `file`, `encoding`, `namespace` and `dict` — since
    each subcommand lowers to a different shape and a computed one has none;
  - a *body* or condition, as in `while $cond $body`;
  - a variable *list* rather than a single name: `foreach` / `lmap` / `lassign`
    variable lists, `dict update`'s variable names, and the array name of
    `array exists` / `names` / `size` / `get` / `set` / `unset`.

  A single computed variable *name* is no longer among them. `set $n`,
  `set $n v`, `incr $n ?by?`, `append $n …`, `lappend $n …`, `unset $n` and
  `info exists $n` resolve the name when the command runs, as tclsh does —
  including an `a(i)` spelling the name carries, a name inside a procedure
  (which is that activation's local, growing a run-time slot when the compiled
  body never mentioned it), a name the body declared with `global` or
  `variable`, and a name `upvar` bound, which resolves to what the link points
  at rather than to the descriptor. See `crate::cmd_scope::dynamic_link` and the
  computed-name programs in `tests/frame_differential.rs`.
- **Editor tooling.** No LSP, no DAP, no inline `rust {}` FFI. `--disasm`,
  `--dump-tokens` and `--dump-ast` exist, the zsh completion is
  `completions/_tclrs` and the man page is `man/man1/tclrs.1`, and
  `docs/reference.html` is generated from the compiler's own tables by
  `cargo run --bin gen-docs` — every command, every ensemble subcommand with the
  compiler's own answer for whether it is implemented, the `expr` ladder as the
  parser binds it, and the `format` conversions the runtime answers to.

## Divergences from tclsh where behavior *is* implemented

The first few are not fuzzer findings — four belong to the event loop and four to
`encoding` — and they are listed first because each is a deliberate decision with
the measurement behind it.

- **`< > <= >= == !=` between an integer past 2^53 and a double round where
  tclsh is exact.** Measured against tclsh 9.0.4: `set l [expr {3**34}]` is
  16677181699666569 and `double($l)` is 16677181699666568, one apart, so tclsh
  answers `==` 0 and `>` 1. tclrs answers 1 and 0. The rule is not in doubt and
  the frontend already holds it — `runtime::numeric` orders any pair with an
  integer in it through `runtime::big_cmp`, which
  `runtime::numeric_hook_tests` asserts operator by operator in both operand
  orders — but these six operators lower to fusevm's native `Op::NumLt` …
  `Op::NumNe`, and `fusevm 0.17.0`'s `cmp_int_fast` answers a pair of native
  numbers itself, through `to_float`, without consulting the hook. The next
  fusevm release delegates such a pair instead, on the grounds that only the
  frontend knows its language's rule, and this divergence closes with no
  further change here. Ordering *is* already exact everywhere the hook is
  reached: `min` and `max` go through `expr_math::extremum`, and
  `tests/bignum_differential.rs` compares those at the same band. Making the
  operators exact without the fusevm change would mean routing every
  comparison with a substituted operand through an extension op, which costs
  the tracing JIT every comparison loop — the trade this frontend declines
  everywhere else it appears.
- **`encoding names` answers what actually converts, not tclsh's list.** tclsh
  answers in the order of its own hash table and includes the three escape-
  sequence encodings it can load; this one is sorted and omits them, because they
  are refused. That makes the list something a script can act on — every name it
  offers converts, which `names_lists_only_what_converts` in
  `tests/encoding_differential.rs` asserts one name at a time — and it is the one
  answer in the ensemble the differential harness deliberately does not compare.
- **`encoding dirs` starts empty.** tclsh's initial value is the directory its
  own library was installed into, because that is where it looks for a `.enc`
  file. The tables here are inside the binary and no file is ever read, so there
  is no search path to report; a list set through the command comes back from it,
  which is the part that is compared.
- **`encoding system` and `encoding dirs` are process state, not interpreter
  state.** Two `tclrs::eval` calls in one process share what the first set, where
  two tclsh processes would each start from the platform's answer. Same shape as
  the channel table, which is also per process.
- **A name's case matters here and does not always matter in tclsh.** `encoding
  convertfrom ISO8859-1 …` works under the tclsh on this machine and `UTF-8` does
  not: the first is loaded from `iso8859-1.enc` through a case-insensitive
  filesystem and the second is a built-in matched exactly in a hash table. The
  case-insensitivity is macOS's, not Tcl's — the same tclsh on a case-sensitive
  filesystem refuses both — so this frontend matches names exactly, which is what
  tclsh does wherever the filesystem is not answering for it.
- **`vwait` notices a write by comparing values, not by tracing them.** Tcl puts
  a write trace on the variable (`Tcl_TraceVar2`, `generic/tclEvent.c:1604`).
  There is no variable trace here, so `vwait` records what the variable held when
  the wait began and compares after every pass. A write that stores *the value
  that was already there* therefore does not end the wait — `set ::d 0; after 0
  {set ::d 0}; vwait ::d` waits on where tclsh returns. Every other write is
  noticed, including a first assignment to a variable that did not exist and an
  `unset`.
- **`vwait` on something nothing can write answers where tclsh hangs.** This one
  runs the other way: `Tcl_VwaitObjCmd` reports `can't wait for
  variable(s)/channel(s): would wait forever` when `Tcl_DoOneEvent` answers 0
  (`generic/tclEvent.c:1755-1763`), and on macOS that never happens, because the
  CFRunLoop blocks with no timeout. Measured: `vwait neverset` under tclsh 9.0.4
  had to be killed after five seconds, with stdin both a terminal and
  `/dev/null`. tclrs raises Tcl's own message instead of entering the wait.
- **An `after` script's failure prints two lines where tclsh prints four.**
  `AfterProc` hands the error to `Tcl_BackgroundException`, which prints the
  message, the `while executing` stack that produced it, and then
  `    ("after" script)`. There is no error stack in this frontend, so the
  message and the `("after" script)` line are printed and the middle is not
  invented. Both go to stderr and the run continues, as tclsh's does.
- **`info locals` lists the locals the compiler has reached, not the ones that
  exist.** A slot is allocated the first time a name is lowered, and lowering
  follows the body's text, so the answer is "the parameters, plus every local
  mentioned textually before this `info locals`, that is set right now". A local
  whose only mention is *after* the call — reachable only by a loop carrying
  control backwards over it — is not listed. `info vars` inside a procedure has
  the same bound on its local half; the names `global`, `variable` and
  `upvar #0` bound into the frame are exact, and are listed whether or not the
  variable they link to is set, as tclsh lists them.

  The names an activation grew *after* it was compiled are exact and are listed
  beside them, so the `dict with` half of this is gone: `proc p {} {set d {a 1 b
  2}; dict with d {puts [lsort [info locals]]}}` answers `a b d` in both, as does
  `proc p {} {eval {set v 1}; info locals}`. `info locals` asked *inside* a
  nested script is exact too, and is answered by a different route: the script is
  a chunk of its own compiled at the script's own level, where there is no scope
  to list, so the frame comes from the projection in effect when it runs
  (`crate::runtime::State::frame_declared`) rather than from the lowering.
- **`info commands`, `info procs` and `info globals` do not list a script
  library.** tclsh answers with `auto_execok`, `auto_load`, `unknown` and the
  rest, and with the `auto_path` global, because `init.tcl` defined them. There
  is no script library here, so the answers are the script's own names and the
  interpreter's own variables. Both implementations answer the same question
  about the script's own names, which is what `tests/event_differential.rs`
  compares.
- **`info script` answers the empty string unless a host sets it.** The library's
  entry point is handed a string, not a file, which is the case tclsh answers
  the empty string for (`tclsh -c`). `crate::cmd_info::set_script` is how a host
  says which file a script came from.

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

### `rename` reaches a call the same chunk had already compiled

`rename` updates the interpreter's command registry, which is what every
`namespace which`, `namespace origin` and `info`-shaped query then answers from,
and the compiler records a literal `rename` so that

* a call written **after** `rename f g` under the new name `g` reaches `f`'s
  body, and
* a call under the old name `f` is guarded — one op, emitted only at a call site
  whose name the same chunk renames — and refuses with `invalid command name
  "f"`, which is what `rename tkInit {}` inside `tkInit`'s own body needs.

What is not modelled is a call written **before** the `rename` runs but reached
after it: `proc f {} {…}; proc g {} {f}; rename f {}; g` refuses in tclsh at the
call inside `g` and reaches the body here, because the guard is placed by
position in the source and `g`'s body was lowered before the `rename` was read.
The same runtime command table that would fix "procedures across an `eval`"
fixes this.


- **`incr` on a non-integer *variable* reports an `expr` operand error.**
  `set x abc; incr x` says `cannot use non-numeric string "abc" as left operand
  of "+"` where tclsh says `expected integer but got "abc"`. An increment the script wrote
  out (`incr x abc`) is checked while compiling and does report `incr`'s own
  wording; the variable's value cannot be, because the check would have to be an
  extension op in the `incr` lowering and `is_trace_op_allowed_at` rejects
  `Op::Extended` — every loop that counts with `incr` would lose its compiled
  trace, which is the one thing this frontend has that reaches native code.
  Deliberately not taken.

  Re-verified rather than assumed, and the mechanism is worth writing down:
  `incr i` lowers to `GetVar / LoadInt(1) / Add` — *the same ops* as
  `expr {$i + 1}` — and the message comes from the numeric hook, whose signature
  is `(NumOp, &Value, &Value)`. The hook sees `NumOp::Add` and the operands and
  cannot see which construct emitted them, so the two cannot be told apart at
  the point the message is made. The distinguishing op would be `Op::Inc`, which
  fusevm's `is_block_eligible_op_at` refuses under strict numeric mode — the
  mode a numeric hook turns on — so using it would cost `counted_loop` its
  trace. Closing this needs the hook to carry the construct, which is a fusevm
  change, not a frontend one. `expr {$x + 1}` on the same value already matches
  tclsh exactly; only `incr`'s own wording differs.
- **Unreachable code costs a script nothing** — the whole class is closed. Both
  halves of it: a command that cannot work raises where it stands, and a body
  whose own text will not parse is lowered *as* that failure, so it raises only
  if the body is entered. `if {0} {puts [expr {1 +}]}`, `while {0} {puts "a}`,
  `proc p {} {puts "a}` with `p` never called, and a `switch` arm that is never
  selected all run to completion, as they do in tclsh.

  One class stays eager, because tclsh reports it eagerly too: an unbalanced
  brace. `if {0} {puts {unclosed}` is `missing close-brace` in both engines —
  brace counting is how the enclosing script delimits the body's word at all, so
  neither can read past it to decide whether the body would ever run. That is
  the boundary, and it was measured rather than assumed: tclsh defers an
  unterminated quote and an unbalanced bracket *inside* a balanced body, and
  refuses an unbalanced brace anywhere.

  What remained of that class was not a diagnostic difference but *when a script
  starts running* — and it is now closed. tclsh evaluates a file command by
  command, so everything before an unparseable command has already run and
  printed by the time the parse error is reported; this crate compiled the whole
  script first and so reported the same error having printed nothing:

  ```tcl
  puts first
  puts second
  if {1} {          ;# the brace never closes
  ```

  tclsh prints `first` and `second` and then reports `missing close-brace`.
  tclrs used to report only the error, and to name the line the scan ran out of
  text on rather than the line the failing command starts on.

  Reaching tclsh's behaviour was once written here as needing to compile and run
  one top-level command at a time, giving up whole-script compilation and with
  it the static dispatch a procedure call resolves through. It does not: the
  whole-script compile still runs first and is untouched on every script that
  parses, and only its FAILURE path recovers anything. `parser::valid_prefix`
  re-scans the source command by command, returns the byte offset just past the
  last one that parsed, and `runtime::run_prefix` compiles and runs exactly that
  prefix — one chunk, static dispatch intact — before reporting the error at the
  failing command's own first line. A script that parses pays nothing, since
  nothing on that path is reached.

  Because the prefix RUNS, it is also what decides which error is reported: a
  command that fails at run time before the malformed text is reached is the
  failure tclsh names, and now the failure tclrs names, instead of the syntax
  error further down. `puts hi; string; puts {` is `wrong # args: should be
  "string subcommand ?arg ...?"` in both.

  The same path serves a nested `eval`, which `Tcl_EvalEx` treats identically:
  `eval "puts b\nputs \{"` writes `b` and then raises. Measured against
  tclsh 9.0.4 in `tests/execution_differential.rs`; on the seed-20260828,
  1500-case fuzz run this took the divergence count from 87 to 51, with
  `stdout-compile-time` going 15 -> 2 and `message-compile-time` 49 -> 25, and
  turned 14 of the 180 committed findings in `tests/fuzz_corpus/` into passes.

  The command half of it — an unknown command, a wrong argument count, an
  unknown ensemble subcommand — is gone: those are raised where the command
  stands rather than while the script is read, so `if {0} {incr}` and
  `if {0} {nosuchcommand}` now run to completion and print nothing, as they do
  in tclsh, and `catch {nosuchcommand}` answers 1 instead of taking the script
  down (`Compiler::defer`, `src/compiler.rs`).

  Three handlers that decided their own refusals *after* emitting ops were
  outside that, and are inside it now, because a refusal raised mid-emit has
  nothing to roll back. `switch` and `array names` read every option before
  emitting anything, so a bad option, an odd number of pattern words, a `-`
  body with nothing after it and a pattern list that will not parse all wait
  for the command (`Tcl_GetIndexFromObj` and `Tcl_SwitchObjCmd` reach every one
  of them while running). `if` reads its whole clause chain into a plan first,
  for the same reason and with the same effect. `catch {switch -- x {a}}` is 1
  rather than a dead script, and the `if` in a `switch` arm nobody selects
  costs nothing — which is the shape the fuzzer's seed-1 case 00196 had.

  A CONDITION was the half none of that reached. `Compiler::command` can only
  turn a refusal into code while the command has emitted nothing, and by the
  time an `elseif`'s test is read `if` has already emitted its first condition
  and branch — so `if {1} {puts taken} elseif {$x in éé} {puts no}` took the
  whole script down where tclsh prints `taken` and never looks at the second
  test. `Compiler::expr_word` now emits an unparseable condition AS its failure,
  the way `body_of` emits an unparseable body, which puts the raise exactly
  where the expression would have been evaluated: nowhere, when the branch above
  is taken; and with the same wording, `in expression` context and line when it
  is. The same word is every command's condition, so `while`, `for`, `switch`
  and `expr` itself all get it from one place.

  Measured on the 400-program run (seed 1, depth 3): **150 parity / 162
  divergence before the command half, 230 / 77 after it, and 269 / 31 once
  bodies were deferred too**. On the 1 500-case seed-20260828 run, deferring the
  condition took the total from **51 divergences to 24** and closed both
  compile-time classes outright: `message-compile-time` 25 -> 0 and
  `stdout-compile-time` 2 -> 0, with seven more of the 180 committed findings in
  `tests/fuzz_corpus/` becoming passes. The `wrong # args` group went from 83 cases to
  none, the ensemble-subcommand group from 19 to none, and the parse-error
  groups — `invalid bareword`, `invalid character`, `missing operand`, the
  unterminated quotes and brackets — from 66 to none. Runtime
  divergences rose from 10 to 17 in the same run, which is the expected shape of
  the change rather than a regression — those scripts now reach code the
  compile-time refusal used to hide, so the differences they were always going
  to have became visible.

  **One of them was a hang, and it is now fixed** — see the strict-undef entry
  below. `tests/fuzz_corpus/message-compile-time-693dbe3e.tcl` was recorded
  `CRITICAL hang`: it contains `catch {... while {$w13 < 1} {}}` over a variable
  that was never set. tclsh raises `can't read "w13": no such variable` on the read, the
  `catch` takes it, and the script ends; here an unset variable reads as the
  empty string, `"" < 1` is true as a string comparison, and the loop never
  ends. Reduced:

  ```tcl
  catch {while {$w13 < 1} {}} m
  puts "caught: $m"
  ```

  Nothing about that is new — it is the unset-variable divergence at the top of
  this section (allowlist A1), which every one of these scripts was always going
  to reach. What changed is that the script now gets there: the compile-time
  refusal used to stop it first. A refusal was masking a hang, which is the
  worse of the two failures, and any further parity work will expose more of
  them. The fix is A1, not a return to refusing early; it is recorded here
  rather than papered over because a hang is the one verdict this project's
  harness never suppresses.

  **What A1 took, now that it is done.** The value model
  already tells absence from emptiness: an unset variable reaches a hook as
  `Value::Undef`, and no assignment can produce one — `set x ""` stores
  `Value::Str("")`. The information is there; what is missing is a *read* that
  refuses. Putting that check in the frontend means an extension op at every
  variable read, and a counted loop reads its counter every iteration, so every
  traced loop in the language would lose its trace — the same wall the `incr`
  wording hits. The shape that works is fusevm's own: it already has a strict
  *numeric* mode, where the VM's arithmetic defers to a host hook instead of
  coercing silently. The equivalent for variables — a strict-undef mode in which
  `GetVar` and `GetSlot` raise through a host callback, with the name index the
  VM already carries — leaves the op native and JIT-eligible while letting this
  frontend supply `can't read "x": no such variable`. That was a fusevm change,
  and it closed the divergence class and the hang together: fusevm 0.16.0's
  `VM::set_undef_hook`, wired in `runtime::Hooks::install`.

  Two details the wiring settled. The hook is told the read's **chunk and op
  index**, because `incr x` on a variable that does not exist creates it at zero
  where `$x` refuses, and both lower to the same read op on the same name — only
  the site separates them. The pair is needed rather than the index alone: a
  nested `eval` is a chunk of its own whose indices start at zero again, and
  `Chunk::op_hash` will not do as the key because it ignores the name pool by
  design (it keys the JIT's native-code cache, where a name is only an index).
  And the array guards, `dict set` and the scalar guards now read through their
  place operand rather than a bare `GetVar`, because a refusing read would fire
  before the guard could answer — `set b 5` emits its guard *before* the
  assignment, so every first assignment to a name used as an array would refuse.

  **What is left**: a procedure-local read. `proc p {} {puts $x}` still reads
  empty rather than naming `x`, and `catch {set x}` in a body answers 0 where
  tclsh answers 1. It is the one case the fuzzer's `A1c` entry still excuses.

  The blocker used to be that nothing carried slot names. That is no longer true:
  fusevm 0.17.0 added `Chunk::sub_slot_names` and `Frame::entry_ip`, and
  `src/procs.rs` fills in the name of every slot of every procedure — which is
  what `uplevel`, `apply` and `eval` in a body now run against. What remains is
  one site in fusevm: `Op::GetSlot` builds its `UndefRead` with `name: None`
  (`vm.rs:1940`), under a comment that 0.17.0 made stale. Resolving it there —
  `self.frames.last().and_then(|f| f.entry_ip)`, then
  `self.chunk.sub_slot_names_at(entry).get(slot)`, skipping an empty name — is
  the whole change, in the shape the `Op::GetVar` arm above it already uses.
  Nothing in this frontend needs to change with it: the hook already reports a
  name it is given (`runtime.rs`, `Hooks::install`) and only falls back to
  `Ok(Value::Undef)` when there is none. The read stays a native op, so no traced
  loop pays for it.

  The entry used to say this was not a patchable defect — that resolving a name
  while compiling is what makes a call an `Op::Call` to a known sub, so fixing
  it meant a runtime dispatch table. That reasoning was wrong in an instructive
  way. Nothing about dispatch had to become dynamic: a command that *cannot*
  work is lowered as code that raises its own error, and a command that can work
  is lowered exactly as before. Static dispatch, the `Op::Call`, and the JIT and
  ahead-of-time paths are all untouched — only the failing command changed shape.

  The parse-error remainder is closed too, and by the same insight one level up.
  It looked architectural — reaching tclsh seemed to mean not parsing a body
  until it ran, at the cost of the compile-once property. It did not. A body is
  still parsed once, where it always was; what changed is that a body whose text
  will not parse is *lowered as* that failure (`Compiler::body_of`,
  `Compiler::emit_body`) instead of failing the command that owns it. The raise
  stands where the body's code would have, which is exactly where tclsh reports
  it, and a body that parses is lowered exactly as before.

  The shape was settled by measurement, not by reading: tclsh evaluates an `if`
  condition and only then fails on an unparsable body (`if {[puts hi; expr 0]}
  {puts "a}` prints `hi` and exits 0), and it runs the commands *before* the
  failing one in a body (`if {1} {puts one; puts [expr {a}]}` prints `one`
  first). So the failure belongs at the command inside the body, and the body's
  own parse failure belongs at the body — not at either enclosing command.
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

- **`puts` with no channel does not go through the channel table.** `close
  stdout` makes `puts stdout hi` report `can not find channel named "stdout"`,
  the same as tclsh, but bare `puts hi` still prints: it lowers to `ext::PUTS`,
  which writes to the interpreter's output sink directly. That op exists to keep
  the common `puts` off the channel path — the whole family is extension ops
  that stop a JIT trace — and routing it through the table would cost a lookup
  on every write to buy a case that only `close stdout` reaches. Named here
  rather than hidden.

### Reached by the widened generator

Seven more, from the 2000-program run above. Each is pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh, and each is reachable only
because the generator now builds `format`'s specifier matrix, draws shift counts
with a sign, and carries `nan` / `inf` in its value pools.

- **`format`'s `-` flag does not override `0`.** `format %-08.2f 1.5` is
  `00001.50` against tclsh's `1.50    `, and `format %-08s ab` is `000000ab`
  against `ab000000`. The integer conversions already agree — `format %-08d 5` is
  `00000005` in both — so this is the `-`-against-`0` rule for `e`, `f`, `g` and
  `s`, not the padding as a whole. Reached only because the generator builds the
  specifier from its axes rather than drawing a fixed spelling.
- **A refusal decided at run time is catchable, so `catch` sees a message where
  tclsh saw an answer.** `catch {dict info {a 1}} m` leaves `m` as
  `dict info is not supported yet` and the script runs on, where tclsh
  answers the hash-table statistics. (`lsort -command` was the example here
  until it landed, and `dict with` until it did.) The refusals decided while
  *compiling* — `string is punct`, `regexp -about` — are not catchable and do
  take the whole case out of comparison as a skip. The two halves are pinned
  together, because which side a refusal falls on is what decides whether the
  harness counts it as a skip or as a divergence.

### Fixed by the four-run campaign

- **`format`'s field padding went on the wrong side** for a left-justified field
  that also carried the `0` flag. tclsh answers three ways, and none is C's —
  C99 says `-` always overrides `0`: `%-08d` is `00000042`, the zeroes staying
  left; `%-08.2f` is `42.00   `, the `0` dropped for spaces on the right; and
  `%-08s` is `42000000`, the `0` kept as the fill but moved right. Only the
  integer case was right here, so `%-012s`, `%-08.2f` and `%-06c` each padded
  the wrong side — a wrong *value* rather than a wrong message, which is why it
  outranked the wording differences around it. Every flag combination is swept
  against tclsh in `tests/string_differential.rs`.

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
  Text left over once an expression is complete follows the same rule: a word is
  named, where a second number or variable is only a missing operator, so
  `expr {1 x}` and `expr {(1)x}` are `invalid bareword "x"` while `expr {1 1}`
  and `expr {1 $x}` are `missing operator at _@_`.
- **`#` starts a comment inside an expression**, running to the end of the line —
  which is why `expr {#1}` is `empty expression` in tclsh.

### Fixed by the fuzzer's own findings

Each of these was a divergence in the run above and is now parity, pinned in
`tests/parity_fuzz_findings.rs` against a live tclsh:

- **`**` with a base of 0, 1 or -1 answers at any exponent.**
  `expr {0 ** 4611686018427387903}` is 0 in tclsh 9.0.4 and was
  `exponent too large` here, as were `1 ** 9223372036854775807` and
  `(-1) ** 4611686018427387903`. Both `**` arms measured the exponent before
  looking at the base — `u32::try_from(j)` in the `i64` arm and
  `u32::try_from(&q)` in `big_arith` — so every exponent past `u32` was refused
  whatever it was raising. Those three bases cannot overflow at any exponent of
  either sign, which is the rule the negative-exponent arm already applied; both
  arms now apply it first, and `(-1) ** n` reads its parity off the low bit
  because a bignum exponent has no `abs()` that fits. `exponent too large` is
  unchanged for |base| >= 2, which is the only base it was ever true of. The
  bignum path matters here even though the base is small: a wide EXPONENT sends
  a small base through it. Found at seed 70123 as
  `if {0 ** 4611686018427387903} {…}`, which tclsh takes the else branch of.
  Regression:
  `tests/bignum_differential.rs::a_base_that_cannot_overflow_answers_at_any_exponent`,
  28 programs of which 11 fail without the change.
- **`format %c` refuses an argument that is not a C `int`.** `%c` hands its
  argument to a C `int`, and `Tcl_GetIntFromObj` accepts a 32-bit word written
  either way, so the window is `i32::MIN ..= u32::MAX` rather than one type's
  range: `-2147483648` and `4294967295` are values, `-2147483649` and
  `4294967296` are `integer value too large to represent`. The conversion took
  `want_int(value)? as u32` instead, which truncates — every out-of-range
  argument printed a character where tclsh fails the command. Inside the window
  nothing changed: the low 32 bits are the code point and one that is not a
  character is U+FFFD, verified byte for byte at `-1`, `-2147483648`,
  `2147483648`, `4294967295` and `1114112`. Found at seed 70123 as
  `format {%+ 5.2c%+40.8o} 4611686018427387903 -1` and
  `format {%+ 0.17c} -4611686018427387904`; closing it took that run from 7
  divergences to 5. Regression: the boundary is in
  `tests/format_differential.rs::character_conversions_match_tclsh`, which
  compares the refusal itself and not just a length.
- **A command substitution inside an EXPRESSION no longer moves the reported
  line.** `if {[llength $x]} {nosuchcommand}` on line 2 reported
  `(file … line 1)` where tclsh reports line 2. `expr.rs` re-parses the
  expression's text, and `parser::command_at` / `parser::quoted_at` each start a
  fresh parser at `line: 1` — so the commands inside `[…]` there are numbered
  from the expression, not from the script. That is exactly the relative
  numbering a re-parsed body carries, and `Compiler::body_depth` is what keeps it
  out of `Compiler::command_line`; only bodies were bumping it. Lowering an
  `Expr::Subst` now bumps it too. A word-level substitution (`puts [foo]`) comes
  from the script's own parse, already carries the absolute line, and is
  unchanged — pinned alongside. Found seven times at seed 70123 (n=900),
  minimising to three distinct cases that were all this one; closing it took that
  run from 14 divergences to 7 and emptied the `location` class. Regression:
  `tests/execution_differential.rs::a_substitution_in_an_expression_keeps_the_scripts_line`.
- **A NaN compares as IEEE says it does.** `expr {nan > 1}` and `expr {nan >= 1}`
  answered 1 where tclsh answers 0: the numeric hook ordered its operands with
  `partial_cmp` and called the `None` a NaN produces `Ordering::Greater`. Every
  ordered comparison against a NaN is false and `!=` is the one that is true, so
  the hook answers that directly instead of inventing an ordering.
- **A NaN is refused where it is asked to be a boolean.** `if {nan} {…}`,
  `expr {nan && 1}` and `expr {nan ? 1 : 2}` took the true branch; tclsh says
  `floating point value is Not a Number`. The check already existed in
  `ext::BOOL` — nothing reached it, because a condition skips that op when its
  value is statically a number. `Compiler::can_be_nan` narrows that: an
  expression that yields an *integer* cannot be a NaN, and a counted loop's test
  is a comparison, which does — so `while {$i < $n}` still carries no extension
  op and keeps its trace, while a float-valued condition reaches the check.
  `expr {!nan}` and `expr {-nan}` name the operand the script wrote, and
  `expr {nan}` alone raises tclsh's `domain error: argument not in valid range`
  when it runs rather than when it is read.
- **`format %p`** is hexadecimal over the whole word, always prefixed. It was
  refused; it differs from `%#x` in exactly two ways, both pinned: a zero keeps
  its prefix (`0x0`, where `%#x` gives `0`) and the value is taken as a full 64
  bits (`-1` is `0xffffffffffffffff`, where `%#x` gives `0xffffffff`).
- **A refused operand is quoted by its spelling**: `expr {1e10 % 3}` names
  `1e10` and `expr {nan + 1}` names `nan`, where the value's own formatting
  would have said `10000000000.0` and `NaN`. tclsh quotes an operand by its
  string representation, which for a literal is what the script wrote;
  `Compiler::numeric_operand` pushes that spelling for a literal the formatter
  would not reproduce, and the numeric hook parses it back into the operation.
  A literal already in canonical form (`1.5`, `0.5`, `-0.0`) is untouched and
  stays a native operand. The claim that `1.0e-7` was canonical was wrong —
  `format_double(1e-7)` is `1e-7` — and that case was in the committed corpus.
- **A NaN *operand* is refused as an operand**, not reported as a domain error.
  `expr {nan + 1}` and `expr {nan * 2}` answered `domain error: argument not in
  valid range`, which is what tclsh says for a NaN *result*; an operand that is
  already NaN is `cannot use non-numeric floating-point value "nan" as left
  operand of "+"`. The literal now reaches the operation as its spelling, so the
  operand rule sees it before the result gate does.
- **`in` and `ni` split the list on Tcl's string form.** The haystack was split
  through fusevm's `as_str_cow`, which spells a double the VM's way, so the list
  `3.0` became the one-element list `3` and `expr {3 in 3.0}` was true where
  tclsh says false. Only reachable once a double literal could arrive as a
  `Value::Float`.
- **An integer beyond `i64` promotes**: `expr {99999999999999999999 + 1}` is
  `100000000000000000000`, `1 << 64` is `18446744073709551616`, and
  `expr {-9223372036854775808}` answers rather than being refused for a spelling
  one past `i64::MAX`. Three stages of this are now closed — the value first
  became a double and answered `1e+20`, then became a refusal, and is now exact.
- **`lsort -integer` refuses a value too wide for a machine integer**, as tclsh
  does. `list::parse_int` saturated to `i64::MAX` instead, so
  `lsort -integer {99999999999999999999 5}` sorted by a value the script never
  wrote; an *index* still saturates deliberately, since one past the end is out
  of range either way (`list::parse_int_exact`).
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
  **Where the separator may go is part of that grammar**, and accepting it
  everywhere was its own divergence: `_` is legal only *between* two digits, so
  `1_000_000` and even `1__0` are numbers while `0x_10`, `0b_10`, `0o_17`,
  `0d_9`, `1_`, `0x1_`, `1_.5`, `1e_10`, `1e10_` and `1_e10` are refused as
  tclsh refuses them — each `invalid bareword`, naming the word the separator
  sits in rather than the number being read, which is why `1.5_` is
  `invalid bareword "5_"` and not `"1.5_"`. The runs the rule applies to are all
  three: `12_34.56_78e9_0` is a number, where the fraction and the exponent used
  to end the literal early. A leading `_` starts no word at all — `expr {_1}` is
  `invalid character "_"` — and a `.` that never resolves into a number is named
  itself, so `expr {1._5}` and `expr {.x}` are `invalid character "."`
  (`src/expr.rs`, pinned in `tests/expr_lexer_differential.rs`).
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
  **Where the separator may go is part of that grammar**, and accepting it
  everywhere was its own divergence: `_` is legal only *between* two digits, so
  `1_000_000` and even `1__0` are numbers while `0x_10`, `0b_10`, `0o_17`,
  `0d_9`, `1_`, `0x1_`, `1_.5`, `1e_10`, `1e10_` and `1_e10` are refused as
  tclsh refuses them — each `invalid bareword`, naming the word the separator
  sits in rather than the number being read, which is why `1.5_` is
  `invalid bareword "5_"` and not `"1.5_"`. The runs the rule applies to are all
  three: `12_34.56_78e9_0` is a number, where the fraction and the exponent used
  to end the literal early. A leading `_` starts no word at all — `expr {_1}` is
  `invalid character "_"` — and a `.` that never resolves into a number is named
  itself, so `expr {1._5}` and `expr {.x}` are `invalid character "."`
  (`src/expr.rs`, pinned in `tests/expr_lexer_differential.rs`).
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
- **`${name}` ends at the close brace that BALANCES the ones inside it**, not at
  the first one: `${a{b}c}` is the variable `a{b}c`, a backslash consumes the
  byte after it (so `${a\}b}` names `a\}b`, both bytes kept), and running out
  of text is `missing close-brace for variable name`. Read as "up to the first
  `}`", `puts ${` followed by a line holding a balanced `{…}` swallowed the rest
  of the script into the name and reported `can't read "…"` instead. A port of
  `Tcl_ParseVarName`'s `braceCount` scan (`generic/tclParse.c:1383-1416`).
- **A refused operand is named as the script spelled it**, not as the number it
  parses to: `expr {~inf}` is `cannot use floating-point value "inf" as operand
  of "~"` and `expr {~1.50}` names `"1.50"`. The binary operators already
  carried the spelling; unary `~` read the parsed double back and answered
  `"Inf"` and `"1.5"`. A *computed* operand has no spelling to carry and stays
  canonical, which is why `expr {~-inf}` is `"-Inf"` — the sign makes it one.
- **A failure inside a body** is located at the script's own command, which is the
  line tclsh's `(file "…" line N)` names. A **procedure** body is the one that is
  not located at all: tclsh's `(file …)` there is the CALL site, which the same
  body reaches from every call there is, so the compiler cannot know it and the
  body's own line is not it — a three-line procedure called from line 10
  reported `line 3` where tclsh reports `(procedure "p" line 3)` for that
  position and `(file … line 10)` for the file. It now reports no location,
  which is what every other run-time failure inside a procedure already did.
- **Input nesting is bounded** by `parser::MAX_NESTING_DEPTH` (64_000, measured),
  so the deepest input reports a Tcl error instead of aborting the process. The
  limit sits above every depth tclsh survives — it segfaults on 30_000 nested `[`
  — so nothing tclsh can parse became a refusal. Found by the `parse` cargo-fuzz
  target (`fuzz/fuzz_targets/parse.rs`), not by the differential fuzzer: no
  generated *program* has fifty thousand open brackets. A host embedding the
  library on a stack smaller than `runtime::RECOMMENDED_STACK` still has to give
  the parser the stack this crate documents; the limit is calibrated for that one.

The four divergences the fuzzer's report allowlists rather than counting are the
documented ones, and each is pinned in `tests/parity_fuzz_findings.rs` too, so an
entry cannot outlive the behavior it excuses: an unset *procedure-local* reading
as `""` — a frame slot has no name to report, and the global case is fixed —
`array names` / `array get` sorted where tclsh hashes (order is unspecified in
`array(n)`), arity refused before anything runs, and a message carrying
` (line N)` through the library. A fifth, an unterminated brace located where
the input ran out, was retired when the behaviour was fixed. `scripts/fuzz/classify.pl` holds them with their reasons, and every run
prints a hit count per entry.

## What the differential fuzzer cannot reach

The generator's own blind spots, so a gap in the report is a known gap rather
than an unexamined one. Measured against the 2000-program run above.

- **`upvar` and `uplevel` reach any level; `apply` needs its lambda written
  out.** All three have to reach variables that are not the running chunk's
  globals. A procedure's locals here are frame slots the compiler assigned, and
  nothing in a *built* chunk mapped a name onto one — which is what limited both
  commands to the global level.

  **Option 1 below is what landed.** The compiler now publishes, per procedure
  body, which name each of its frame slots was written as, keyed by chunk identity
  the way the tolerant-read set is (`cmd_scope::SlotNames`). A live frame is
  attributed to its body without any new machinery: `Op::Call` records
  `return_ip`, so the op before frame *k+1*'s `return_ip` is in the body frame *k*
  is running, and the innermost frame's is the op before `vm.ip`. With that,
  `upvar` at any level resolves its target to one of three homes — a global at its
  index in the chunk's projection, a global whose computed name the chunk's table
  does not carry (interned past the end of it by `runtime::intern_overflow`), or a
  frame slot by frame index and slot — optionally with an array element key,
  because `upvar ::tk::FocusGrab($i) data` (`library/tk.tcl:145`) links a local to
  one element. `uplevel` into a procedure activation projects that frame's named
  locals into the interpreter's variables, runs the script as a chunk of its own,
  and reads the values back.

  `apply` takes a different route to the same place, and takes a lambda the
  script *computed* with it: `runtime::apply_op` writes the lambda out as a `proc`
  with a name no Tcl name can be, runs that source, and renames the synthesised
  name out of any diagnostic — so a lambda's parameters are a procedure's frame
  slots and `return` returns from it, whether the text was in the chunk or not.
  What is left refused is a lambda's third element naming a second namespace,
  this frontend having one.

  **What it cost, and what option 2 would have cost.**

  1. *A per-procedure slot-name table.* Compile-time metadata: a chunk that never
     says `upvar` carries it, never reads it, and runs the ops it ran before. What
     is paid is by the *linked name only* — `Compiler::var_place` answers
     `Place::Link` for it, so its reads and writes are extension ops rather than
     `GetSlot`/`SetSlot` and a loop over one is not traceable. Measured:
     `tclrs --tiers bench/counted_loop_proc.tcl` still reports `traced=true` and
     `reaches native code true`, in both feature sets, because the procedure's
     *other* locals are still slots.
  2. *A run-time variable table addressable by name at any level*, which is what
     the reference interpreter has. It costs **every** procedure local its slot,
     and with it fusevm's block and tracing tiers, which read slots out of a flat
     `i64` buffer (`refresh_slot_buffers`) and cannot see a hash table. That is
     not a trade this frontend can make: `bench/counted_loop_proc.tcl` reaching
     native code is the whole reason locals are slots, and every procedure in
     every script would lose it to buy a feature a few of them use.

  The second is still the real fix — the same machinery that would move an
  unknown command name from compile time to run time — and it is a trade, not a
  free win.

  What option 1 cannot do is give `upvar` a slot to point at that the procedure
  running there never wrote, so `upvar 1 neverused z` is refused rather than
  silently dropped. `uplevel` is not limited by it — it projects the frame rather
  than addressing a slot — and `fusevm::Chunk::sub_slot_names` (0.17.0) is the
  second half of the same record, carried by the chunk itself and read by
  `VM::slot_names_at`, which is what the projection uses. See the refusal list
  above.
- **Commands tclrs does not have.** `interp`, `binary`, `trace`, `socket` and
  `exec` are outside the command set entirely, so a generated use of one is
  `invalid command name` and says nothing about parity. `{*}` expansion,
  `namespace`, `rename`, `source`, `encoding` and file I/O were on this list
  until each landed; the generator should reach them now. `uplevel`,
  `upvar`, `variable` and `apply` were on this list until `src/cmd_scope.rs`
  landed; what they now refuse is the entry above, and what they answer is
  `tests/event_differential.rs`. They are deliberately not generated, and belong
  in the generator on the day the commands exist. `regexp`, `regsub`,
  `lassign`, `lset`, `lpop`,
  `ledit`, `lrepeat`, `lremove`, `lseq` and `lmap` exist now and are not
  generated yet, so the run above says nothing about them either; what does is
  `tests/list_commands_differential.rs`.
- **`array` on a procedure local, `unset` of one, and `eval` inside a procedure
  body** are generated, at `RARE_SHAPE_RATE` — so are the `dict` subcommands
  outside the implemented set. All three were refusals when the rate was named
  and are comparisons now, as are `lsort -command` and every option in the
  generator's two "rare" lists: at seed 1, depth 4, 200 cases the skip bucket is
  1 and its one entry is `format %a`. The rate stayed where the refusals put it,
  so those shapes are drawn rarely and are under-measured for it; raising it
  moves what every seed generates and is a change of its own.
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

- **tclsh 9.0.4 reads one byte past its input to describe a `cesu-8` decoding
  error.** When a `cesu-8` decode ends on an unpaired high surrogate under
  `-profile strict`, `UtfToUtfProc` reports a failure at an index equal to the
  input's *length* — it does not rewind — and `Tcl_ExternalToUtfDStringEx` then
  formats `srcStart[nBytesProcessed]`, which is one past the end of the byte
  array. The value is whatever follows in the heap: `encoding convertfrom
  -profile strict cesu-8 \xED\xA0\x80` says `'\x00'` in a small script and said
  `'\xED'` in 699 of 700 cases inside a sweep, and `'\x0E'` in the remaining
  one. tclrs reports `'\x00'`, the answer that does not depend on the heap. The
  `-failindex` value for the same input is 3 in both, so only the message
  differs, and no test asserts tclsh's side of it because there is nothing
  stable to assert.
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
