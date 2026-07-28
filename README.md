```
████████╗ ██████╗██╗     ██████╗ ███████╗
╚══██╔══╝██╔════╝██║     ██╔══██╗██╔════╝
   ██║   ██║     ██║     ██████╔╝███████╗
   ██║   ██║     ██║     ██╔══██╗╚════██║
   ██║   ╚██████╗███████╗██║  ██║███████║
   ╚═╝    ╚═════╝╚══════╝╚═╝  ╚═╝╚══════╝
```

![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-in%20development-9b5de5?style=flat-square)

### `[TCL, COMPILED TO BYTECODE — LOWERED ONCE, NOT RE-PARSED PER EVALUATION]`

> *"tclsh interprets Tcl. tclrs compiles it to fusevm bytecode."*

**Tcl in Rust** — a Tcl frontend that parses Tcl source and lowers it to
[`fusevm`](https://github.com/MenkeTechnologies/fusevm) bytecode, the shared
execution engine behind `zshrs`, `stryke`, `awkrs`, `vimlrs`, `elisprs`,
`rubylang`, `pythonrs`, `phplang`, `node-js`, `rlang`, `go-rs`, and the JVM
frontends. No bespoke VM. No interpreter loop and no code generator in this
crate — those belong to the VM.

The reference implementation is **tclsh 9.0.4**. It is the specification:
behavior is ported from it, not reinvented, and the test suite compares against
it directly rather than against expectations written by hand.

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] Build](#0x01-build)
- [\[0x02\] The Binary](#0x02-the-binary)
- [\[0x03\] The Library](#0x03-the-library)
- [\[0x04\] Language Surface](#0x04-language-surface)
- [\[0x05\] What Is Refused](#0x05-what-is-refused)
- [\[0x06\] The Parser](#0x06-the-parser)
- [\[0x07\] Architecture](#0x07-architecture)
- [\[0x08\] JIT Compilation](#0x08-jit-compilation)
- [\[0x09\] Ahead-of-Time Compilation](#0x09-ahead-of-time-compilation)
- [\[0x0A\] Benchmarks](#0x0a-benchmarks)
- [\[0x0B\] Conformance](#0x0b-conformance)
- [\[0x0C\] Testing](#0x0c-testing)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

Tcl 9 evaluates through a bytecode engine wrapped around a dual-representation
object model, re-deriving string representations as values cross command
boundaries. tclrs takes a different path: it parses a script once — resolving
every substitution the grammar permits at parse time — and lowers each command
to `fusevm` bytecode, the same bytecode sixteen other language frontends emit.

- **Compiled, not re-parsed** — a braced body is fully known at parse time, so
  `if` / `while` / `for` bodies and braced `expr` expressions compile once into
  bytecode instead of being re-parsed on every evaluation. Words carry a
  `braced` flag for exactly this decision.
- **fusevm-hosted** — no local `vm.rs` / `jit.rs`, no bespoke object heap. Tcl
  strings, integers and floats map onto `fusevm::Value` directly; a value
  produced as a number stays a number in a VM slot and only acquires a string
  representation when something asks for one.
- **Native arithmetic** — `+ - *`, the comparisons, the bitwise and shift
  operators, and the short-circuiting `&&` / `||` lower to native fusevm ops.
  Only the operators whose Tcl meaning differs from the VM's generic one — `/`,
  `%`, `**` — take a frontend extension op, and only operands the VM cannot
  compute on natively (mostly strings) take the numeric hook.
- **One driver for everything** — procedure calls, `catch` unwinding, coroutine
  switching and nested `eval` all go through a single driver that owns the
  interpreter's variables and installs every VM hook in one place.
- **Compiled ahead of time** — `tclrs --aot out script.tcl` lowers a script
  through fusevm's closed-world compiler to a native object and links it into a
  standalone executable with no parser and no bytecode dispatch loop inside it.
- **JIT armed, and honest about it** — every VM this crate builds enables
  fusevm's Cranelift tiers, and `tclrs --tiers` reports which of them a given
  script actually reaches. Today that answer is *none*, for reasons this README
  names precisely: see [JIT Compilation](#0x08-jit-compilation).
- **Differentially tested** — every program in the suite is executed by both
  `tclsh` and tclrs and the output compared byte for byte. No expected output in
  this repository is written by hand.

---

## [0x01] BUILD

```sh
git clone https://github.com/MenkeTechnologies/tclrs
cd tclrs
cargo build
cargo test
```

Requires a stable Rust toolchain, and a C compiler for `--aot` to link with.

`cargo build` produces three artifacts: the `tclrs` binary, the `tclrs` rlib,
and `libtclrs.a` — the staticlib an ahead-of-time object links against.

The differential tests invoke `tclsh` (or `tclsh9.0` / `tclsh8.6`) from `PATH`
and report a skip when none is installed, so the suite still runs on a machine
without Tcl.

---

## [0x02] THE BINARY

```text
tclrs FILE ?arg ...?        run a script file
tclrs -c SCRIPT ?arg ...?   run SCRIPT
tclrs                       read from stdin; a REPL when stdin is a terminal
tclrs --version             print the version    (also -V)
tclrs --help                print the usage      (also -h)
```

`tclsh` is the specification for what the binary prints and what it exits with.

| Behavior | What happens |
| --- | --- |
| A script file | One script. The first failure ends it: the message goes to stderr, followed by `    (file "…" line N)` when the failure was located while compiling, and the process exits 1. |
| Stdin, not a terminal | A sequence of commands. Each is evaluated as it completes, a failure is reported on stderr and the next command still runs, and end of input exits 0 — which is why `tclrs < script` exits 0 where `tclrs script` exits 1. |
| Stdin, a terminal | The same loop with a `% ` prompt and the value of each command echoed. A command spanning lines keeps reading while a brace, quote or bracket is still open; the continuation prompt is empty, as `tclsh`'s is. |
| `argv0`, `argc`, `argv` | Set before the script runs, as `tclsh` sets them. |
| Errors | stderr only. No banner, no prompt outside a terminal, and no output the binary produces that the script did not ask for. |

An unknown option is refused (`tclrs: unknown option "--wat"`) rather than
treated as a file name — the one place this binary deliberately differs from
`tclsh`, which reads stdin for any argument starting with `-`.

### Options that do not run the script the ordinary way

```sh
tclrs --aot out script.tcl          # compile to a standalone native executable
tclrs --aot-object out.o script.tcl # emit the relocatable object only
tclrs --tiers script.tcl            # run it, then report which fusevm tiers took it
tclrs --disasm script.tcl           # print the compiled bytecode instead of running it
```

Each of those wants a whole script, so it reads a file, a `-c` argument, or all
of stdin, and never opens a REPL.

`TCLRS_JIT=off` (or `0`, or `no`) skips arming the JIT. It exists so the
benchmark can measure the interpreter and the JIT-armed VM as separate rows of
the same binary.

fusevm's own knobs work unchanged: `FUSEVM_JIT_BLOCK_THRESHOLD`,
`FUSEVM_JIT_TRACE_THRESHOLD`, `FUSEVM_JIT_CACHE_DIR`.

---

## [0x03] THE LIBRARY

`tclrs::eval` compiles and runs a script in a fresh interpreter, returning its
value and everything it wrote to stdout:

```rust
let out = tclrs::eval("set x 5\nputs [expr {$x * 2}]").unwrap();
assert_eq!(out.output, "10\n");
```

`tclrs::Interp` is the same thing with the state kept between calls, which is
what a REPL needs and what the `eval` command needs:

```rust
let mut interp = tclrs::Interp::capturing();
interp.set_global("argv", "a b c");
interp.eval("set total 0").unwrap();
interp.eval("foreach x {1 2 3} {set total [expr {$total + $x}]}").unwrap();
assert_eq!(interp.global("total").as_deref(), Some("6"));
```

| Entry | What it is for |
| --- | --- |
| `Interp::new` | Scripts write to the process's stdout, through one buffered writer flushed at the end of each evaluation. |
| `Interp::capturing` | Scripts' writes are collected for `Interp::take_output`. |
| `Interp::set_global` / `Interp::global` | Host access to the interpreter's variables. |
| `Interp::set_recursion_limit` | How deep `eval` may nest. The default is `DEFAULT_RECURSION_LIMIT` (1000), which needs `RECOMMENDED_STACK` (256 MiB) of thread stack; the binary spawns a thread that size. Nesting deeper is a script error, never a stack overflow. |
| `Interp::cache_stats` | `(hits, misses)` from the source-keyed chunk cache — one miss per compilation, so the same `eval` text in a loop is lowered once. |
| `tclrs::eval_captured` | `eval` for a caller that wants both halves of a failing run: the error *and* whatever the script had already printed. |
| `tclrs::parse` | The parsed `Script` without running it, for tooling that wants the word structure. |
| `tclrs::aot` | `compile_object`, `compile_executable`, and `run_native` — the same codegen driven in-process. |
| `tclrs::tiers` | `report` and `inspect`: which fusevm tiers a chunk reaches. |

---

## [0x04] LANGUAGE SURFACE

### Commands

| Group | Commands |
| --- | --- |
| Variables | `set`, `incr`, `unset`, `append`, array variables (`a(k)`), `global` |
| Output | `puts`, with `-nonewline` |
| Expressions | `expr` |
| Control flow | `if` / `elseif` / `else`, `while`, `for`, `foreach`, `switch` (`-exact`, `-glob`), `break`, `continue` |
| Procedures | `proc`, `return` (with `-code ok` / `-code error`) |
| Errors | `catch`, `error` |
| Coroutines | `coroutine`, `yield`, `yieldto`, `info coroutine` |
| Run-time evaluation | `eval` |
| Lists | `list`, `llength`, `lindex`, `lappend`, `lrange`, `lreverse`, `linsert`, `lreplace`, `lsearch`, `lsort`, `join`, `split`, `concat` |
| Associative data | `array` — `exists`, `get`, `names`, `set`, `size`, `unset`; `dict` — `create`, `exists`, `get`, `keys`, `merge`, `remove`, `set`, `values` |
| Strings | `format`, and the `string` ensemble — `cat`, `compare`, `equal`, `first`, `last`, `index`, `insert`, `is`, `length`, `map`, `match`, `range`, `repeat`, `replace`, `reverse`, `tolower`, `totitle`, `toupper`, `trim`, `trimleft`, `trimright` |

Command substitution works on any of them.

### `expr`

The whole operator set of `expr(n)`:

| Group | Operators |
| --- | --- |
| Arithmetic | `+ - * / % **`, unary `+ -` — with Tcl's floored integer division and remainder, and integral `**` for integral operands |
| Comparison | `< > <= >= == !=` — numeric-preferring, falling back to string order |
| String comparison | `lt gt le ge eq ne` — always string |
| Bitwise / shift | `& ^ \| ~ << >>` |
| Logical | `&& \|\| !`, short-circuiting; the ternary `?:` |
| Membership | `in` `ni` — string equality against a list's elements, so `1 in {01}` is false |

Operands are literals, variables, nested commands (`[…]`), quoted and braced
strings, and parenthesised subexpressions. Doubles print in Tcl's format: the
shortest representation that reads back exactly, never looking like an integer,
exponential outside the positional range.

### Lists

A Tcl list is a string, so every list command re-derives its elements from one.
Both directions are ports of the reference implementation rather than
reconstructions from the manual, because neither is what a reading of the manual
would suggest.

| Piece | How |
| --- | --- |
| **Parsing** | `TclFindElement`: whitespace separates elements, a leading brace or quote delimits one, and backslash sequences resolve everywhere except inside braces — the same escape table as rule 9, reached through the same code. |
| **Formatting** | `TclScanElement` / `TclConvertElement`, including the historical mode where an element needing protection only because of a `]` or an internal `"` has those escaped while its braces are left bare: `list {a]b}` is `a\]b`, not `{a]b}`. An empty element is `{}`, and a leading `#` is quoted in the first element only. |
| **Indices** | `end`, `end±n`, `m±n` and the integer grammar (`0x` / `0o` / `0b` / `0d` prefixes, `_` separators), resolved as `Tcl_GetIntForIndex` resolves them. |
| **`lsort`** | The reference merge sort, element for element — with `-unique` the algorithm rather than the ordering decides which of two equal elements survives, so a library sort would give a different answer. |
| **`lsearch`** | The reference option parsing, including unique-prefix abbreviation and the rule that `-integer` / `-real` only apply in `-exact` mode. |
| **`foreach`** | Any number of variable lists and value lists; the longest list fixes the iteration count and shorter ones supply empty values. The loop state rides the VM stack rather than a variable a script could reach, and is read in place, so no copy happens per iteration. |

### Procedures

A procedure's parameters and locals are **frame slots**, not entries in the
global table: `proc` collects every signature before anything is emitted, so a
procedure may call one the script defines further down, and a call site pushes
one value per formal — filling in defaults and collecting a trailing `args`
there rather than in the body. `global` moves a named variable back to the
global table for the body that declared it.

### Coroutines

A coroutine is a second `fusevm::VM` over the same chunk. `coroutine name cmd
?arg…?` positions it at the procedure's entry and enters it; `yield` halts that
VM and hands its value to whoever resumed it; resuming pushes a value and runs
it again. `yieldto` donates the resumer to another coroutine of the script, so
the value of a call that ends in `yieldto` is whatever the target eventually
produces. A body may suspend at any depth, inside a loop, and inside an open
`catch`; an error that escapes a body deletes the coroutine and is reported to
whatever resumed it; a coroutine's command goes away when its body ends, so a
later call reports `invalid command name`.

The driver owns the one global variable table and moves it into whichever VM is
about to run, so every context sees the same variables. Exactly one VM runs at
a time, so that is a move and not a copy.

### `eval`

`eval` is the one command whose script is a value rather than braced text, so
it is compiled when the op runs. The chunk cache is keyed by the source text —
identical source is identical bytecode, whatever produced it — so `eval` in a
loop is lowered once however many times it runs. The nested script sees the
interpreter's variables in both directions, including the ones a failing nested
script had already set.

---

## [0x05] WHAT IS REFUSED

Nothing is approximated. A construct this frontend has not built is an error,
at compile time where the script's shape decides it and at run time where a
value does. [`BUGS.md`](BUGS.md) is the ledger.

| Refused | Message |
| --- | --- |
| Any command outside [the list above](#0x04-language-surface) | `invalid command name "X"` |
| `{*}` argument expansion | `{*} argument expansion is not supported yet` |
| Every `expr` math function | `math function "sin" is not supported yet` |
| A variable or body word that is not literal (`set $name …`) | the word is refused where a literal is required |
| An array variable in a `foreach` variable list | `array variables are not supported yet` |
| `array` / `dict` on a procedure-local variable | `… of the procedure-local variable "x" is not supported yet` |
| `array startsearch` and the other search subcommands | `array startsearch is not supported yet` |
| `dict` subcommands outside the implemented set; `dict set` into an array element | `dict append is not supported yet` |
| `string` subcommands outside the implemented set; `string is -failindex` | `"string wordend" is not supported yet` |
| `format` conversions outside the implemented set | `the "%n" conversion is not supported yet` |
| `lsearch -regexp` / `-sorted` / `-dictionary` / `-nocase` / `-index` / `-stride` / `-subindices` / `-bisect`; `lsort -command` / `-dictionary` / `-index` / `-nocase` / `-stride` | `lsearch -regexp is not supported yet` |
| `proc` anywhere but a script's top level; redefining a built-in; redefining a procedure; a procedure and a coroutine of the same name | `"proc" is only supported at the top level of a script` |
| `return` outside a procedure; `return` or `break` or `continue` out of a `catch` script; `return -code` other than `ok` or `error`; `return`'s other options | `"return" outside of a procedure is not supported` |
| `catch`'s third (options-variable) argument; `error`'s `info` and `code` arguments | `… the options variable is not supported` |
| `eval` inside a procedure body | `"eval" inside a procedure is not supported: the script it builds cannot reach the procedure's local variables` |
| `coroutine` anywhere but a script's top level or a command substitution in one; a coroutine of a built-in or of anything but one of the script's procedures; `yieldto` at a command that is not a coroutine of the script | `"coroutine" is only supported at the top level of a script, or in a command substitution in one` |
| `info`, apart from `info coroutine` | `unknown or unsupported subcommand "exists": only "info coroutine" is supported` |
| Arbitrary-precision integers. An `i64` that overflows is an error, and so is the one integer division whose true quotient does not fit (`i64::MIN / -1`) | `integer value too large to represent` |
| Ahead-of-time compilation of a script using `catch` or a coroutine | `ahead-of-time compilation of a script using "catch" is not supported: it needs the driver that only the interpreter has` |

`coroprobe`, `coroinject` and deleting a coroutine by renaming its command are
not implemented; a coroutine goes away when its body ends.

---

## [0x06] THE PARSER

`tclrs::parse` implements all twelve syntax rules of `Tcl(n)`:

| Rule | Covered by |
|---|---|
| 1 Commands, 3 Words | command and word splitting, line tracking |
| 2 Evaluation | words retained in order for the compiler |
| 4 Double quotes | quoted words with substitution |
| 5 Argument expansion | `{*}` recorded on the word |
| 6 Braces | nesting, literal text, backslash retention |
| 7 Command substitution | nested scripts parsed eagerly |
| 8 Variable substitution | `$name`, `$name(index)`, `${name}`, `${name(index)}` |
| 9 Backslash substitution | full escape table, including the backslash-newline pre-pass |
| 10 Comments | `#` in first-word position only |
| 11, 12 Order and word boundaries | single pass, substitution never splits a word |

Rule 11 rules out rescanning substituted values, so each character is processed
once and the compiler can resolve variable and command references statically
wherever the word shape allows.

---

## [0x07] ARCHITECTURE

tclrs contains no virtual machine, no interpreter loop, and no code generator.
The execution path mirrors how `zshrs` hosts zsh and `groovyrs` hosts Groovy:

```
Tcl script → parser (Script/Command/Word) → fusevm bytecode → Interp → Machine → fusevm VM
                                                                          │
                                                     numeric hook (string operands, overflow)
                                                     extension ops (/ % ** floored, normalize, …)
                                                     enable_tracing_jit
```

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`. Each command lowers into a `fusevm::Chunk` and runs on the shared VM. |
| **`Interp`** | The variables of a session, keyed by name, plus the source-keyed chunk cache. A chunk interns its own name table, so a slot vector cannot cross evaluations; the map is the authority and the vector is projected out of it on entry and read back into it on exit. |
| **`Machine`** | One evaluation. It switches coroutine contexts, unwinds `catch`, services the requests coroutine ops raise, and moves the global slot vector between the VMs of one chunk. Every one of those works the same way: an op stashes something in a cell and halts, and the driver reads the cell after `run()` returns. |
| **One install point** | The output sink, the numeric hook, the extension dispatch and `enable_tracing_jit` are installed in exactly one function, so the main VM, a coroutine's VM, a nested `eval`'s VM and an ahead-of-time run all behave alike. |
| **Numeric hook** | Catches operands the VM cannot compute on natively. An operand that parses as a number is one (including the `0x` / `0o` / `0b` radix prefixes); comparisons fall back to string order when it does not; arithmetic on a non-number is an error. |
| **Extension ops** | `/` and `%` floor toward negative infinity (`-57 / 10` is `-6`, `-57 % 10` is `3`), `**` stays integral for integral operands, and a normalize op converts a VM-native result into its Tcl value — booleans to `1`/`0`, doubles to Tcl's double format. The list, associative and string commands are extension ops too. |
| **No object heap** | Tcl's value model needs none on top of fusevm's: strings, integers and floats map onto `Value` directly. |

Extension op ids are laid out so `runtime`'s dispatch can test ranges from the
highest base down: the arithmetic and normalizing ops at 0–5, `eval` at 6,
control flow at 7–9, the coroutine ops at 10–14, the list commands from 16, the
associative ones from 64, and the string ones from 128. `catch` is the one op
whose payload is an op index, so it is an extension-*wide* op.

Static stack tracking is what keeps the lowering cheap: each command leaves its
result on the stack and the compiler tracks that depth as it goes, so `break`
and `continue` unwind with a known number of pops rather than a runtime
unwinder.

---

## [0x08] JIT COMPILATION

### How it is turned on

`fusevm` is pulled with the Cranelift features, so `cargo build` links the JIT
and the persistent native-code cache:

```toml
fusevm = { version = "0.14.20", features = ["jit", "jit-disk-cache", "aot"] }
```

| Feature | What it adds |
| --- | --- |
| `jit` | fusevm's Cranelift tiers — linear, block, tracing. |
| `jit-disk-cache` | Compiled native code persists to `~/.cache/fusevm-jit`, so codegen is not repaid on the next process. Relocate it with `FUSEVM_JIT_CACHE_DIR`, disable it with `FUSEVM_JIT_CACHE_DIR=off`. |
| `aot` | The closed-world compiler behind [`--aot`](#0x09-ahead-of-time-compilation). |

One call arms the tiers, in the same function that installs every other hook,
so the interpreter, the binary, a coroutine's VM and an ahead-of-time run all
get the same VM.

### What the tiers reach on Tcl today: nothing

Not an estimate. `tclrs --tiers` asks fusevm's own predicates
(`is_block_eligible`, `is_trace_eligible`, `trace_is_compiled`,
`block_jit_is_compiled`) after running the script.

A loop whose counter is an ordinary Tcl variable:

```
$ tclrs --tiers bench/counted_loop.tcl
ops                     20
block-JIT eligible      false
block-JIT compiled      false
largest eligible region none
loop @4                trace-eligible=false traced=false blacklisted=false
JIT-ineligible ops
  GetVar                3
  PrintLn               1
  SetVar                2
reaches native code     false
```

The arithmetic is not the problem — `tclrs --disasm` shows `NumLt`, `Add`,
`LoadInt`, `Jump` and `JumpIfFalse` with no extension op anywhere in the loop.
`GetVar` / `SetVar` are. A Tcl variable at a script's top level lowers to a VM
**global**, and fusevm's block tier accepts slots, not globals: `Op::GetVar` and
`Op::SetVar` are absent from `is_block_eligible_op_at`
(`fusevm-0.14.20/src/jit.rs:4249`), whole-chunk eligibility is the conjunction
of that predicate over every op (`:4419`), and the tracing tier defers to the
same predicate for everything but `Call` / `Return`
(`is_trace_op_allowed_at`, `:6180`).

**Inside a procedure that disqualification is gone, and it still does not
help.** A procedure's locals are frame slots, so the counter is
`GetSlot` / `SetSlot`, which both tiers accept — and the loop body is reported
trace-eligible:

```tcl
proc f {} {set i 0; while {$i < 3000000} {incr i}; return $i}
puts [f]
```

```
$ tclrs --tiers proc_loop.tcl
ops                     27
block-JIT eligible      false
block-JIT compiled      false
largest eligible region none
loop @5                trace-eligible=true traced=false blacklisted=false
JIT-ineligible ops
  Call                  1
  PrintLn               1
  ReturnValue           2
reaches native code     false
```

`trace-eligible=true`, `traced=false`: three million iterations, and no trace is
installed. The remaining blocker is the **shape of the loop**, not the ops in
it. fusevm's trace installer takes a do-while — a conditional backward branch
that closes the loop — and declines a while-do, a forward conditional exit
closed by an unconditional backward `Jump`. Tcl's `while` and `for` both lower
to the second shape. Building both shapes directly against fusevm 0.14.20, with
no Tcl involved, reproduces it: the do-while installs a compiled trace, the
while-do does not, and both are reported eligible.

The chunk as a whole stays block-ineligible for a third, separate reason — the
`Call` and the `puts` around the loop — so the whole-chunk tier is not an
alternative route either.

**Neither fix is in this crate's lowering, and neither is attempted here.** The
first is fusevm's trace installer accepting the while-do shape; the second is
slot-allocating a top-level Tcl variable whose name is known at compile time,
spilling to a global only where a script can reach a variable by computed name.
Both are changes to code this README can point at, not flags.

Until then, arming the JIT costs and returns nothing on Tcl — measured as the
gap between the `tclrs interp` and `tclrs JIT` rows of the
[benchmark table](#0x0a-benchmarks). Both are the same binary; the difference is
the recorder check in the dispatch loop and the once-per-run block-tier lookup.

### The disk cache

`jit-disk-cache` is enabled and `~/.cache/fusevm-jit` is live, but nothing
compiles for a Tcl script, so nothing is cached and no run is faster for it. It
is on now so it is already correct when either fix lands.

---

## [0x09] AHEAD-OF-TIME COMPILATION

`--aot` produces a standalone native executable with no parser and no compiler
inside it — the bytecode is baked in, already lowered. Whether the *dispatch
loop* is gone too depends on the script.

```sh
tclrs --aot hello hello.tcl              # emit + link
./hello                                  # runs; exit status is the script's
tclrs --aot-object hello.o hello.tcl     # just the relocatable object
```

The pipeline, all of it fusevm's except the first and last steps:

```
script → parser → compiler → fusevm::Chunk
                                 │
              fusevm::aot::compile_object → hello.o
                                 │  exports fusevm_aot_entry (native driver)
                                 │          fusevm_aot_chunk_blob / _len
                                 ▼
     cc main.c hello.o libtclrs.a → hello
                                 │  main.c calls fusevm_aot_run_embedded()
                                 ▼
     runtime: deserialize chunk → VM → fusevm_aot_register_builtins(vm)
                                     → native driver → exit code
```

`src/aot_runtime.rs` is this crate's whole contribution to the linked binary:
the `fusevm_aot_register_builtins` hook fusevm calls back into, which installs
the same hooks the interpreter installs. `crate-type = ["rlib", "staticlib"]`
in `Cargo.toml` is what produces the `libtclrs.a` it links against; set
`TCLRS_STATICLIB` to point the link somewhere else.

### What runs natively, and what does not

fusevm's ahead-of-time compiler lowers scalar arithmetic, comparisons, branches
and globals to registers, runs string / list / hash ops through a boxed shim,
and turns anything it has no lowering for into a **deopt point** that hands the
rest of the run to the interpreter. Every operation this frontend implements as
an extension op is such a point: `/`, `%`, `**`, `in` / `ni`, the `expr` result
normalizer, `eval`, all thirteen list commands, `foreach`, every `array` and
`dict` operation, and the whole `string` ensemble. A script that calls `expr`
once therefore runs its prologue natively and the remainder interpreted.

What AOT removes for such a script is the parse and the lowering, not the
dispatch loop — a small number, and the [benchmarks](#0x0a-benchmarks) measure
it as such. What it removes for a script with no extension op in its hot path is
the dispatch loop as well, and that number is not small: 8.1 ms against tclsh's
1136.0 for three million iterations.

### Semantics do not change, and that is tested

Every benchmark-shaped program is run both ways and compared byte for byte,
including the failing ones. That caught a real divergence: Tcl integers are
arbitrary-precision and this frontend has no bignum, so an `i64` overflow is an
error raised through the numeric hook — but native codegen wraps, and AOT
printed `-9223372036854775808` where the interpreter reported `integer value too
large to represent`. Every chunk now carries `int_overflow_deopt`, so
`Add` / `Sub` / `Mul` stay native registers on the common path and deopt into
the hook when a result does not fit. The same flag is why the JIT, armed on
every VM, cannot wrap either.

### Limitations

- **`catch` and coroutines are refused.** Both are driven from outside
  `VM::run` — the driver reads a cell an op parked, restores the VM and runs it
  again — and fusevm's ahead-of-time entry owns the run and never hands control
  back mid-way. Compiling one would turn a caught error into a fatal one, so
  `--aot` and `--aot-object` refuse the script instead.
- **One script, one binary.** The chunk is baked in at compile time. No `argv`,
  no reading a script at run time, no `source`.
- **Everything the frontend does through an extension op deopts** — see above.
- **macOS emits a linker warning**: `ld: warning: no platform load command found
  in …tclrs_aot_*.o, assuming: macOS`. The object cranelift-object writes
  carries no platform load command; the link and the binary are fine.
- **No cross-compilation.** `cranelift_native` targets the host.
- **The binary is large** — it links the whole runtime, Cranelift included,
  because `libtclrs.a` is one archive.

---

## [0x0A] BENCHMARKS

Reproduce from a fresh checkout:

```sh
bench/run.sh                            # every script in bench/
RUNS=10 WARMUP=3 bench/run.sh           # what the numbers below were taken with
bench/run.sh bench/counted_loop.tcl
```

`bench/run.sh` builds the release binary, compiles each script with `--aot`, and
runs four configurations of every script under
[hyperfine](https://github.com/sharkdp/hyperfine) — falling back to a warmed
`Time::HiRes` loop when hyperfine is not installed. Every row is wall clock of a
whole process, including startup, and every row runs through `env` so none of
them pays for an exec the others do not:

| Row | Command |
| --- | --- |
| `tclsh` | `env tclsh SCRIPT` |
| `tclrs interp` | `env TCLRS_JIT=off target/release/tclrs SCRIPT` |
| `tclrs JIT` | `env TCLRS_JIT=on target/release/tclrs SCRIPT` |
| `tclrs AOT` | `env target/bench/NAME` — built by `tclrs --aot` |

### Measured

Apple M1 Max, macOS 26.5.1, rustc 1.97.1, `--release` (`lto = true`,
`codegen-units = 1`), tclsh 9.0.4 from `/usr/local/bin`, 10 runs after 3 warmup
runs, load average 2.3 at the start of the run. Mean ± σ in milliseconds,
copied from the `target/bench/*.md` hyperfine exports that run produced.

| Benchmark | tclsh 9.0.4 | tclrs interp | tclrs JIT | tclrs AOT |
| --- | ---: | ---: | ---: | ---: |
| `startup` — the empty script | 50.2 ± 2.2 | 6.2 ± 0.4 | **5.4 ± 0.3** | 6.0 ± 0.8 |
| `counted_loop` — 3M × `incr` | 1136.0 ± 46.2 | 244.2 ± 1.5 | 324.7 ± 2.0 | **8.1 ± 0.5** |
| `counted_loop_expr` — 3M × `set i [expr {$i + 1}]` | 1273.2 ± 19.5 | **274.8 ± 2.0** | 357.6 ± 2.5 | 341.5 ± 1.9 |
| `integer_arith` — 1M × `$sum + $i * $i - ($i >> 3)` | 783.8 ± 23.7 | **197.6 ± 1.4** | 224.4 ± 1.8 | 221.5 ± 1.6 |
| `string_build` — 100k × `set s "$s$i"` | 876.7 ± 7.0 | 556.7 ± 21.7 | **556.1 ± 16.9** | 558.0 ± 30.8 |
| `list_iterate` — 5k × `lappend`, then `foreach` | **57.7 ± 1.1** | 810.6 ± 7.6 | 807.7 ± 5.1 | 972.3 ± 171.2 |

Every ratio below is that table's means divided; nothing else is inferred.

**Where tclrs wins.** Interpreted, tclrs is 4.7× tclsh on the counted loop, 4.6×
on the same loop written with `expr`, 4.0× on integer arithmetic, 1.6× on string
building, and starts in 6.2 ms against tclsh's 50.2. Ahead-of-time compiled, the
counted loop is **140× tclsh and 30× tclrs interpreted**: 8.1 ms for the whole
process, of which 6.0 is the binary's own startup — about 2 ms for 3,000,000
iterations. No dispatch loop runs at that rate. That script contains no
extension op in its loop, so fusevm's ahead-of-time compiler lowers all of it,
counter included, to native registers.

**Where the AOT win disappears.** `counted_loop_expr` is the same loop with the
increment written as `expr` instead of `incr`. That adds one op — the extension
op `expr` emits to normalize its result — and AOT goes from 8.1 ms to 341.5 ms,
1.24× *slower* than interpreting, because the native path deopts there and hands
the rest of the run over. Same story for `integer_arith` (1.12× slower than
interpreting) and `string_build` (within noise). On `list_iterate` AOT is 1.2×
slower still, paying for the embedded chunk's deserialization and the deopt on a
script that never gets to run natively; hyperfine flagged statistical outliers
on that row, so treat its σ as a floor rather than a measurement.

**Where tclrs loses.** `list_iterate` — 14× slower than tclsh interpreted, 17×
ahead-of-time compiled. tclsh keeps a list as an object and `lappend` appends to
it in amortized constant time; tclrs stores the list as its string
representation and re-derives it on every `lappend`, which is quadratic.
Building a 5,000-element list takes tclsh 58 ms and tclrs 811. This is a
data-representation problem in the list commands, not a code-generation one, and
no JIT tier would fix it.

**What the JIT costs.** The `tclrs JIT` column is the same binary as `tclrs
interp` with the tracing JIT armed. It compiles nothing (see
[JIT Compilation](#0x08-jit-compilation)) and it is not free: 33% slower on
`counted_loop`, 30% on `counted_loop_expr`, 14% on `integer_arith`, and within
noise on the two benchmarks whose time goes elsewhere. That is the recorder
check in the dispatch loop and the block-tier lookup, paid on every run for a
tier that never fires. It stays on because that is the price of the tier being
armed for when the trace shape is accepted, and hiding it would make the table
dishonest.

Caveats worth knowing before quoting any of this: the machine was not idle (load
average 2.3, a shared workstation); `startup` at ~6 ms is close to hyperfine's
calibration floor and it warns as much; and the AOT rows run with the JIT armed
too, since the ahead-of-time runtime hook goes through the same install point.

---

## [0x0B] CONFORMANCE

The differential suites test what tclrs claims to do. `conformance/` measures the
opposite: how much of *real Tcl* it does, by running the Tcl project's own test
suite against it.

**1404 of 2941 attempted cases pass — 47.7%.** Over every case the suite
contains, including the ones that cannot be run here, that is 1404 of 69424.
[`conformance/REPORT.md`](conformance/REPORT.md) has the breakdown behind the
number: attempted, passed, failed and skipped per suite file, why each skipped
case could not be run, and the failure causes ranked.

Regenerate it:

```sh
conformance/run.sh
```

That fetches the Tcl source release — the suite ships there, not in a binary
install — verifies it against a pinned SHA-256, lifts every case out of every
`tests/*.test` file using tcltest's own argument parsing, runs each one under
both `tclsh` and tclrs, and rewrites the report. No file and no case is chosen
by hand, and the runner has no option to run a subset. A case passes only when
the two runs agree on the whole triple of exit code, result string and stdout,
byte for byte; the suite's own `-result` values are not consulted, because
tclsh is the specification and comparing against what it actually does is
stricter than comparing against what the suite says it should.

The report checked in was produced before `proc`, the string ensemble,
coroutines and `eval` landed, so it understates the current tree — its
skip table still attributes thousands of cases to a missing `proc`. Rerun it
before quoting the number as current.

---

## [0x0C] TESTING

```sh
cargo test
```

Every suite compares against the reference interpreter rather than against
hand-written expectations: each program is executed by both `tclsh` and tclrs
and the outputs compared byte for byte. The suites cover the twelve parse rules,
word splitting character for character, whole programs, the list commands, the
associative commands, the string ensemble, procedures and control flow,
coroutines, the interpreter's state across evaluations, the binary's stdout /
stderr / exit status in each of its input modes, and the ahead-of-time path
against the interpreter.

Several of them generate their cases rather than listing them: every awkward
element value driven through every list command, `foreach` through every shape
its grammar allows, the glob matcher over a pattern × subject grid, and every
index form against lists of every length — each matrix run as one script and
compared line for line.

The differential suites skip when no `tclsh` is on `PATH`. The full
ahead-of-time link test skips when `libtclrs.a` has not been built or there is
no `cc`.

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).
