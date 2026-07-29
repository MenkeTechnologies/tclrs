# tclrs conformance against the official Tcl test suite

Reference interpreter: **tclsh 9.0.4**. Suite: `tcl9.0.4/tests` — the `tests/` directory of the matching Tcl source release, fetched and checksum-verified by `conformance/fetch-suite.sh`.

**2248 of 5066 attempted cases pass — 44.4%.** Over every case the suite contains, including the ones that cannot be run here, that is 2248 of 69424 — 3.2%.

## How the number is produced

The suite drives every test through the `tcltest` package. tclrs cannot load it — tcltest is Tcl code built on namespaces, `proc`, `catch`, `regexp` and channel IO, none of which this frontend has — so the cases are lifted out of the suite files instead of being run in place.

The lifting is done by `conformance/extract.tcl`, running under tclsh with the real tcltest loaded and only `::tcltest::test` replaced by a recorder. The recorder is a port of tcltest's own argument parsing, so both the `-option value` form and the historical `test name desc ?constraints? body result` form are read exactly as tcltest reads them, and constraint state comes from tcltest's own evaluation rather than a re-implementation of it. Every suite file is extracted; there is no option to select a subset, and the runner has no way to run one.

Each extracted case becomes a standalone program — its `-setup` followed by its `-body` — and is run twice: once by tclsh in a fresh child interpreter, once by tclrs through `tclrs::eval_captured`. The outcome of a run is the triple (return code, result string, everything written to stdout), and a case passes only when the two triples are identical byte for byte. The suite's own `-result` and `-match` values are not consulted: tclsh is the specification, and comparing against what it actually does is stricter than comparing against what the suite says it should.

Verdicts are assigned in a fixed order, and agreement is checked before any excuse for tclrs is considered, so no rule below can turn a pass into a skip. A case is set aside only when it genuinely cannot be run:

| Skip reason | What it means |
| --- | --- |
| tcltest constraint not met | tcltest's own constraint check says this build, platform or configuration cannot run the case. |
| tclsh produced no reference outcome | the reference run hung and was killed, or died on the case, so there is nothing to compare against. |
| needs a command plain tclsh has not got | the reference run failed with `invalid command name`: the case needs the internal commands of the `tcl::test` package, or a helper an earlier test body would have defined. Set aside even when tclrs happens to report the same error, which costs passes rather than inventing them. |
| needs a package that is not installed | the reference run failed with `can't find package`. |
| tclrs has no such command | tclrs refused with `invalid command name` for a command it does not implement. |

Everything else is attempted, and anything attempted either matches or fails. A *feature* tclrs declines inside a command it does have — `{*}` expansion, a missing math function, an `lsort` option, an integer too wide for `i64` — counts as a failure, not a skip. Those failures are also counted on their own below, so the effect of the looser rule is visible rather than assumed.

Three things about the extraction are worth stating plainly. First, suite files set variables at their top level and then write bodies that read them, so each case carries the global variables its file had created by the time the test was declared, replayed ahead of the body as `set` and `array set` commands. Only variables whose name appears in the case's own text are carried — without that a file which builds a large table at its top level would attach a copy of it to every one of its cases — and both runs get exactly the same program, so whatever is left out is left out of both. Second, procs are not replayed and bodies are not executed during extraction, so a case that depends on a helper proc or on state an earlier body would have produced fails under tclsh too, and is skipped as needing an unavailable command rather than counted against tclrs. Third, `-cleanup` scripts are not run: they execute after the value under test is produced and cannot change it.

## Totals

| | Cases | Share |
| --- | ---: | ---: |
| Extracted from the suite | 69424 | 100% |
| Skipped — cannot be run | 64358 | 92.7% |
| Attempted | 5066 | 7.3% |
| ⤷ passed | 2248 | 44.4% of attempted |
| ⤷ failed | 2818 | 55.6% of attempted |

Of the 2818 failures, 1538 are a feature tclrs documents as not built yet rather than a wrong answer. Counting those as skips instead would give 2248 of 3528 — 63.7% — and that looser number is stated here only so the choice of rule is visible. The headline above uses the strict rule.

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| tclrs has no such command | 45944 |
| tcltest constraint not met | 13663 |
| needs a command plain tclsh has not got | 4751 |

### Commands tclrs does not have, by how many cases they block

