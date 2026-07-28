# tclrs

Tcl as a [fusevm](https://github.com/MenkeTechnologies/fusevm) frontend. Tcl source is parsed and lowered to fusevm bytecode, which fusevm executes and compiles to native code through its Cranelift JIT. There is no interpreter loop and no code generator in this crate — those belong to the VM.

The reference implementation is tclsh 9.0.4. It is the specification: behavior is ported from it, not reinvented, and the test suite compares against it directly rather than against expectations written by hand.

## Status

Phase 1 of 7 — the parser. `tclrs::parse` implements all twelve syntax rules of `Tcl(n)`:

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

Not yet present: the compiler, builtins, a runtime, the CLI. Nothing here executes Tcl yet.

## Why this shape

Two properties of the grammar make compilation worthwhile:

**Braces suppress substitution.** A braced body is fully known at parse time, so `if`, `while`, `proc` bodies and braced `expr` expressions compile once into bytecode instead of being re-parsed on every evaluation. Words carry a `braced` flag for exactly this decision.

**Each character is processed once.** Rule 11 rules out rescanning substituted values, so the parse is single-pass and the compiler can resolve variable and command references statically where the word shape allows.

**Values need no object heap.** Tcl strings, integers and floats map onto `fusevm::Value` directly. A value produced as a number stays a number in a VM slot and only acquires a string representation when something asks for one, which is where the reference implementation spends time in hot loops.

## Build

```sh
cargo build
cargo test
```

The differential tests invoke `tclsh` from `PATH` and report a skip when none is installed.

## License

MIT OR Apache-2.0
