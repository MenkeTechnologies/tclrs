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
- [\[0x06\] Status & Roadmap](#0x06-status--roadmap)
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

---

## [0x01] BUILD

```sh
git clone https://github.com/MenkeTechnologies/tclrs
cd tclrs
cargo build
cargo test
```

Requires a stable Rust toolchain. The differential tests invoke `tclsh` from
`PATH` and report a skip when none is installed, so the suite still runs on a
machine without Tcl.

---

## [0x02] USAGE

tclrs is a library in this phase — the `tclsh`-compatible binary arrives with
the command set that would make it useful (phase 4). `tclrs::eval` compiles and
runs a script, returning its value and everything it wrote to stdout:

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

### Coroutines

`coroutine name command ?arg ...?`, `yield ?value?`, `yieldto`, `info
coroutine`, and the lifecycle of the context command a coroutine creates:
calling it resumes the body, the body returning deletes it, and calling it after
that is `invalid command name`.

A coroutine is a second `fusevm::VM` over the same chunk — the arrangement
`fusevm`'s scheduler uses for a goroutine — positioned at the body's sub entry
with the actual arguments below a frame that returns past the end of the
program. `yield` stashes a request and calls `VM::request_halt`, exactly as a
channel op stashes a `SchedReq`; the driver reads it after `run()` returns,
pushes the yielded value onto the resumer's stack and runs the resumer. Because
the op has already advanced `ip`, resuming is just a value on the stack and
another `run()`. Tcl needs two things the Go model does not have: control
transfer names its successor rather than taking the next goroutine off a queue
(so each context records the resumer to return to, and `yieldto` donates it to
the target), and every context shares one global variable table (so the driver
owns it and moves it into whichever VM runs — one at a time, so no copy).

```tcl
proc allNumbers {} {
    yield
    set i 0
    while 1 {
        yield $i
        incr i 2
    }
}
coroutine nextNumber allNumbers
for {set i 0} {$i < 10} {incr i} {
    puts "received [nextNumber]"
}
```

The name a `coroutine` command creates has to be known to every call site, since
this frontend resolves a command to bytecode rather than to a runtime command
table. So the name and the body command are literals, the body is a procedure
the script defines, and the `coroutine` command itself appears at the top level
of the script or in a command substitution in one — the positions the prescan
that collects those names reaches. `yieldto` cedes control to a coroutine of the
script; ceding it to an arbitrary command would have to evaluate that command in
the resumer's context, which this frontend cannot do, so it is refused. `info`
has one subcommand, `coroutine`; `coroprobe`, `coroinject` and deleting a
coroutine by renaming its command are not implemented.

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
| **One driver, two jobs** | An op that cannot finish on its own stashes something in a cell and halts; the driver reads the cell after `run()` returns. `catch` uses it to resume the same VM at a handler, a coroutine uses it to run a different VM — the pattern `fusevm`'s scheduler is built on, and the only runtime unwinding in the crate. |

Static stack tracking is what keeps the lowering cheap: each command leaves its
result on the stack and the compiler tracks that depth as it goes, so `break`
and `continue` unwind with a known number of pops rather than a runtime
unwinder.

---

## [0x06] STATUS & ROADMAP

**Phase 2 of 7.** Scripts compile to `fusevm` bytecode and run. The crate is a
library: there is no `tclsh` binary, no LSP, no DAP, and no JIT yet — `fusevm`
is pulled with its default features, so the VM's interpreter tier executes the
chunk and no `cranelift-*` crate is linked. The JIT features arrive in phase 6,
with the benchmarks that justify them.

| Phase | Contents | State |
| --- | --- | --- |
| 1 | Parser — the twelve rules of `Tcl(n)` | done |
| 2 | Compiler + runtime — `set` / `puts` / `expr` / `incr` / `if` / `while` / `break` / `continue` | done |
| 3 | Lists — list parsing and quoting, the thirteen list commands, `foreach`, `in` / `ni` | done, except `{*}` expansion |
| 4 | `proc`, `return`, `upvar` / `global`, arrays, the `tclsh` binary | planned |
| 5 | The command library — `string`, `regexp`, `switch`, `for`, `catch` / `error`, file and channel IO | planned |
| 6 | `fusevm` `jit` / `jit-disk-cache` / `aot` features, bignum, benchmarks | planned |
| 7 | Toolchain parity with the sibling frontends — LSP, DAP, zsh completion, man pages, `reference.html`, inline `rust {}` FFI, `--dump-tokens` / `--dump-ast` / `--disasm` | planned |

### Differential test harness

Five suites, all comparing against the reference interpreter rather than
against hand-written expectations:

```sh
cargo test --test dodekalogue            # the twelve parse rules
cargo test --test differential_tclsh     # word splitting vs tclsh, character for character
cargo test --test execution_differential # whole programs vs tclsh, byte for byte
cargo test --test list_differential      # the list commands, plus generated matrices
cargo test --test coroutine_differential # coroutines: generators, transfers, lifecycle
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