A case is attributed to the first command tclrs refused, so a body using several missing commands is counted once, against the first of them.

| Command | Cases |
| --- | ---: |
| `clock` | 17287 |
| `encoding` | 16863 |
| `apply` | 5567 |
| `file` | 733 |
| `binary` | 664 |
| `namespace` | 517 |
| `interp` | 415 |
| `oo::class` | 383 |
| `open` | 336 |
| `trace` | 273 |
| `scan` | 247 |
| `zipfs` | 220 |
| `regexp` | 203 |
| `regsub` | 129 |
| `lseq` | 121 |
| `subst` | 111 |
| `glob` | 95 |
| `try` | 94 |
| `chan` | 83 |
| `socket` | 81 |
| `run` | 78 |
| `oo::object` | 73 |
| `after` | 71 |
| `variable` | 62 |
| `rename` | 61 |
| `ledit` | 60 |
| `history` | 55 |
| `upvar` | 50 |
| `assemble` | 47 |
| `exec` | 41 |
| `tcl::prefix` | 36 |
| `lmap` | 32 |
| `safe::interpCreate` | 31 |
| `fpclassify` | 30 |
| `zlib` | 27 |
| `child` | 24 |
| `tcl::unsupported::disassemble` | 22 |
| `tcl::unsupported::getbytecode` | 22 |
| `lassign` | 21 |
| `safe::interpDelete` | 20 |
| *118 further commands* | 659 |

## Why cases failed

| Cause | Cases | Share of failures | For example |
| --- | ---: | ---: | --- |
| tclrs raised an error, tclsh did not | 1963 | 69.7% | `append.test` append-4.19, `append.test` append-4.20, `append.test` append-9.1 |
| both raised an error, messages differ | 787 | 27.9% | `append.test` append-3.1, `append.test` append-3.2, `append.test` append-6.1 |
| results differ | 57 | 2.0% | `append.test` append-3.4, `append.test` append-3.5, `append.test` append-3.6 |
| tclsh raised an error, tclrs did not | 10 | 0.4% | `compExpr.test` compExpr-2.11, `encoding.test` encoding-23.1, `namespace-old.test` namespace-old-1.27 |
| tclrs was killed or crashed | 1 | 0.0% | `obj.test` obj-32.1 |

Every failing case is written out in full — its program, the tclsh outcome and the tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this table.

### The most frequent failing messages

Error text with the quoted part elided and tclrs's trailing `(line N)` removed, so that one cause groups into one row.

| Message | Cases |
| --- | ---: |
| math function "…" is not supported yet | 447 |
| unknown or unsupported subcommand "…": only "…" is supported | 332 |
| command name must be a literal in this phase | 282 |
| wrong # args: should be "…"; the options variable is not supported | 241 |
| expression must be a literal in this phase | 145 |
| integer value too large to represent | 142 |
| wrong # args: should be "…" | 100 |
| invalid bareword "…" | 78 |
| "…" outside of a procedure is not supported | 67 |
| identical text apart from tclrs's trailing (line N) | 59 |
| missing operand at _@_ | 57 |
| bad option "…": only -exact, -glob and -- are supported | 43 |
| script body must be a literal in this phase | 42 |
| lsearch -stride is not supported yet | 40 |
| missing operator at _@_ | 34 |
| lsort -dictionary is not supported yet | 33 |
| array startsearch is not supported yet | 27 |
| lsort -index is not supported yet | 27 |
| lsearch -index is not supported yet | 25 |
| dict filter is not supported yet | 24 |
| dict replace is not supported yet | 18 |
| the "…" character class needs Unicode category tables, which are not built yet | 16 |
| "…" of the procedure-local variable "…" is not supported yet | 15 |
| array default is not supported yet | 15 |
| lsearch -subindices is not supported yet | 15 |
| this command does not take an array element yet | 15 |
| dict map is not supported yet | 14 |
| wrong # args: should be "…"; the info and code arguments are not supported | 14 |
| dict update is not supported yet | 13 |
| dict lappend is not supported yet | 12 |

## Command coverage

Independently of the suite: of the 109 commands the reference interpreter defines in the global namespace, tclrs answers to 40 — 36.7%. A name counts as answered when tclrs does not refuse it with `invalid command name`.

