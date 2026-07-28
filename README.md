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
  script actually reaches. A hot loop **inside a procedure** reaches a compiled
  trace: 3,000,000 iterations of `while {$i < $n} {incr i}` in 6.6 ms against
  243.7 ms interpreted. The same loop at a script's **top level** reaches
  nothing, because a top-level variable is a VM global. Both halves are measured,
  and both are named precisely: see [JIT Compilation](#0x08-jit-compilation).
- **Differentially tested** — every program in the suite is executed by both
  `tclsh` and tclrs and the output compared byte for byte. No expected output in
  this repository is written by hand.

---

## [0x01] BUILD

A release tag publishes a prebuilt `tclrs` for macOS (arm64, x86_64) and Linux
(x86_64, aarch64) and bumps the tap formula, so a binary install is one command:

```sh
brew tap MenkeTechnologies/menketech
brew install tclrs
```

From source:

```sh
git clone https://github.com/MenkeTechnologies/tclrs
cd tclrs
cargo build
cargo test
```

Requires a stable Rust toolchain, and a C compiler for `--aot` to link with. A
script containing a [`rust { ... }`](#inline-rust) block needs `rustc` at *run*
time as well, since the block is compiled when the script is.

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

Shell completion is [`completions/_tclrs`](completions/_tclrs) — put that
directory on `fpath`. The manual pages are [`man/man1/tclrs.1`](man/man1/tclrs.1) and the all-in-one
[`man/man1/tclrsall.1`](man/man1/tclrsall.1): `man ./man/man1/tclrsall.1`.

`tclsh` is the specification for what the binary prints and what it exits with.

| Behavior | What happens |
| --- | --- |
| A script file | One script. The first failure ends it: the message goes to stderr, followed by `    (file "…" line N)` when the failure was located while compiling, and the process exits 1. |
| Stdin, not a terminal | A sequence of commands. Each is evaluated as it completes, a failure is reported on stderr and the next command still runs, and end of input exits 0 — which is why `tclrs < script` exits 0 where `tclrs script` exits 1. |
| Stdin, a terminal | The same evaluation, driven by a line editor: prompt, history, completion, multi-line editing, and the value of each command echoed. See [The REPL](#the-repl). |
| `argv0`, `argc`, `argv` | Set before the script runs, as `tclsh` sets them. |
| Errors | stderr only. No banner, no prompt outside a terminal, and no output the binary produces that the script did not ask for. |

An unknown option is refused (`tclrs: unknown option "--wat"`) rather than
treated as a file name — the one place this binary deliberately differs from
`tclsh`, which reads stdin for any argument starting with `-`.

### The REPL

A terminal gets a [`reedline`](https://crates.io/crates/reedline) line editor.
A pipe does not: `tclrs < script` is still the silent loop, byte for byte.

```text
─( 14:52:07 )──< command 3 >──────────────────────{ tclrs 0.1.0 }─
tclrs❯ proc double {x} {
····❯   expr {$x * 2}
····❯ }
tclrs❯ double 21
42
```

| | |
| --- | --- |
| Multi-line editing | A command left open keeps the editor on the same buffer. What counts as open is the parser's own answer — the `Validator` is `repl::incomplete` and nothing else — so the editor and the evaluator cannot disagree about where a command ends. Text that is malformed rather than unfinished is evaluated, and its error reported, instead of hanging the prompt. |
| Completion | Tab offers what the compiler would accept in that position: command names at the head of a command, an ensemble's subcommands after `string` / `array` / `dict` / `info`, this session's procedures, and the interpreter's variables after `$`. The vocabulary is assembled from the compiler's own tables (`src/names.rs`), and a test fails if a name is offered that the compiler does not know. |
| Procedures | A procedure is compiled into the chunk of the script that defines it, so it would otherwise last exactly one line. The REPL keeps the text of each definition and prefixes the set to every later evaluation, which is what makes `double` answer on the line after it was written. Writing a definition again replaces the earlier one. Coroutines are not carried this way — replaying `coroutine` would run its body again. |
| History | `~/.tclrs/history`, 5,000 commands, shared across sessions. |
| Keys | Emacs by default. `TCLRS_REPL_MODE=vi`, or `mode = "vi"` under `[repl]` in `~/.tclrs/config.toml`, switches to modal editing; Tab and Shift-Tab drive the completion menu in either. |
| Leaving | `exit`, `exit N`, `quit`, or Ctrl-D. Ctrl-C abandons the line being typed. |

### Options that do not run the script the ordinary way

```sh
tclrs --aot out script.tcl          # compile to a standalone native executable
tclrs --aot-object out.o script.tcl # emit the relocatable object only
tclrs --tiers script.tcl            # run it, then report which fusevm tiers took it
tclrs --dump-tokens script.tcl      # print the parser's lexical output
tclrs --dump-ast script.tcl         # print the parse tree
tclrs --disasm script.tcl           # print the compiled bytecode instead of running it
tclrs --lsp                         # speak the Language Server Protocol on stdio
tclrs --dap                         # speak the Debug Adapter Protocol on stdio
```

The two dumps are the parse made visible. Tcl has no lexer to print — a word's
substitutions are decided while it is read — so `--dump-tokens` prints the parts
of each word in the order they were read, under the shape of the word that
decides whether they are substituted at all:

```text
$ tclrs --dump-tokens -c 'puts "x is $x"'
line word  kind     value
   1    1  bare     puts
   1    1  · lit    puts
   1    2  quoted   x is $x
   1    2  · lit    x is
   1    2  · var    x
```

`--dump-ast` prints the same parse as the tree it is, with a command
substitution nested inside the word that contains it.

Each of those wants a whole script before it does anything, so it reads a file,
a `-c` argument, or all of stdin, and never opens a REPL. `--lsp` and `--dap`
are the exception: stdio carries the protocol, so neither takes a script there —
the language server is sent the document's text, and the debug adapter opens the
file its `launch` request names.

### The language server

`tclrs --lsp` speaks the Language Server Protocol on stdio. Point an editor's
Tcl client at it — the binary needs no configuration and no workspace.

| Capability | Where the answer comes from |
| --- | --- |
| Diagnostics | The parser's failure, then the compiler's, republished on every edit. A construct this frontend refuses is a diagnostic, so the editor reports what running the file would report rather than what full Tcl allows. |
| Completion | Command names at the head of a command, an ensemble's subcommands after `string` / `array` / `dict` / `info`, and the document's own procedures. The list is the compiler's tables (`src/names.rs`). |
| Hover, signature help | The synopsis — the wording of the command's own `wrong # args` message — and a one-line summary. An ensemble answers with its subcommand's synopsis. |
| Document symbols | The `proc` commands the parser found. |

What is under the cursor is decided by [`src/cursor.rs`](src/cursor.rs), the
module the REPL's completer uses, so the editor and the prompt agree.
[`tests/lsp_session.rs`](tests/lsp_session.rs) drives the real binary over the
wire: handshake, unsolicited diagnostics, edits, and a shutdown that exits.

### Inline Rust

A `rust { ... }` block compiles to a shared library and its exports become Tcl
commands:

```tcl
rust {
    pub extern "C" fn add(a: i64, b: i64) -> i64 { a + b }
}
puts [add 21 21]        ;# → 42
```

`rust {` is not a Tcl command, so the block never reaches the parser: the source
is rewritten first into `__rust_compile <base64> <line>`, padded to keep the
line count so a later error still points where it was written. The compiling,
`dlopen`ing and marshalling belong to
[`fusevm::ffi`](https://github.com/MenkeTechnologies/fusevm); the library is
cached under `~/.cache/fusevm/ffi` by the SHA-256 of the block's body
(`FUSEVM_FFI_DIR` relocates it), so the second run of a script does not call
rustc.

**Registration happens while compiling, not while running.** This frontend
resolves dispatch at compile time — a name is a builtin, a procedure, a
coroutine, or an error before the VM starts — so the block is compiled and
registered as its command is lowered, which is what makes `add` a known name by
the next line. A procedure of the same name still wins: dispatch asks the
script's own definitions first.

Signatures are fusevm's marshalling set: up to four `i64` arguments returning
`i64`, up to three `f64` returning `f64`, and `*const c_char` returning either
`i64` or `*const c_char` (`c_char`, `CStr` and `CString` are already in scope
inside a block). Anything else is not exported, and the block is refused for
having no exports.

### The debugger

`tclrs --dap` speaks the Debug Adapter Protocol on stdio: breakpoints, stepping,
stack frame, variables, and the program's output as `output` events.

Stopping is compiled in, not interpreted around. `compiler::compile_debug`
emits an `ext_wide::DBG_LINE` marker before every command, and the marker's
handler stops when a breakpoint matches, when the client is stepping, or when a
pause was asked for. Three consequences worth knowing:

- **A debugged script runs the same bytecode a plain run does**, plus the
  markers. There is no second lowering, and no interpreter written for the
  debugger.
- **An ordinary compilation carries no markers at all**, so nothing is paid for
  a debugger that is not attached.
- **Markers go into procedure bodies too**, which is what makes a breakpoint
  inside a procedure reachable and lets a step walk into one. A command
  substitution gets none — `set out [double 21]` is one step, not two.

The run happens on the adapter's own thread, and requests are served from inside
the stop, so `variables` reads the paused VM rather than a snapshot of it. The
cost is that an asynchronous `pause` lands at the next command rather than
mid-command; `stepIn` and `next` both stop at the next command, and `stepOut`
resumes to the next breakpoint.

### Environment

`TCLRS_JIT=off` (or `0`, or `no`) skips arming the JIT. It exists so the
benchmark can measure the interpreter and the JIT-armed VM as separate rows of
the same binary. `TCLRS_REPL_MODE=vi` picks the REPL's keymap.
`TCLRS_STATICLIB` points an [`--aot`](#0x09-ahead-of-time-compilation) link at a
`libtclrs.a` somewhere other than the build's own.

fusevm's own knobs work unchanged: `FUSEVM_JIT_BLOCK_THRESHOLD`,
`FUSEVM_JIT_TRACE_THRESHOLD`, `FUSEVM_JIT_CACHE_DIR` and `FUSEVM_FFI_DIR`.

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
| `parser::MAX_NESTING_DEPTH` | How deeply command substitutions and array indices may nest in the *input*, which is the parser's own recursion — 64_000, measured against the stack above. Deeper is a script error, for the same reason: an exhausted stack is a signal with nothing to report. |
| `Interp::cache_stats` | `(hits, misses)` from the source-keyed chunk cache — one miss per compilation, so the same `eval` text in a loop is lowered once. |
| `tclrs::eval_captured` | `eval` for a caller that wants both halves of a failing run: the error *and* whatever the script had already printed. |
| `tclrs::parse` | The parsed `Script` without running it, for tooling that wants the word structure. |
| `tclrs::aot` | `compile_object`, `compile_executable`, and `run_native` — the same codegen driven in-process. |
| `tclrs::tiers` | `report` and `inspect`: which fusevm tiers a chunk reaches. |
| `tclrs::dump` | `tokens` and `ast`: the two listings [`--dump-tokens`](#options-that-do-not-run-the-script-the-ordinary-way) and `--dump-ast` print. |
| `tclrs::lsp` | `run_stdio` for the whole server, or `diagnostics`, `completion`, `hover`, `signature_help` and `document_symbols` one answer at a time, for a host that already owns the transport. |
| `tclrs::dap` | `run_stdio`: the debug adapter, over stdio. |
| `tclrs::cursor` | `word_at` and `context_at`: what a position in a line is inside, which is how the REPL and the language server agree about it. |

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
| Associative data | `array` — `exists`, `get`, `names`, `set`, `size`, `unset`; `dict` — `create`, `exists`, `for`, `get`, `keys`, `merge`, `remove`, `set`, `size`, `values` |
| Strings | `format`, and the `string` ensemble — `cat`, `compare`, `equal`, `first`, `last`, `index`, `insert`, `is`, `length`, `map`, `match`, `range`, `repeat`, `replace`, `reverse`, `tolower`, `totitle`, `toupper`, `trim`, `trimleft`, `trimright` |

Command substitution works on any of them.

`docs/reference.html` is the same surface as a page, generated rather than
written: `cargo run --bin gen-docs` renders every command from the compiler's
own tables (`src/names.rs`), asks the compiler about each ensemble subcommand
and the runtime about each `format` conversion, and prints the `expr` ladder
from the table the parser binds with. A command it lists exists; one it does not
is `invalid command name`.

### `expr`

The whole operator set of `expr(n)`:

| Group | Operators |
| --- | --- |
| Arithmetic | `+ - * / % **`, unary `+ -` — with Tcl's floored integer division and remainder, and integral `**` for integral operands, a negative exponent included (`2 ** -1` is `0`; a zero base there is an error) |
| Comparison | `< > <= >= == !=` — numeric-preferring, falling back to string order |
| String comparison | `lt gt le ge eq ne` — always string |
| Bitwise / shift | `& ^ \| ~ << >>` |
| Logical | `&& \|\| !`, short-circuiting; the ternary `?:` |
| Membership | `in` `ni` — string equality against a list's elements, so `1 in {01}` is false |

Operands are literals, variables, nested commands (`[…]`), quoted and braced
strings, and parenthesised subexpressions. Doubles print in Tcl's format: the
shortest representation that reads back exactly, never looking like an integer,
exponential outside the positional range. A *literal*, though, prints as the
script wrote it — Tcl's first rule — so `puts 3.0` is `3.0` and `puts 007.0` is
`007.0`; the formatter is what an `expr` result goes through, not what a value
carries.

Where a value is used as a **condition** — `if`, `while`, `for`, the ternary,
`&&`, `||` — it has to be a boolean, and Tcl's boolean is narrower than "not
empty": a number in any radix, or one of `true` / `false` / `yes` / `no` / `on` /
`off` in any case, abbreviated to any prefix that stays unambiguous. `t`, `fals`,
`y`, `n` and `of` are booleans; `o` is not, because `on` and `off` both start with
it. Anything else is `expected boolean value but got "…"`, which is why
`if {"b"} {…}` is an error rather than a taken branch. `!` is the exception: it
takes a number *or* a boolean word, and refuses the operand otherwise.

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
| Arbitrary-precision integers. An `i64` that overflows is an error, and so is the one integer division whose true quotient does not fit (`i64::MIN / -1`) and an integer *literal* or operand that does not fit at all (`expr {99999999999999999999 + 1}`) | `integer value too large to represent` |
| Input nesting past `parser::MAX_NESTING_DEPTH` — 64_000 command substitutions or array indices deep, well past anything the reference interpreter survives | `too many nested substitutions (infinite loop?)` |
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

Braces nest through a counter, so a script of a million `{` costs no stack, but
command substitution and an array index are recursive — a `[` inside a `[` is a
nested script. That recursion is bounded by `parser::MAX_NESTING_DEPTH`, because
running out of native stack is a signal with nothing to report rather than an
error a script can be blamed for. The limit is 64_000 and it is measured: on the
stack the binary gives the parser (`runtime::RECOMMENDED_STACK`) a script of
nothing but `[` still parses at 80_000 levels and aborts by 90_000, and tclsh
segfaults on the same input between 20_000 and 30_000 — so the bound sits above
every depth the reference interpreter itself survives, and refuses nothing tclsh
can parse.

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
| **Numeric hook** | Catches operands the VM cannot compute on natively. An operand that parses as a number is one (including the `0x` / `0o` / `0b` / `0d` radix prefixes and `_` as numeric whitespace); comparisons fall back to string order when it does not; arithmetic on a non-number is an error, and so is an integer past `i64` — a bignum is what Tcl would promote to and this frontend has none. |
| **Extension ops** | `/` and `%` floor toward negative infinity (`-57 / 10` is `-6`, `-57 % 10` is `3`), `**` stays integral for integral operands *including a negative exponent* (`2 ** -1` is `0`), a normalize op converts a VM-native result into its Tcl value — booleans to `1`/`0`, doubles to Tcl's double format — and a boolean op applies Tcl's rule for a condition, which is not the VM's truthiness. The list, associative and string commands are extension ops too. |
| **No object heap** | Tcl's value model needs none on top of fusevm's: strings, integers and floats map onto `Value` directly. |

Extension op ids are laid out so `runtime`'s dispatch can test ranges from the
highest base down: the arithmetic and normalizing ops at 0–5, `eval` at 6,
control flow at 7–9, the coroutine ops at 10–14, the boolean conversion at 15,
the list commands from 16, the associative ones from 64, and the string ones
from 128. `catch` is the one op
whose payload is an op index, so it is an extension-*wide* op.

Static stack tracking is what keeps the lowering cheap: each command leaves its
result on the stack and the compiler tracks that depth as it goes, so `break`
and `continue` unwind with a known number of pops rather than a runtime
unwinder. Every loop — `while`, `for`, `foreach`, `dict for` — is emitted by one
function, `Compiler::rotated_loop`, which is what keeps that arithmetic and the
rotated branch layout the tracing JIT needs in a single place rather than
repeated four times.

---

## [0x08] JIT COMPILATION

### How it is turned on

`fusevm` is pulled with the Cranelift features, so `cargo build` links the JIT
and the persistent native-code cache:

```toml
fusevm = { version = "0.14.20", features = ["jit", "jit-disk-cache", "aot", "ffi"] }
```

| Feature | What it adds |
| --- | --- |
| `jit` | fusevm's Cranelift tiers — linear, block, tracing. |
| `jit-disk-cache` | Compiled native code persists to `~/.cache/fusevm-jit`, so codegen is not repaid on the next process. Relocate it with `FUSEVM_JIT_CACHE_DIR`, disable it with `FUSEVM_JIT_CACHE_DIR=off`. |
| `aot` | The closed-world compiler behind [`--aot`](#0x09-ahead-of-time-compilation). |
| `ffi` | The compile-and-`dlopen` path behind [`rust { ... }`](#inline-rust). |

One call arms the tiers, in the same function that installs every other hook,
so the interpreter, the binary, a coroutine's VM and an ahead-of-time run all
get the same VM.

### What the tiers reach on Tcl today

Not an estimate. `tclrs --tiers` asks fusevm's own predicates
(`is_block_eligible`, `is_trace_eligible`, `trace_is_compiled`,
`block_jit_is_compiled`) after running the script. The answer splits on one
thing: whether the loop's variables are a procedure's locals or a script's
top-level globals.

#### A loop inside a procedure reaches a compiled trace

```tcl
proc count {n} {
    set i 0
    while {$i < $n} {
        incr i
    }
    return $i
}
puts [count 3000000]
```

```
$ tclrs --tiers bench/counted_loop_proc.tcl
ops                     29
block-JIT eligible      false
block-JIT compiled      false
largest eligible region none
loop @7                trace-eligible=true traced=true blacklisted=false
JIT-ineligible ops
  Call                  1
  PrintLn               1
  ReturnValue           2
reaches native code     true
```

`traced=true`. Two things have to hold at once for that, and each was a separate
blocker.

**The ops.** A procedure's locals are frame slots, so the counter is
`GetSlot` / `SetSlot`, which the tiers accept. A top-level Tcl variable is a VM
global instead, and that is still fatal — see below.

**The shape.** fusevm arms its trace recorder at a backward branch and closes the
recording when a branch lands back on the anchor. A textbook `while` — evaluate
the test, `JumpIfFalse` forward past the body, close with an unconditional
backward `Jump` — records an op sequence that `is_trace_eligible` accepts and
that the trace compiler then declines, so the recording is aborted and nothing is
ever installed. A do-while, whose *conditional* backward branch closes the loop,
compiles. Every loop this frontend emits is therefore rotated into that shape
(`Compiler::rotated_loop`, `src/compiler.rs`):

```
    Jump -> cond          ; enter at the test, so it still runs before iteration 1
  body:
    <body>
  step:                   ; `for`'s third clause; empty for `while`
    <step>
  cond:
    <cond>
    JumpIfTrue -> body    ; conditional BACKWARD branch
  end:
```

`while {$i < 300000} {incr i}` inside a `proc`, before and after:

| Before — declined | After — traced |
| --- | --- |
| `05 GetSlot(0)` ← anchor | `05 Jump(12)` |
| `06 LoadInt(300000)` | `06 GetSlot(0)` ← anchor |
| `07 NumLt` | `07 LoadInt(1)` |
| `08 JumpIfFalse(16)` | `08 Add` |
| `09 GetSlot(0)` | `09 Dup` |
| `10 LoadInt(1)` | `10 SetSlot(0)` |
| `11 Add` | `11 Pop` |
| `12 Dup` | `12 GetSlot(0)` |
| `13 SetSlot(0)` | `13 LoadInt(300000)` |
| `14 Pop` | `14 NumLt` |
| `15 Jump(5)` | `15 JumpIfTrue(6)` |

Rotation moves where the exits land: `break` still jumps past the loop, and
`continue` jumps to the **step**, because in a rotated loop the next test sits
below the body. `for`'s step therefore still runs on `continue`, and a `break`
inside the step still ends the loop, as `for(n)` specifies. The condition is
still evaluated before the first iteration — that is what the entry `Jump` is
for — so `while {0} {...}` runs its body zero times and a loop's own value is
still empty. `for` / `foreach` / `while` programs covering break, continue, zero
iterations, a loop's own value, multi-variable `foreach`, nesting, and a body
that leaves values on the stack per iteration are all checked byte for byte
against tclsh in `tests/execution_differential.rs`.

The chunk as a whole stays block-ineligible for a separate reason — the `Call`
and the `puts` around the loop — so the whole-chunk tier is not what runs here.
It is the tracing tier.

#### A loop at a script's top level still reaches nothing

```
$ tclrs --tiers bench/counted_loop.tcl
ops                     20
block-JIT eligible      false
block-JIT compiled      false
largest eligible region none
loop @5                trace-eligible=false traced=false blacklisted=false
JIT-ineligible ops
  GetVar                3
  PrintLn               1
  SetVar                2
reaches native code     false
```

Same rotated shape, and `trace-eligible=false` before the shape is even
consulted. The arithmetic is not the problem — `tclrs --disasm` shows `NumLt`,
`Add`, `LoadInt`, `Jump` and `JumpIfTrue` with no extension op anywhere in the
loop. `GetVar` / `SetVar` are. A Tcl variable at a script's top level lowers to a
VM **global**, and fusevm's tiers accept slots, not globals: `Op::GetVar` and
`Op::SetVar` are absent from `is_block_eligible_op_at`
(`fusevm-0.14.20/src/jit.rs:4249`), whole-chunk eligibility is the conjunction of
that predicate over every op (`:4419`), and the tracing tier defers to the same
predicate for everything but `Call` / `Return` (`is_trace_op_allowed_at`,
`:6180`).

The fix is not in the loop's shape and is not attempted here: it is
slot-allocating a top-level Tcl variable whose name is known at compile time,
spilling to a global only where a script can reach a variable by computed name.
Wrapping the hot loop in a `proc` is the workaround, and it is what the
`counted_loop_proc` benchmark row measures.

#### `foreach` and `dict for` reach nothing either, for a third reason

Both are rotated too, and neither is trace-eligible in any spelling — not even
with its variables in a procedure's slots:

```tcl
proc sum {l} {set t 0; foreach x $l {incr t $x}; return $t}
set l {}
set i 0
while {$i < 2000} {lappend l 1; incr i}
puts [sum $l]
```

```
$ tclrs --tiers foreach_proc.tcl
ops                     61
block-JIT eligible      false
block-JIT compiled      false
largest eligible region none
loop @10               trace-eligible=false traced=false blacklisted=false
loop @39               trace-eligible=false traced=false blacklisted=false
JIT-ineligible ops
  Call                  1
  Extended              5
  GetVar                4
  PrintLn               1
  ReturnValue           2
  SetVar                4
reaches native code     false
```

`loop @10` is the `foreach` inside the procedure — slots, and still refused;
`loop @39` is the top-level `while` that builds the list, refused for the global
counter *and* for `lappend`. `foreach`'s loop state is carried by four frontend
extension ops (`FOREACH_INIT` / `MORE` / `TAKE` / `ADVANCE`), and
`is_trace_op_allowed_at` rejects `Op::Extended` outright — an extension handler
is arbitrary Rust with no Cranelift lowering. `dict for` is refused the same way
through `DICT_PAIRS`, plus the two hidden globals its cursor uses. Rotation
cannot help either of them; lowering their state to native ops could.

#### What Tcl's boolean rule costs, and where

That rejection of `Op::Extended` is why the conversion a Tcl condition needs is
emitted selectively. A condition has to be a boolean — `if {"b"}` is an error, not
a taken branch — and the rule is a ported one (`ParseBoolean`, `tclObj.c`), so it
lives in an extension op. Putting one before every branch would have taken the
compiled trace away from every loop in the language.

`Compiler::yields_number` decides it statically: an expression whose top-level
operator answers with a number needs no conversion, because the VM's truthiness
and Tcl's agree on every number. A relational or arithmetic test — which is what a
counted loop's is — is therefore untouched, and only a condition whose value could
be a *string* pays:

```tcl
proc h {n} {set i 0; set go 1; while {$go} {incr i; if {$i >= $n} {set go 0}}; return $i}
proc h2 {n} {set i 0; while {$i < $n} {incr i}; return $i}
```

| loop | condition | ops | trace-eligible | traced |
| --- | --- | --- | --- | --- |
| `h2` | `$i < $n` | 29 | true | true |
| `h` | `$go` | 42 | false | false |

Both are proc-local, both are rotated, and the second is refused for the one
`Extended` in its body. That is the whole cost of the rule, it is measured rather
than assumed, and the alternative was answering the wrong thing.

### The disk cache

`jit-disk-cache` is enabled and `~/.cache/fusevm-jit` is live, so a proc-local
loop's compiled trace outlives the process. The saving is below noise on this
machine: `counted_loop_proc` is 6.3 ± 0.3 ms with `FUSEVM_JIT_CACHE_DIR=off` and
6.7 ± 0.2 ms with the cache on, 5 runs after 2 warmup runs — Cranelift codegen
for a ten-op trace is cheap enough that the cache read costs about what it saves.
It earns its place on larger traces, not this one.

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
1117.7 for three million iterations.

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
runs, load average 4.0 at the start of the run. Mean ± σ in milliseconds,
copied from the `target/bench/*.md` hyperfine exports that run produced.

| Benchmark | tclsh 9.0.4 | tclrs interp | tclrs JIT | tclrs AOT |
| --- | ---: | ---: | ---: | ---: |
| `startup` — the empty script | 49.5 ± 2.1 | 6.1 ± 0.4 | 6.5 ± 0.9 | **6.0 ± 0.6** |
| `counted_loop_proc` — 3M × `incr`, inside a `proc` | 159.1 ± 7.8 | 243.7 ± 1.8 | **6.6 ± 0.2** | 8.5 ± 0.5 |
| `counted_loop` — 3M × `incr`, at the top level | 1117.7 ± 34.9 | 234.7 ± 1.9 | 316.7 ± 3.2 | **8.1 ± 0.9** |
| `counted_loop_expr` — 3M × `set i [expr {$i + 1}]` | 1271.3 ± 25.0 | **265.5 ± 2.3** | 347.5 ± 3.4 | 333.3 ± 3.2 |
| `integer_arith` — 1M × `$sum + $i * $i - ($i >> 3)` | 768.6 ± 14.5 | **205.5 ± 2.0** | 234.8 ± 1.6 | 230.2 ± 3.0 |
| `string_build` — 100k × `set s "$s$i"` | 864.6 ± 8.3 | **550.2 ± 23.0** | 559.3 ± 14.9 | 558.5 ± 19.9 |
| `list_iterate` — 5k × `lappend`, then `foreach` | **56.7 ± 2.4** | 810.1 ± 21.5 | 804.9 ± 6.5 | 900.0 ± 27.4 |

Every ratio below is that table's means divided; nothing else is inferred.

**What the JIT is worth when it fires.** `counted_loop_proc` is `counted_loop`
with the loop moved inside a `proc`, which is the whole difference: the counter
becomes a frame slot and the tracing tier takes it (`--tiers` says
`traced=true`; see [JIT Compilation](#0x08-jit-compilation)). 6.6 ms against
243.7 ms interpreted — **36.9× tclrs interpreted and 24.1× tclsh** — with
`startup` measured at 6.5 ms on the same run, so the 3,000,000 iterations
themselves are inside the noise of process startup. Scaling the same script to
30,000,000 iterations takes 18.8 ± 0.3 ms (5 runs, 2 warmup), which puts the
marginal cost at about 0.45 ns per iteration: native code, not a dispatch loop.

That row also inverts tclsh's own ranking. tclsh compiles a procedure's locals
too, so it runs the proc-local loop in 159.1 ms against 1117.7 ms for the
top-level one — a 7.0× spread on the same arithmetic. tclrs interpreted is nearly
indifferent (243.7 vs 234.7 ms); it is the JIT that turns the procedure boundary
into 37×.

**Where tclrs wins without the JIT.** Interpreted, tclrs is 4.8× tclsh on the
top-level counted loop, 4.8× on the same loop written with `expr`, 3.7× on
integer arithmetic, 1.6× on string building, and starts in 6.1 ms against tclsh's
49.5. Ahead-of-time compiled, the top-level counted loop is **138× tclsh and 29×
tclrs interpreted**: 8.1 ms for the whole process, of which 6.0 is the binary's
own startup. That script contains no extension op in its loop, so fusevm's
ahead-of-time compiler lowers all of it, counter included, to native registers —
AOT is a closed-world compile and is not bound by the slot/global split the
tracing tier is.

**Where the AOT win disappears.** `counted_loop_expr` is the same loop with the
increment written as `expr` instead of `incr`. That adds one op — the extension
op `expr` emits to normalize its result — and AOT goes from 8.1 ms to 333.3 ms,
1.26× *slower* than interpreting, because the native path deopts there and hands
the rest of the run over. Same story for `integer_arith` (1.12× slower than
interpreting) and `string_build` (within noise). On `list_iterate` AOT is 1.1×
slower still, paying for the embedded chunk's deserialization and the deopt on a
script that never gets to run natively.

**Where tclrs loses.** `list_iterate` — 14× slower than tclsh interpreted, 16×
ahead-of-time compiled. tclsh keeps a list as an object and `lappend` appends to
it in amortized constant time; tclrs stores the list as its string
representation and re-derives it on every `lappend`, which is quadratic.
Building a 5,000-element list takes tclsh 57 ms and tclrs 810. This is a
data-representation problem in the list commands, not a code-generation one, and
no JIT tier would fix it.

**What the JIT costs where it does not fire.** The `tclrs JIT` column is the same
binary as `tclrs interp` with the tracing JIT armed. On a script whose loops it
cannot take it is not free: 35% slower on `counted_loop`, 31% on
`counted_loop_expr`, 14% on `integer_arith`, and within noise on the two
benchmarks whose time goes elsewhere. That is the recorder check in the dispatch
loop plus the once-per-run block-tier lookup, and on `counted_loop` it is paid
for a tier that never fires — the loop's `GetVar`/`SetVar` disqualify it. The
same overhead on `counted_loop_proc` buys a 37× win. It stays on unconditionally
because which of those two a script gets is not knowable before the script runs,
and hiding the cost would make the table dishonest.

Caveats worth knowing before quoting any of this: the machine was not idle (load
average 4.0, a shared workstation); `startup` and the `counted_loop_proc` JIT row
are both ~6 ms, close enough to hyperfine's calibration floor that it warns, so
treat the 37× as a lower bound on the loop and not a precise process-level
figure; and the AOT rows run with the JIT armed too, since the ahead-of-time
runtime hook goes through the same install point.

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

Three suites drive the binary rather than the library:
[`tests/lsp_session.rs`](tests/lsp_session.rs) and
[`tests/dap_session.rs`](tests/dap_session.rs) speak the real protocols to the
real process over stdio — handshake, diagnostics, breakpoints, stepping, and a
shutdown that exits — and [`tests/rust_ffi.rs`](tests/rust_ffi.rs) runs a script
with a `rust { ... }` block in it, so `rustc` is invoked, the library is loaded
and the exported function is called, rather than the test stopping at the
desugaring.

Several of them generate their cases rather than listing them: every awkward
element value driven through every list command, `foreach` through every shape
its grammar allows, the glob matcher over a pattern × subject grid, and every
index form against lists of every length — each matrix run as one script and
compared line for line.

The differential suites skip when no `tclsh` is on `PATH`. The full
ahead-of-time link test skips when `libtclrs.a` has not been built or there is
no `cc`.

### Examples

[`examples/`](examples) holds runnable programs, one per slice of the language —
variables and substitution, `expr`, control flow, procedures, lists, strings,
`dict` and `array`, errors, coroutines, `eval`, and a FizzBuzz that prints. Run
one directly:

```sh
cargo run --bin tclrs -- examples/lists.tcl
```

Each is self-checking: results go through a `check` procedure that raises a Tcl
error — so a non-zero exit — the moment one drifts.
[`tests/examples.rs`](tests/examples.rs) gates them twice. Every script has to
exit cleanly under the built binary, which needs no Tcl installed and so runs
anywhere; and every script's stdout has to match `tclsh` byte for byte, which is
what keeps an expectation written into a script from being wrong in the same
direction as the implementation. The second test skips when no `tclsh` is on
`PATH`, like the other differential suites.

### Differential fuzzing

```sh
cargo build
bash scripts/fuzz_parity.sh -n 400 -s 1 -m
```

`scripts/fuzz/gen.tcl` generates whole Tcl programs from a seed — the same seed
gives the byte-identical corpus, so a divergence reproduces from the seed and the
case index alone — and `scripts/fuzz_parity.sh` runs every one under both `tclsh`
and `tclrs` through one driver, `scripts/fuzz/drive.tcl`. Loop bounds are
structural, so a generated program always terminates, and values come from a pool
of awkward literals: empty strings, braces, brackets, quotes, backslashes, `$`, a
leading `#`, leading zeros, `1_0`, the `i64` boundaries, exponent-form floats and
non-ASCII text.

Every case lands in exactly one bucket, and every bucket is counted: **pass**,
**skip** (tclrs refused something it documents as unimplemented, with the
refusal's own wording as the reason), **allowed** (one of the enumerated known
divergences, each with a per-entry hit count so an over-broad suppression is
visible rather than silent), **divergence**, **critical** (tclrs died or hung —
never suppressible), and **excluded** (tclsh died or hung, so there is no
reference behavior; never charged against tclrs). `-m` minimises each divergence
to the statement that causes it and writes it to `tests/fuzz_corpus/` with both
engines' observed output; `tests/parity_fuzz_corpus.rs` replays that corpus, and
`tests/parity_fuzz_findings.rs` pins each finding against a live tclsh. The exit
status is the number of unsuppressed divergences, capped at 250. `-h` prints the
whole interface.

What it has found is [`BUGS.md`](BUGS.md).

A second fuzzer needs no `tclsh`: `fuzz/fuzz_targets/` holds cargo-fuzz targets
over the parser and the compiler, for the inputs a grammar never produces — a
lone `\x00`, a truncated escape, thousands of nested brackets.

```sh
cargo +nightly fuzz run parse      # or: compile
```

`tests/fuzz_smoke.rs` replays their seed corpus and a hostile-input list under
stable, so `cargo test` keeps the scaffolding honest without nightly.

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).
