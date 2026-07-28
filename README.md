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
![status](https://img.shields.io/badge/status-phase%204%20%C2%B7%20in%20development-9b5de5?style=flat-square)

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

```sh
tclrs FILE ?arg ...?   # run a script file
tclrs -c SCRIPT        # run SCRIPT
tclrs                  # read a script from stdin; a REPL when stdin is a terminal
tclrs --version
```

The binary prints what the script prints and nothing else: no banner, no
version stripe, no prompt when stdin is not a terminal. Errors go to stderr.
`argv0`, `argc` and `argv` are set as `tclsh` sets them.

`tclsh` decides what each mode does, and the modes differ:

| Mode | Reads | On a failure | Exit status |
| --- | --- | --- | --- |
| `tclrs FILE` | the file, as one script | stops | 1, or 0 |
| `tclrs -c SCRIPT` | the argument, as one script | stops | 1, or 0 |
| `tclrs < file`, `… \| tclrs` | one command at a time | reports it and runs the next | 0 |
| `tclrs` on a terminal | one command at a time, prompting `% ` | reports it and prompts again | 0 |

`-c` and `--version` are the two things with no `tclsh` equivalent — `tclsh`
reads stdin for any argument beginning with `-`. An option tclrs does not
recognize is refused rather than quietly turned into something else.

### The REPL

A command may span lines. The loop keeps reading while the text so far leaves a
brace, quote or bracket open, and evaluates when it closes — so a `{` at the end
of a line continues rather than fails, and a `}` that closes nothing is
evaluated and its error reported instead of waiting for input that cannot help.
The value of each command is echoed unless it is empty. Variables, arrays and
dicts persist from one command to the next. Text left unfinished at end of input
is discarded, as `tclsh` discards it.

### As a library

`tclrs::Interp` is the interpreter object: state that outlives one evaluation.

```rust
let mut interp = tclrs::Interp::capturing();
interp.eval("set x 5").unwrap();
assert_eq!(interp.eval("expr {$x * 2}").unwrap(), "10");
assert_eq!(interp.take_output(), "");
```

`tclrs::eval` is the one-shot form, building an interpreter and discarding it:

```rust
let out = tclrs::eval("set x 5\nputs [expr {$x * 2}]").unwrap();
assert_eq!(out.output, "10\n");
```

`tclrs::parse` returns the parsed `Script` without running it, for tooling that
wants the word structure.

---

## [0x03] LANGUAGE SURFACE

Working commands: `set`, `unset`, `puts` (with `-nonewline`), `expr`, `incr`,
`eval`, `if` / `elseif` / `else`, `while`, `foreach`, `break`, `continue`, the
list commands — `list`, `llength`, `lindex`, `lappend`, `lrange`, `lreverse`,
`linsert`, `lreplace`, `lsearch`, `lsort`, `join`, `split`, `concat` —
`array` and `dict`, and command substitution of any of them.

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
command outside the list above, `{*}` expansion, math functions, variable and
body words that are not literal outside `eval`, and arbitrary-precision
integers — an operation that overflows `i64` is an error instead of silently
wrapping. Refusal is at compile time where the script's shape decides it and at
run time where a value does. See [`BUGS.md`](BUGS.md) for the ledger.

### Scripts that are values

Every other command's script is braced text the compiler lowers where it stands.
`eval`'s is a value — `eval $cmd` cannot be lowered until `$cmd` has one — so it
is the one command that compiles while running.

| Piece | How |
| --- | --- |
| **The op** | `eval` lowers its arguments as ordinary words and emits one extension op. The handler concatenates them the way `concat` does (one argument is used as it stands), compiles the result and runs it. |
| **The cache** | Keyed by the source text and nothing else: nothing outside the text changes what it lowers to, since the compiler reads no interpreter state and binds variables to slots by name. `eval {incr i}` in a loop is lowered on the first pass and reused on every later one. |
| **The state** | One set of variables, not two. A chunk interns its own name table, so slots cannot be carried between chunks; the interpreter's name-keyed store is the authority and a chunk's slots are a projection of it, written on entry and read back on exit — in both directions across an `eval`, so a nested script sees what the outer one set and the outer one sees what the nested one set at its very next command. |
| **The depth** | A nested script runs on a VM of its own, so nesting costs native stack. It is refused past `interp recursionlimit`'s default of 1000 with the reference interpreter's own message, at the same depth, rather than being allowed to overflow the stack. |

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

## [0x06] STATUS & ROADMAP

**Phase 4 of 7, in progress.** Scripts compile to `fusevm` bytecode and run,
and there is a binary to run them with. No LSP, no DAP, and no JIT yet —
`fusevm` is pulled with its default features, so the VM's interpreter tier
executes the chunk and no `cranelift-*` crate is linked. The JIT features arrive
in phase 6, with the benchmarks that justify them.

| Phase | Contents | State |
| --- | --- | --- |
| 1 | Parser — the twelve rules of `Tcl(n)` | done |
| 2 | Compiler + runtime — `set` / `puts` / `expr` / `incr` / `if` / `while` / `break` / `continue` | done |
| 3 | Lists — list parsing and quoting, the thirteen list commands, `foreach`, `in` / `ni` | done, except `{*}` expansion |
| 4 | Arrays and dicts, the `tclrs` binary, the REPL, run-time `eval` | done |
| 4 | `proc`, `return`, `upvar` / `global`, `exit` | planned |
| 5 | The command library — `string`, `regexp`, `switch`, `for`, `catch` / `error`, file and channel IO | planned |
| 6 | `fusevm` `jit` / `jit-disk-cache` / `aot` features, bignum, benchmarks | planned |
| 7 | Toolchain parity with the sibling frontends — LSP, DAP, zsh completion, man pages, `reference.html`, inline `rust {}` FFI, `--dump-tokens` / `--dump-ast` / `--disasm` | planned |

Two things the binary does not reproduce, and does not pretend to:

- **The `errorInfo` traceback.** `tclsh` follows a failure's message with the
  stack of commands that raised it. tclrs resolves command dispatch while
  compiling and has no such stack; it prints the message, and the source
  location when it has one, in `tclsh`'s spelling, and invents nothing between
  them.
- **Where a failure is noticed.** A command that does not exist is a compile
  error here, so `puts a` before it never runs, where `tclsh` prints `a` first
  and fails afterwards. Running a file is where this shows; reading stdin, where
  commands are compiled one at a time as they complete, it does not.

### Differential test harness

Six suites, all comparing against the reference interpreter rather than
against hand-written expectations:

```sh
cargo test --test dodekalogue            # the twelve parse rules
cargo test --test differential_tclsh     # word splitting vs tclsh, character for character
cargo test --test execution_differential # whole programs vs tclsh, byte for byte
cargo test --test list_differential      # the list commands, plus generated matrices
cargo test --test array_differential     # arrays and dicts
cargo test --test cli_differential       # the binary vs tclsh: stdout, stderr, exit status
```

`cli_differential` runs the same script through both binaries the same way —
as a file, and piped to stdin — and compares all three. It is where the driver's
rules are pinned: which mode stops at the first failure and which carries on,
what each exits with, and what an unreadable file is called.

`interp_state` covers what no process can show: that a variable set by one
evaluation is there for the next, and that `eval {incr i}` in a hundred-pass
loop is compiled once.

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