Implemented: `append`, `array`, `break`, `catch`, `concat`, `continue`, `coroutine`, `dict`, `error`, `eval`, `expr`, `for`, `foreach`, `format`, `global`, `if`, `incr`, `info`, `join`, `lappend`, `lindex`, `linsert`, `list`, `llength`, `lrange`, `lreplace`, `lreverse`, `lsearch`, `lsort`, `proc`, `puts`, `return`, `set`, `split`, `string`, `switch`, `unset`, `while`, `yield`, `yieldto`

## Per file

| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `aaa_exit.test` | 2 | 2 | 0 | 0 | 0 | — |
| `abstractlist.test` | 123 | 123 | 0 | 0 | 0 | — |
| `append.test` | 52 | 10 | 42 | 26 | 16 | 61.9% |
| `appendComp.test` | 48 | 17 | 31 | 21 | 10 | 67.7% |
| `apply.test` | 42 | 41 | 1 | 0 | 1 | 0.0% |
| `assemble.test` | 283 | 282 | 1 | 0 | 1 | 0.0% |
| `assocd.test` | 11 | 11 | 0 | 0 | 0 | — |
| `async.test` | 12 | 12 | 0 | 0 | 0 | — |
| `autoMkindex.test` | 11 | 11 | 0 | 0 | 0 | — |
| `basic.test` | 147 | 144 | 3 | 0 | 3 | 0.0% |
| `bigdata.test` | 113 | 113 | 0 | 0 | 0 | — |
| `binary.test` | 750 | 750 | 0 | 0 | 0 | — |
| `brodnik.test` | 422 | 422 | 0 | 0 | 0 | — |
| `chan.test` | 42 | 42 | 0 | 0 | 0 | — |
| `chanio.test` | 779 | 779 | 0 | 0 | 0 | — |
| `clock-ivm.test` | 8744 | 8725 | 19 | 0 | 19 | 0.0% |
| `clock-no-tzdata.test` | 0 | 0 | 0 | 0 | 0 | — |
| `clock.test` | 8744 | 8725 | 19 | 0 | 19 | 0.0% |
| `cmdAH.test` | 17001 | 16988 | 13 | 3 | 10 | 23.1% |
| `cmdIL.test` | 168 | 54 | 114 | 29 | 85 | 25.4% |
| `cmdInfo.test` | 12 | 12 | 0 | 0 | 0 | — |
| `cmdMZ.test` | 97 | 51 | 46 | 9 | 37 | 19.6% |
| `compExpr-old.test` | 184 | 3 | 181 | 105 | 76 | 58.0% |
| `compExpr.test` | 82 | 9 | 73 | 47 | 26 | 64.4% |
| `compile.test` | 171 | 145 | 26 | 4 | 22 | 15.4% |
| `concat.test` | 9 | 0 | 9 | 9 | 0 | 100.0% |
| `config.test` | 9 | 9 | 0 | 0 | 0 | — |
| `coroutine.test` | 77 | 60 | 17 | 2 | 15 | 11.8% |
| `dcall.test` | 6 | 6 | 0 | 0 | 0 | — |
| `dict.test` | 373 | 86 | 287 | 85 | 202 | 29.6% |
| `dstring.test` | 46 | 46 | 0 | 0 | 0 | — |
| `encoding.test` | 232 | 228 | 4 | 3 | 1 | 75.0% |
| `env.test` | 32 | 29 | 3 | 0 | 3 | 0.0% |
| `error.test` | 317 | 139 | 178 | 2 | 176 | 1.1% |
| `eval.test` | 12 | 0 | 12 | 9 | 3 | 75.0% |
| `event.test` | 65 | 64 | 1 | 0 | 1 | 0.0% |
| `exec.test` | 145 | 145 | 0 | 0 | 0 | — |
| `execute.test` | 157 | 103 | 54 | 15 | 39 | 27.8% |
| `expr-old.test` | 461 | 32 | 429 | 294 | 135 | 68.5% |
| `expr.test` | 2168 | 1100 | 1068 | 432 | 636 | 40.4% |
| `fCmd.test` | 306 | 306 | 0 | 0 | 0 | — |
| `fileName.test` | 306 | 306 | 0 | 0 | 0 | — |
| `fileSystem.test` | 140 | 140 | 0 | 0 | 0 | — |
| `fileSystemEncoding.test` | 1 | 1 | 0 | 0 | 0 | — |
| `for-old.test` | 9 | 0 | 9 | 5 | 4 | 55.6% |
| `for.test` | 88 | 41 | 47 | 12 | 35 | 25.5% |
| `foreach.test` | 43 | 3 | 40 | 19 | 21 | 47.5% |
| `format.test` | 269 | 1 | 268 | 257 | 11 | 95.9% |
| `get.test` | 23 | 17 | 6 | 6 | 0 | 100.0% |
| `history.test` | 62 | 57 | 5 | 5 | 0 | 100.0% |
| `http.test` | 528 | 513 | 15 | 0 | 15 | 0.0% |
| `http11.test` | 147 | 147 | 0 | 0 | 0 | — |
| `httpPipeline.test` | 5988 | 5988 | 0 | 0 | 0 | — |
| `httpProxy.test` | 150 | 150 | 0 | 0 | 0 | — |
| `httpcookie.test` | 60 | 56 | 4 | 0 | 4 | 0.0% |
| `icu.test` | 58 | 58 | 0 | 0 | 0 | — |
| `if-old.test` | 33 | 7 | 26 | 17 | 9 | 65.4% |
| `if.test` | 73 | 5 | 68 | 1 | 67 | 1.5% |
| `incr-old.test` | 14 | 1 | 13 | 5 | 8 | 38.5% |
| `incr.test` | 69 | 2 | 67 | 21 | 46 | 31.3% |
| `indexObj.test` | 65 | 65 | 0 | 0 | 0 | — |
| `info.test` | 287 | 170 | 117 | 0 | 117 | 0.0% |
| `init.test` | 10 | 10 | 0 | 0 | 0 | — |
| `interp.test` | 355 | 355 | 0 | 0 | 0 | — |
| `io.test` | 884 | 883 | 1 | 0 | 1 | 0.0% |
| `ioCmd.test` | 377 | 372 | 5 | 0 | 5 | 0.0% |
| `ioTrans.test` | 106 | 106 | 0 | 0 | 0 | — |
| `iogt.test` | 17 | 17 | 0 | 0 | 0 | — |
| `join.test` | 10 | 0 | 10 | 7 | 3 | 70.0% |
| `lindex.test` | 84 | 38 | 46 | 44 | 2 | 95.7% |
| `link.test` | 77 | 77 | 0 | 0 | 0 | — |
| `linsert.test` | 28 | 0 | 28 | 27 | 1 | 96.4% |
| `list.test` | 78 | 1 | 77 | 75 | 2 | 97.4% |
| `listObj.test` | 59 | 17 | 42 | 42 | 0 | 100.0% |
| `listRep.test` | 231 | 227 | 4 | 4 | 0 | 100.0% |
| `llength.test` | 6 | 0 | 6 | 4 | 2 | 66.7% |
| `lmap.test` | 66 | 65 | 1 | 0 | 1 | 0.0% |
| `load.test` | 30 | 30 | 0 | 0 | 0 | — |
| `lpop.test` | 19 | 19 | 0 | 0 | 0 | — |
| `lrange.test` | 1766 | 1735 | 31 | 27 | 4 | 87.1% |
| `lrepeat.test` | 12 | 12 | 0 | 0 | 0 | — |
| `lreplace.test` | 3579 | 3521 | 58 | 56 | 2 | 96.6% |
| `lsearch.test` | 165 | 0 | 165 | 48 | 117 | 29.1% |
| `lseq.test` | 136 | 133 | 3 | 0 | 3 | 0.0% |
| `lset.test` | 89 | 89 | 0 | 0 | 0 | — |
| `lsetComp.test` | 19 | 19 | 0 | 0 | 0 | — |
| `macOSXFCmd.test` | 14 | 14 | 0 | 0 | 0 | — |
| `macOSXLoad.test` | 57 | 57 | 0 | 0 | 0 | — |
| `main.test` | 67 | 67 | 0 | 0 | 0 | — |
| `mathop.test` | 385 | 262 | 123 | 0 | 123 | 0.0% |
| `misc.test` | 301 | 300 | 1 | 0 | 1 | 0.0% |
| `msgcat.test` | 135 | 135 | 0 | 0 | 0 | — |
| `mutex.test` | 12 | 12 | 0 | 0 | 0 | — |
| `namespace-old.test` | 126 | 113 | 13 | 0 | 13 | 0.0% |
| `namespace.test` | 314 | 311 | 3 | 0 | 3 | 0.0% |
| `notify.test` | 23 | 23 | 0 | 0 | 0 | — |
| `nre.test` | 28 | 27 | 1 | 0 | 1 | 0.0% |
| `obj.test` | 84 | 76 | 8 | 0 | 8 | 0.0% |
| `oo.test` | 388 | 364 | 24 | 0 | 24 | 0.0% |
| `ooNext2.test` | 62 | 54 | 8 | 0 | 8 | 0.0% |
| `ooProp.test` | 55 | 55 | 0 | 0 | 0 | — |
| `ooUtil.test` | 33 | 33 | 0 | 0 | 0 | — |
| `opt.test` | 31 | 31 | 0 | 0 | 0 | — |
| `package.test` | 0 | 0 | 0 | 0 | 0 | — |
| `parse.test` | 271 | 208 | 63 | 0 | 63 | 0.0% |
| `parseExpr.test` | 286 | 219 | 67 | 3 | 64 | 4.5% |
| `parseOld.test` | 158 | 11 | 147 | 119 | 28 | 81.0% |
| `pid.test` | 5 | 5 | 0 | 0 | 0 | — |
| `pkgMkIndex.test` | 27 | 27 | 0 | 0 | 0 | — |
| `platform.test` | 9 | 8 | 1 | 0 | 1 | 0.0% |
| `proc-old.test` | 74 | 31 | 43 | 22 | 21 | 51.2% |
| `proc.test` | 38 | 31 | 7 | 1 | 6 | 14.3% |
| `process.test` | 18 | 18 | 0 | 0 | 0 | — |
| `pwd.test` | 3 | 3 | 0 | 0 | 0 | — |
| `reg.test` | 1141 | 1141 | 0 | 0 | 0 | — |
| `regexp.test` | 257 | 257 | 0 | 0 | 0 | — |
| `regexpComp.test` | 179 | 179 | 0 | 0 | 0 | — |
| `registry.test` | 125 | 125 | 0 | 0 | 0 | — |
| `rename.test` | 19 | 18 | 1 | 1 | 0 | 100.0% |
| `resolver.test` | 10 | 10 | 0 | 0 | 0 | — |
| `result.test` | 26 | 22 | 4 | 0 | 4 | 0.0% |
| `safe-stock.test` | 11 | 5 | 6 | 0 | 6 | 0.0% |
| `safe-stock86.test` | 0 | 0 | 0 | 0 | 0 | — |
| `safe-zipfs.test` | 22 | 6 | 16 | 0 | 16 | 0.0% |
| `safe.test` | 155 | 100 | 55 | 0 | 55 | 0.0% |
| `scan.test` | 185 | 185 | 0 | 0 | 0 | — |
| `security.test` | 1 | 1 | 0 | 0 | 0 | — |
| `set-old.test` | 153 | 11 | 142 | 60 | 82 | 42.3% |
| `set.test` | 64 | 4 | 60 | 23 | 37 | 38.3% |
| `socket.test` | 189 | 180 | 9 | 0 | 9 | 0.0% |
| `source.test` | 23 | 23 | 0 | 0 | 0 | — |
| `split.test` | 18 | 0 | 18 | 16 | 2 | 88.9% |
| `stack.test` | 3 | 3 | 0 | 0 | 0 | — |
| `string.test` | 705 | 677 | 28 | 3 | 25 | 10.7% |
| `stringObj.test` | 81 | 81 | 0 | 0 | 0 | — |
| `subst.test` | 63 | 63 | 0 | 0 | 0 | — |
| `switch.test` | 113 | 49 | 64 | 2 | 62 | 3.1% |
| `tailcall.test` | 37 | 33 | 4 | 0 | 4 | 0.0% |
| `tcltest.test` | 127 | 114 | 13 | 7 | 6 | 53.8% |
| `thread.test` | 52 | 52 | 0 | 0 | 0 | — |
| `timer.test` | 54 | 54 | 0 | 0 | 0 | — |
| `tm.test` | 21 | 21 | 0 | 0 | 0 | — |
| `trace.test` | 290 | 290 | 0 | 0 | 0 | — |
| `unixFCmd.test` | 49 | 49 | 0 | 0 | 0 | — |
| `unixFile.test` | 7 | 7 | 0 | 0 | 0 | — |
| `unixForkEvent.test` | 1 | 1 | 0 | 0 | 0 | — |
| `unixInit.test` | 8 | 7 | 1 | 0 | 1 | 0.0% |
| `unixNotfy.test` | 4 | 4 | 0 | 0 | 0 | — |
| `unknown.test` | 7 | 7 | 0 | 0 | 0 | — |
| `unload.test` | 27 | 27 | 0 | 0 | 0 | — |
| `uplevel.test` | 57 | 48 | 9 | 8 | 1 | 88.9% |
| `upvar.test` | 70 | 53 | 17 | 0 | 17 | 0.0% |
| `utf.test` | 399 | 316 | 83 | 66 | 17 | 79.5% |
| `utfext.test` | 842 | 842 | 0 | 0 | 0 | — |
| `util.test` | 462 | 340 | 122 | 122 | 0 | 100.0% |
| `var.test` | 221 | 171 | 50 | 7 | 43 | 14.0% |
| `while-old.test` | 15 | 0 | 15 | 10 | 5 | 66.7% |
| `while.test` | 46 | 0 | 46 | 1 | 45 | 2.2% |
| `winConsole.test` | 46 | 46 | 0 | 0 | 0 | — |
| `winDde.test` | 50 | 50 | 0 | 0 | 0 | — |
| `winFCmd.test` | 173 | 173 | 0 | 0 | 0 | — |
| `winFile.test` | 11 | 11 | 0 | 0 | 0 | — |
| `winNotify.test` | 14 | 14 | 0 | 0 | 0 | — |
| `winPipe.test` | 56 | 56 | 0 | 0 | 0 | — |
| `winTime.test` | 3 | 3 | 0 | 0 | 0 | — |
| `word.test` | 55 | 55 | 0 | 0 | 0 | — |
| `zipfs.test` | 528 | 528 | 0 | 0 | 0 | — |
| `zlib.test` | 74 | 73 | 1 | 0 | 1 | 0.0% |

