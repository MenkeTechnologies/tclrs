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
![status](https://img.shields.io/badge/status-phase%202%20%C2%B7%20in%20development-9b5de5?style=flat-square)

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
- [\[0x02\] Usage](#0x02-usage)
- [\[0x03\] Language Surface](#0x03-language-surface)
- [\[0x04\] The Parser](#0x04-the-parser)
- [\[0x05\] Architecture](#0x05-architecture)
- [\[0x06\] JIT Compilation](#0x06-jit-compilation)
- [\[0x07\] Ahead-of-Time Compilation](#0x07-ahead-of-time-compilation)
- [\[0x08\] Benchmarks](#0x08-benchmarks)
- [\[0x09\] Status & Roadmap](#0x09-status--roadmap)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

Tcl 9 evaluates through a bytecode engine wrapped around a dual-representation
object model, re-deriving string representations as values cross command
boundaries. tclrs takes a different path: it parses a script once — resolving
every substitution the grammar permits at parse time — and lowers each command
to `fusevm` bytecode, the same bytecode sixteen other language frontends emit.
Highlights:

- **Compiled, not re-parsed** — a braced body is fully known at parse time, so
  `if` / `while` bodies and braced `expr` expressions compile once into bytecode
  instead of being re-parsed on every evaluation. Words carry a `braced` flag
  for exactly this decision.
- **fusevm-hosted** — no local `vm.rs` / `jit.rs`, no bespoke object heap. Tcl
  strings, integers and floats map onto `fusevm::Value` directly; a value
  produced as a number stays a number in a VM slot and only acquires a string
  representation when something asks for one.
- **Native arithmetic** — `+ - *`, the comparisons, the bitwise and shift
  operators, and the short-circuiting `&&` / `||` lower to native fusevm ops.
  Only the operators whose Tcl meaning differs from the VM's generic one — `/`,
  `%`, `**` — take a frontend extension op, and only operands the VM cannot
  compute on natively (mostly strings) take the numeric hook.
- **Differentially tested** — every program in the suite is executed by both
  `tclsh` and tclrs and the output compared byte for byte. No expected output in
  this repository is written by hand.
- **Compiled ahead of time** — `tclrs --aot script.tcl -o …` lowers a script
  through fusevm's closed-world compiler to a native object and links it into a
  standalone executable with no parser and no bytecode dispatch loop inside it.
- **JIT armed, and honest about it** — every VM this crate builds enables
  fusevm's three Cranelift tiers, and `tclrs --tiers` reports which of them a
  given script actually reaches. Today that answer is *none*, for a reason this
  README names precisely: see [JIT Compilation](#0x06-jit-compilation).

---

## [0x01] BUILD

```sh
git clone https://github.com/MenkeTechnologies/tclrs
cd tclrs
cargo build
cargo test
```

Requires a stable Rust toolchain and, for `--aot`, a C compiler to link with.
The differential tests invoke `tclsh` from `PATH` and report a skip when none
is installed, so the suite still runs on a machine without Tcl.

`cargo build` produces three artifacts: the `tclrs` binary, the `tclrs` rlib,
and `libtclrs.a` — the staticlib an AOT object links against.

---

## [0x02] USAGE

```sh
tclrs script.tcl            # run a script
tclrs -c 'puts [expr 6*7]'  # run a script given on the command line
tclrs --aot out script.tcl  # compile to a standalone native executable
tclrs --tiers script.tcl    # which fusevm execution tiers the script reaches
tclrs --disasm script.tcl   # the compiled fusevm bytecode
```

The binary runs a script and exits. It is **not** a `tclsh` replacement: there
is no REPL, no `argv` / `argc` / `env`, no `info`, and no `-encoding` handling —
and the language surface is the one in
[Language Surface](#0x03-language-surface), not all of Tcl.

As a library, `tclrs::eval` compiles and runs a script, returning its value and
everything it wrote to stdout:

```rust
let out = tclrs::eval("set x 5\nputs [expr {$x * 2}]").unwrap();
assert_eq!(out.output, "10\n");
```

`tclrs::parse` returns the parsed `Script` without running it, for tooling that
wants the word structure.

---

## [0x03] LANGUAGE SURFACE

Working commands: `set`, `puts` (with `-nonewline`), `expr`, `incr`, `if` /
`elseif` / `else`, `while`, `foreach`, `break`, `continue`, the list commands —
`list`, `llength`, `lindex`, `lappend`, `lrange`, `lreverse`, `linsert`,
`lreplace`, `lsearch`, `lsort`, `join`, `split`, `concat` — and command
substitution of any of them.

`expr` covers the whole operator set of `expr(n)`:

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

Not built yet, and **refused rather than approximated**: `proc` and every
command outside the list above, arrays, `{*}` expansion, math functions,
variable and body words that are not literal, and arbitrary-precision integers —
an operation that overflows `i64` is an error instead of silently wrapping.
Refusal is at compile time where the script's shape decides it and at run time
where a value does. See [`BUGS.md`](BUGS.md) for the ledger.

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

Options that exist in tclsh but are not built here — `lsearch -regexp`,
`-sorted`, `-dictionary`, `-nocase`, `-index`, `-stride`, `-subindices`,
`-bisect`, and `lsort -command`, `-dictionary`, `-index`, `-nocase`, `-stride` —
are errors, never silent no-ops.

---

## [0x04] THE PARSER

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

## [0x05] ARCHITECTURE

tclrs contains no virtual machine, no interpreter loop, and no code generator.
The execution path mirrors how `zshrs` hosts zsh and `groovyrs` hosts Groovy:

```
Tcl script → parser (Script/Command/Word) → lower to fusevm bytecode → fusevm VM
                                                   │
                                    numeric hook (string operands, overflow)
                                    extension ops (/ % ** floored + integral, normalize)
```

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`. Each command lowers into a `fusevm::Chunk` and runs on the shared VM. |
| **Numeric hook** | Catches operands the VM cannot compute on natively. An operand that parses as a number is one (including the `0x` / `0o` / `0b` radix prefixes); comparisons fall back to string order when it does not; arithmetic on a non-number is an error. |
| **Extension ops** | `/` and `%` floor toward negative infinity (`-57 / 10` is `-6`, `-57 % 10` is `3`), `**` stays integral for integral operands, and a normalize op converts a VM-native result into its Tcl value — booleans to `1`/`0`, doubles to Tcl's double format. |
| **No object heap** | Tcl's value model needs none on top of fusevm's: strings, integers and floats map onto `Value` directly. |

Static stack tracking is what keeps the lowering cheap: each command leaves its
result on the stack and the compiler tracks that depth as it goes, so `break`
and `continue` unwind with a known number of pops rather than a runtime
unwinder.

---

## [0x06] JIT COMPILATION

### How it is turned on

`fusevm` is pulled with the Cranelift features, so `cargo build` links the JIT
and the persistent native-code cache:

```toml
fusevm = { version = "0.14.12", features = ["jit", "jit-disk-cache", "aot"] }
```

| Feature | What it adds |
| --- | --- |
| `jit` | fusevm's three Cranelift tiers — linear, block, tracing. |
| `jit-disk-cache` | Compiled native code persists to `~/.cache/fusevm-jit`, so codegen is not repaid on the next process. Relocate it with `FUSEVM_JIT_CACHE_DIR`, disable it with `FUSEVM_JIT_CACHE_DIR=off`. |
| `aot` | The closed-world compiler behind [`--aot`](#0x07-ahead-of-time-compilation). |

One call arms the tiers, in `runtime::install_hooks` — the function every driver
goes through, so the interpreter, `tclrs script.tcl` and an AOT binary all get
the same VM:

```rust
vm.set_numeric_hook(Arc::new(numeric));
vm.set_extension_handler(Box::new(…));
vm.enable_tracing_jit();          // block tier + trace recorder, per fusevm's phase-10 dispatch
```

`TCLRS_JIT=off` skips that last call. It exists so the benchmark can measure the
interpreter and the JIT-armed VM as separate rows of the same binary.

fusevm's own warmup knobs work unchanged: `FUSEVM_JIT_BLOCK_THRESHOLD`,
`FUSEVM_JIT_TRACE_THRESHOLD`.

### What the tiers reach on Tcl today: nothing

Not an estimate — `tclrs --tiers` asks fusevm's own predicates
(`is_block_eligible`, `is_trace_eligible`, `trace_is_compiled`,
`block_jit_is_compiled`) after running the script:

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

The arithmetic is not the problem. `tclrs --disasm` on that loop shows exactly
the native ops the frontend advertises — `NumLt`, `Add`, `LoadInt`, `Jump`,
`JumpIfFalse`, with no extension op anywhere in the loop:

```
0004     4     GetVar(0)          ← loop header
0005     4     LoadInt(3000000)
0006     4     NumLt
0007     4     JumpIfFalse(15)
0008     2     GetVar(0)
0009     2     LoadInt(1)
0010     2     Add
0011     2     Dup
0012     2     SetVar(0)
0013     2     Pop
0014     2     Jump(4)            ← backward branch, the trace anchor
```

The problem is `GetVar` / `SetVar`. A Tcl variable lowers to a VM **global**,
and fusevm's block tier accepts slots, not globals: `Op::GetVar` and
`Op::SetVar` are absent from `is_block_eligible_op_at`
(`fusevm-0.14.20/src/jit.rs:4249`), block eligibility is the conjunction of that
predicate over every op in the chunk (`:4408`), and the tracing tier defers to
the same predicate for everything but `Call`/`Return` (`is_trace_op_allowed_at`,
`:6180`). So a chunk with one variable in it is refused whole by the block
tier, and a recorded loop body containing one is refused as a trace. Every Tcl
loop has a variable in it.

`tests` pins both halves of that, so it cannot quietly change:
`tiers::tests::a_slot_counter_loop_is_accepted_by_both_tiers` builds the same
counter loop against fusevm slots and asserts both tiers take it;
`tiers::tests::the_tcl_counter_loop_reaches_no_tier` asserts the Tcl spelling
reaches neither. The report is not hardwired to say no — it says no *here*.

**The fix is in the compiler, and is not done in this branch:** lower a Tcl
variable whose name is known at compile time to a fusevm slot (`GetSlot` /
`SetSlot`) instead of a named global, spilling to a global only where a script
can reach a variable by computed name (`set $name …`, `upvar`, `global`).
That is a change to `compiler.rs` and the variable model, not a flag.

Until then, `enable_tracing_jit` costs and returns nothing on Tcl — measured at
the bottom of the [benchmark table](#0x08-benchmarks) as the gap between the
`tclrs interp` and `tclrs JIT` rows. Both are the same binary; the difference is
the recorder check in the dispatch loop and the once-per-run block-tier lookup.

### The disk cache

`jit-disk-cache` is enabled and `~/.cache/fusevm-jit` is live, but nothing
compiles, so nothing is cached and no run is faster for it. It is on now so it
is already correct when the slot lowering lands.

---

## [0x07] AHEAD-OF-TIME COMPILATION

`--aot` produces a standalone native executable with no parser and no compiler
inside it — the bytecode is baked in, already lowered. Whether the *dispatch
loop* is gone too depends on the script; see below.

```sh
tclrs --aot hello hello.tcl   # emit + link
./hello                       # runs, exit status is the script's
tclrs --aot-object hello.o hello.tcl   # just the relocatable object
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
the `fusevm_aot_register_builtins` hook fusevm calls back into, installing the
same numeric hook and extension dispatch the interpreter uses. `crate-type =
["rlib", "staticlib"]` in `Cargo.toml` is what produces the `libtclrs.a` it
links against; set `TCLRS_STATICLIB` to point the link somewhere else.

### What runs natively, and what does not

fusevm's AOT compiler lowers scalar arithmetic, comparisons, branches and
globals to registers, runs string/list/hash ops through a boxed shim, and turns
anything it has no lowering for into a **deopt point** that hands the rest of
the run to the interpreter. Every operation this frontend implements as an
extension op — `/`, `%`, `**`, `in`/`ni`, the `expr` result normalizer, all
thirteen list commands, `foreach`, and every `array` / `dict` operation — is
such an op. A script that calls `expr` once therefore runs its prologue natively
and the remainder interpreted.

What AOT removes for such a script is the parse and the lowering, not the
dispatch loop. The [benchmarks](#0x08-benchmarks) measure that, and it is a
small number.

### Semantics must not change, and are tested

`tests/aot_differential.rs` runs every program both ways and compares byte for
byte, including the failing ones. It caught a real divergence: Tcl integers are
arbitrary-precision and this frontend has no bignum, so an `i64` overflow is an
error raised through the numeric hook — but native codegen wraps, and AOT
printed `-9223372036854775808` where the interpreter reported `integer value too
large to represent`. `runtime::compile` now sets `chunk.int_overflow_deopt`, so
`Add`/`Sub`/`Mul` stay native registers on the common path and deopt into the
hook when a result does not fit.

The test drives `fusevm::aot::run_chunk_native` — the same codegen, through
Cranelift's in-memory module — so it needs no C toolchain and runs in CI; the
full link path is a separate test that skips when `libtclrs.a` or `cc` is
missing.

### Limitations

- **One script, one binary.** The chunk is baked in at compile time. No `argv`,
  no reading a script at run time, no `source`.
- **Everything the frontend does through an extension op deopts** — see above.
  AOT is not a way around the missing slot lowering.
- **macOS emits a linker warning** — `ld: warning: no platform load command
  found in …tclrs_aot_*.o, assuming: macOS`. The object cranelift-object writes
  carries no platform load command; the link and the binary are fine.
- **No cross-compilation.** `cranelift_native` targets the host.
- **The binary is large** — it links the whole runtime, Cranelift included,
  because `libtclrs.a` is one archive.

---

## [0x08] BENCHMARKS

Reproduce from a fresh checkout:

```sh
bench/run.sh                       # every script in bench/
RUNS=30 WARMUP=5 bench/run.sh      # what the numbers below were taken with
bench/run.sh bench/counted_loop.tcl
```

`bench/run.sh` builds the release binary, compiles each script with `--aot`,
and runs four configurations of every script under
[hyperfine](https://github.com/sharkdp/hyperfine) — falling back to a warmed
`Time::HiRes` loop when hyperfine is not installed. Every row is wall clock of
a whole process, including startup, and every row runs through `env` so none of
them pays for an exec the others do not:

| Row | Command |
| --- | --- |
| `tclsh` | `env tclsh SCRIPT` |
| `tclrs interp` | `env TCLRS_JIT=off target/release/tclrs SCRIPT` |
| `tclrs JIT` | `env TCLRS_JIT=on target/release/tclrs SCRIPT` |
| `tclrs AOT` | `env target/bench/NAME` — built by `tclrs --aot` |

### Measured

Apple M1 Max, macOS 26.5.1, rustc 1.97.1, `--release` (`lto = true`,
`codegen-units = 1`), tclsh 9.0.4 from `/usr/local/bin`, 30 runs after 5 warmup
runs, load average 4.1 at the start of the run. Mean ± σ in milliseconds,
copied from the `target/bench/*.md` hyperfine exports that run produced.

| Benchmark | tclsh 9.0.4 | tclrs interp | tclrs JIT | tclrs AOT |
| --- | ---: | ---: | ---: | ---: |
| `startup` — the empty script | 54.6 ± 0.8 | 5.5 ± 0.5 | 5.5 ± 0.5 | 6.3 ± 0.5 |
| `counted_loop` — 3M × `incr` | 1159.1 ± 64.7 | 244.2 ± 1.4 | 332.5 ± 1.8 | **8.1 ± 0.4** |
| `counted_loop_expr` — 3M × `set i [expr {$i + 1}]` | 1329.4 ± 26.8 | **275.5 ± 1.6** | 363.3 ± 4.3 | 349.2 ± 1.6 |
| `integer_arith` — 1M × `$sum + $i * $i - ($i >> 3)` | 812.1 ± 38.4 | **199.6 ± 1.8** | 230.0 ± 1.7 | 231.1 ± 2.1 |
| `string_build` — 100k × `set s "$s$i"` | 911.9 ± 17.6 | 602.8 ± 41.5 | 596.9 ± 13.7 | **595.3 ± 12.2** |
| `list_iterate` — 5k × `lappend`, then `foreach` | **58.9 ± 1.0** | 847.6 ± 3.8 | 852.2 ± 14.3 | 946.9 ± 3.7 |

Ratios below are that table's means divided; nothing else is inferred.

**Where tclrs wins.** Interpreted, tclrs is 4.8× tclsh on the counted loop,
4.1× on integer arithmetic, 1.5× on string building, and starts in 5.5 ms
against tclsh's 54.6. AOT-compiled, the counted loop is **143× tclsh and 30×
tclrs interpreted**: 8.1 ms for the whole process, of which 6.3 is the AOT
binary's own startup — about 1.8 ms, or 0.6 ns per iteration, for 3,000,000
iterations. No dispatch loop runs at that rate. The script contains no extension
op (see the disassembly above), so fusevm's AOT compiler lowers all of it,
counter included, to native registers.

**Where the AOT win disappears.** `counted_loop_expr` is the same loop with the
increment written as `expr` instead of `incr`. That adds one op — the extension
op `expr` emits to normalize its result — and AOT drops from 8.1 ms to 349.2 ms,
level with the interpreter, because the native path deopts there and hands the
rest of the run over. Same story for `integer_arith` and `string_build`: AOT
matches the interpreter and beats nothing. On `list_iterate` AOT is 12% *slower*
than interpreting, paying for the embedded chunk's deserialization and the
deopt on a script that never gets to run natively.

**Where tclrs loses.** `list_iterate` — 14× slower than tclsh interpreted, 16×
AOT. tclsh keeps a list as an object and `lappend` appends to it in amortized
constant time; tclrs stores the list as its string representation and
re-derives it on every `lappend`, which is quadratic. Building a 5,000-element
list takes tclsh 59 ms and tclrs 850. This is a data-representation problem in
`cmd_list.rs`, not a code-generation one, and no JIT tier would fix it.

**What the JIT costs.** The `tclrs JIT` column is the same binary as `tclrs
interp` with `enable_tracing_jit` called. It compiles nothing (see [JIT
Compilation](#0x06-jit-compilation)) and it is not free: 36% slower on
`counted_loop`, 32% on `counted_loop_expr`, 15% on `integer_arith`, and within
noise on the two benchmarks whose time goes elsewhere. That is the recorder
check in the dispatch loop and the block-tier lookup, paid on every run for a
tier that never fires. It stays on because the cost is the price of the tier
being armed for when the slot lowering lands, and hiding it would make the
table dishonest.

Caveats worth knowing before quoting any of this: the machine was not idle
(load average 4.1, a shared workstation); `startup` at ~5 ms is close to
hyperfine's calibration floor and it warns as much; and the AOT rows run with
the JIT armed too, since the AOT runtime hook goes through the same
`install_hooks`.

---

## [0x09] STATUS & ROADMAP

Scripts compile to `fusevm` bytecode and run, either through the VM or as an
AOT-compiled native binary. There is a script-running `tclrs` binary but no
`tclsh` replacement, no REPL, no LSP and no DAP. `fusevm` is pulled with `jit`,
`jit-disk-cache` and `aot`, so all three Cranelift tiers are linked and armed —
and, on Tcl as it lowers today, none of them ever compiles anything: see [JIT
Compilation](#0x06-jit-compilation) for the measurement and the reason.

| Phase | Contents | State |
| --- | --- | --- |
| 1 | Parser — the twelve rules of `Tcl(n)` | done |
| 2 | Compiler + runtime — `set` / `puts` / `expr` / `incr` / `if` / `while` / `break` / `continue` | done |
| 3 | Lists — list parsing and quoting, the thirteen list commands, `foreach`, `in` / `ni` | done, except `{*}` expansion |
| 4 | `proc`, `return`, `upvar` / `global`, arrays, the `tclsh` binary | planned |
| 5 | The command library — `string`, `regexp`, `switch`, `for`, `catch` / `error`, file and channel IO | planned |
| 6 | `fusevm` `jit` / `jit-disk-cache` / `aot` features, bignum, benchmarks | features on, AOT works, benchmarks measured; the JIT reaches no Tcl code until variables lower to slots, and there is still no bignum |
| 7 | Toolchain parity with the sibling frontends — LSP, DAP, zsh completion, man pages, `reference.html`, inline `rust {}` FFI, `--dump-tokens` / `--dump-ast` / `--disasm` | planned |

### Differential test harness

Every suite compares against a reference rather than against hand-written
expectations — the reference interpreter for the language, and the tclrs
interpreter itself for the AOT compiler:

```sh
cargo test --test dodekalogue            # the twelve parse rules
cargo test --test differential_tclsh     # word splitting vs tclsh, character for character
cargo test --test execution_differential # whole programs vs tclsh, byte for byte
cargo test --test list_differential      # the list commands, plus generated matrices
cargo test --test array_differential     # array variables, `array` and `dict`, vs tclsh
cargo test --test aot_differential       # AOT-compiled output vs the interpreter's
```

`list_differential` also generates its cases: every awkward element value is
driven through every list command, `foreach` through every shape its grammar
allows, the glob matcher over a pattern × subject grid, and every index form
against lists of every length — each matrix run as one script and compared line
for line.

The differential suites invoke `tclsh` (or `tclsh9.0` / `tclsh8.6`) from `PATH`
and skip when none is installed.

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).
