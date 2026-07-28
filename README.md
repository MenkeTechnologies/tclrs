# tclrs

Tcl as a [fusevm](https://github.com/MenkeTechnologies/fusevm) frontend. Tcl source is parsed and lowered to fusevm bytecode, which fusevm executes and compiles to native code through its Cranelift JIT. There is no interpreter loop and no code generator in this crate — those belong to the VM.

The reference implementation is tclsh 9.0.4. It is the specification: behavior is ported from it, not reinvented, and the test suite compares against it directly rather than against expectations written by hand.

## Status

Phase 2 of 7 — scripts compile to fusevm bytecode and run. `tclrs::eval` executes a script and returns its value and output.

Working commands: `set`, `puts` (with `-nonewline`), `expr`, `incr`, `if`/`elseif`/`else`, `while`, `for`, `switch`, `break`, `continue`, `proc`, `return`, `global`, `catch`, `error`, and command substitution of any of them.

`expr` covers the whole operator set of `expr(n)` — arithmetic with Tcl's floored integer division and remainder, integral `**`, numeric-preferring comparisons with string fallback, the always-string comparisons, bitwise and shift operators, short-circuit `&&`/`||`, and the ternary — over operands drawn from literals, variables, nested commands, and parenthesised subexpressions. Doubles print in Tcl's format.

`proc` covers argument lists with defaults and a trailing variadic `args`, procedure-local variables, `global`, recursion, and `return` with and without a value. `switch` covers both syntaxes, `-exact`, `-glob`, `--`, `default`, and the `-` body that falls through to the next clause.

Not built yet, and refused at compile time rather than approximated: every command not listed above, arrays, `{*}` expansion, math functions, `in`/`ni` (list support), variable and body words that are not literal, and arbitrary-precision integers — an operation that overflows `i64` is an error instead of silently wrapping.

Refused for the same reason within the commands that do work: `proc` anywhere but the script's own top level, twice for one name, or under the name of a command the compiler lowers itself, because this compiler registers a procedure whether or not the defining command is reached; `return -code` other than `ok` and `error`; a `return`, `break` or `continue` that leaves a `catch` script, which Tcl turns into that `catch`'s return code rather than an exit from the enclosing procedure or loop; `catch`'s options-dictionary variable; `error`'s `info` and `code` arguments; and `switch -regexp`, `-nocase`, `-matchvar` and `-indexvar`.

### Parser

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

## Why this shape

Two properties of the grammar make compilation worthwhile:

**Braces suppress substitution.** A braced body is fully known at parse time, so `if`, `while`, `proc` bodies and braced `expr` expressions compile once into bytecode instead of being re-parsed on every evaluation. Words carry a `braced` flag for exactly this decision.

**Each character is processed once.** Rule 11 rules out rescanning substituted values, so the parse is single-pass and the compiler can resolve variable and command references statically where the word shape allows.

**Values need no object heap.** Tcl strings, integers and floats map onto `fusevm::Value` directly. A value produced as a number stays a number in a VM slot and only acquires a string representation when something asks for one, which is where the reference implementation spends time in hot loops.

### Procedures on fusevm's calling convention

A procedure body is compiled into the enclosing chunk behind a jump that steps over it and registered with `ChunkBuilder::add_sub_entry`. `Op::Call(name, n)` resolves that entry, pushes a frame whose base is `n` values down the stack, and jumps to it; the prologue moves those `n` arguments into the frame's slots, and `Op::ReturnValue` pops the frame, truncates the stack back to its base and pushes the result. Procedure-local variables are the frame's slots, which fusevm allocates per call — that is what keeps them off the globals and out of a recursive call's way. `global` opts a name back out into `Op::GetVar`/`Op::SetVar`.

Since a call site knows the callee's signature at compile time, it is the call site that pushes a constant for each defaulted argument the caller omitted and folds the surplus into the variadic `args` list. The callee therefore receives exactly one value per formal parameter and needs no runtime argument count.

`catch` is the one construct with a runtime component. Entering a guarded script records the stack and frame depths together with the op index of a handler block the compiler laid down ahead of it; when a chunk stops with an error, the driver restores those depths, pushes the message and resumes the VM at the handler. The handler and the ordinary path meet at the same compile-time stack depth, so the static depth tracking that lets `break` and `continue` emit a known number of pops still holds.

## Build

```sh
cargo build
cargo test
```

The differential tests run every program through both `tclsh` and tclrs and compare the output byte for byte. They invoke `tclsh` from `PATH` and report a skip when none is installed.

## License

MIT OR Apache-2.0