## What the run could not reach

Every suite file was extracted to the end: no file contributed a partial set of cases.

The recorder only sees `test` calls made in the interpreter it runs in. These files created a child interpreter while being read, and any test they declare inside one was not extracted — their case counts are a floor, not a total.

| File | Child interpreters | Cases extracted |
| --- | ---: | ---: |
| `cmdAH.test` | 3 | 17001 |
| `init.test` | 1 | 10 |
| `interp.test` | 2 | 355 |
| `load.test` | 1 | 30 |
| `macOSXLoad.test` | 3 | 57 |
| `package.test` | 1 | 0 |
| `pkgMkIndex.test` | 1 | 27 |
| `timer.test` | 1 | 54 |
| `unload.test` | 2 | 27 |

3 files contributed no cases at all: `clock-no-tzdata.test`, `package.test`, `safe-stock86.test`. A file lands here when it is empty, when everything in it sits behind a constraint this configuration does not meet, or when it declares its tests inside a child interpreter.

A stage that goes 15s without producing an outcome is killed and the case it was on is recorded as an abort, so that one pathological body cannot stall the run. Aborts on the tclrs side count as failures rather than skips, and this run had 1 of them; aborts on the reference side are the `tclsh produced no reference outcome` skips above. That timeout is the only bound in the pipeline, and nothing is dropped without landing in one of those two counts.

Some suite cases depend on the clock, the file system, the environment or the network, so a rerun can move the totals by a few cases. Nothing else in the pipeline is nondeterministic: the case set, the ordering and the comparison are fixed.

## Reproducing this report

From a fresh checkout, with a `tclsh` on `PATH` and a stable Rust toolchain:

```sh
conformance/run.sh
```

That fetches the Tcl source release, verifies it against a pinned SHA-256, unpacks its `tests/` directory, and runs all four stages, rewriting this file. The intermediate artifacts are left under `conformance/work/` — one case file, one reference outcome file and one tclrs outcome file per suite file — so any number here can be traced back to the case that produced it.

A rerun reuses whatever is already in `conformance/work/`, which is what makes an interrupted run cheap to resume. To force everything to be recomputed, remove that directory first. Some suite bodies leave read-only directories behind in the per-file scratch areas, so a plain `rm -rf` can refuse:

```sh
find conformance/work -type d -exec chmod u+rwx {} +
rm -rf conformance/work
```

`TCLSH` selects the reference interpreter and `--jobs N` sets how many suite files are processed at once; neither changes the case set or the verdicts.
