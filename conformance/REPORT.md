# tclrs conformance against the official Tcl test suite

Reference interpreter: **tclsh 9.0.4**. Suite: `tcl9.0.4/tests` — the `tests/` directory of the matching Tcl source release, fetched and checksum-verified by `conformance/fetch-suite.sh`.

**1404 of 2941 attempted cases pass — 47.7%.** Over every case the suite contains, including the ones that cannot be run here, that is 1404 of 69424 — 2.0%.

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
| Skipped — cannot be run | 66483 | 95.8% |
| Attempted | 2941 | 4.2% |
| ⤷ passed | 1404 | 47.7% of attempted |
| ⤷ failed | 1537 | 52.3% of attempted |

Of the 1537 failures, 1165 are a feature tclrs documents as not built yet rather than a wrong answer. Counting those as skips instead would give 1404 of 1776 — 79.1% — and that looser number is stated here only so the choice of rule is visible. The headline above uses the strict rule.

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| tclrs has no such command | 48069 |
| tcltest constraint not met | 13663 |
| needs a command plain tclsh has not got | 4751 |

### Commands tclrs does not have, by how many cases they block

A case is attributed to the first command tclrs refused, so a body using several missing commands is counted once, against the first of them.

| Command | Cases |
| --- | ---: |
| `clock` | 17215 |
| `proc` | 8863 |
| `encoding` | 8512 |
| `apply` | 5540 |
| `catch` | 1898 |
| `binary` | 635 |
| `file` | 631 |
| `oo::class` | 379 |
| `namespace` | 342 |
| `open` | 309 |
| `string` | 291 |
| `format` | 283 |
| `interp` | 263 |
| `info` | 262 |
| `scan` | 233 |
| `zipfs` | 212 |
| `regexp` | 185 |
| `trace` | 162 |
| `regsub` | 119 |
| `lseq` | 112 |
| `glob` | 90 |
| `try` | 85 |
| `switch` | 77 |
| `oo::object` | 72 |
| `socket` | 69 |
| `for` | 68 |
| `subst` | 66 |
| `after` | 65 |
| `chan` | 56 |
| `ledit` | 51 |
| `return` | 48 |
| `variable` | 48 |
| `coroutine` | 38 |
| `tcl::prefix` | 36 |
| `append` | 31 |
| `safe::interpCreate` | 31 |
| `fpclassify` | 30 |
| `eval` | 29 |
| `lmap` | 29 |
| `child` | 24 |
| *89 further commands* | 580 |

## Why cases failed

| Cause | Cases | Share of failures | For example |
| --- | ---: | ---: | --- |
| tclrs raised an error, tclsh did not | 927 | 60.3% | `append.test` append-4.19, `append.test` append-4.20, `append.test` append-10.1 |
| both raised an error, messages differ | 574 | 37.3% | `append.test` append-6.1, `append.test` append-6.2, `append.test` append-10.2 |
| results differ | 31 | 2.0% | `append.test` append-3.5, `append.test` append-3.6, `compExpr-old.test` compExpr-old-1.8 |
| tclsh raised an error, tclrs did not | 4 | 0.3% | `if.test` if-1.17, `namespace-old.test` namespace-old-5.5, `var.test` var-1.7 |
| tclrs was killed or crashed | 1 | 0.1% | `expr.test` expr-36.14 |

Every failing case is written out in full — its program, the tclsh outcome and the tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this table.

### The most frequent failing messages

Error text with the quoted part elided and tclrs's trailing `(line N)` removed, so that one cause groups into one row.

| Message | Cases |
| --- | ---: |
| math function "…" is not supported yet | 323 |
| command name must be a literal in this phase | 231 |
| expression must be a literal in this phase | 120 |
| integer value too large to represent | 118 |
| invalid bare word "…" in expression | 102 |
| extra characters after expression: "…" | 47 |
| identical text apart from tclrs's trailing (line N) | 44 |
| lsort -dictionary is not supported yet | 32 |
| lsearch -stride is not supported yet | 30 |
| unexpected character '*' in expression | 29 |
| lsort -index is not supported yet | 27 |
| lsearch -index is not supported yet | 25 |
| dict filter is not supported yet | 23 |
| dict replace is not supported yet | 18 |
| script body must be a literal in this phase | 17 |
| array default is not supported yet | 15 |
| lsearch -subindices is not supported yet | 15 |
| dict map is not supported yet | 13 |
| dict update is not supported yet | 13 |
| lsearch -sorted is not supported yet | 13 |
| premature end of expression | 11 |
| dict getdef is not supported yet | 10 |
| dict getwithdefault is not supported yet | 10 |
| dict lappend is not supported yet | 10 |
| dict with is not supported yet | 10 |
| lsort -stride is not supported yet | 10 |
| dict append is not supported yet | 9 |
| array for is not supported yet | 8 |
| dict incr is not supported yet | 8 |
| dict unset is not supported yet | 8 |

## Command coverage

Independently of the suite: of the 109 commands the reference interpreter defines in the global namespace, tclrs answers to 25 — 22.9%. A name counts as answered when tclrs does not refuse it with `invalid command name`.

Implemented: `array`, `break`, `concat`, `continue`, `dict`, `expr`, `foreach`, `if`, `incr`, `join`, `lappend`, `lindex`, `linsert`, `list`, `llength`, `lrange`, `lreplace`, `lreverse`, `lsearch`, `lsort`, `puts`, `set`, `split`, `unset`, `while`

## Per file

| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `aaa_exit.test` | 2 | 2 | 0 | 0 | 0 | — |
| `abstractlist.test` | 123 | 123 | 0 | 0 | 0 | — |
| `append.test` | 52 | 23 | 29 | 19 | 10 | 65.5% |
| `appendComp.test` | 48 | 48 | 0 | 0 | 0 | — |
| `apply.test` | 42 | 42 | 0 | 0 | 0 | — |
| `assemble.test` | 283 | 283 | 0 | 0 | 0 | — |
| `assocd.test` | 11 | 11 | 0 | 0 | 0 | — |
| `async.test` | 12 | 12 | 0 | 0 | 0 | — |
| `autoMkindex.test` | 11 | 11 | 0 | 0 | 0 | — |
| `basic.test` | 147 | 147 | 0 | 0 | 0 | — |
| `bigdata.test` | 113 | 113 | 0 | 0 | 0 | — |
| `binary.test` | 750 | 750 | 0 | 0 | 0 | — |
| `brodnik.test` | 422 | 422 | 0 | 0 | 0 | — |
| `chan.test` | 42 | 42 | 0 | 0 | 0 | — |
| `chanio.test` | 779 | 779 | 0 | 0 | 0 | — |
| `clock-ivm.test` | 8744 | 8741 | 3 | 0 | 3 | 0.0% |
| `clock-no-tzdata.test` | 0 | 0 | 0 | 0 | 0 | — |
| `clock.test` | 8744 | 8741 | 3 | 0 | 3 | 0.0% |
| `cmdAH.test` | 17001 | 16996 | 5 | 3 | 2 | 60.0% |
| `cmdIL.test` | 168 | 63 | 105 | 28 | 77 | 26.7% |
| `cmdInfo.test` | 12 | 12 | 0 | 0 | 0 | — |
| `cmdMZ.test` | 97 | 85 | 12 | 9 | 3 | 75.0% |
| `compExpr-old.test` | 184 | 40 | 144 | 86 | 58 | 59.7% |
| `compExpr.test` | 82 | 20 | 62 | 39 | 23 | 62.9% |
| `compile.test` | 171 | 167 | 4 | 2 | 2 | 50.0% |
| `concat.test` | 9 | 1 | 8 | 8 | 0 | 100.0% |
| `config.test` | 9 | 9 | 0 | 0 | 0 | — |
| `coroutine.test` | 77 | 77 | 0 | 0 | 0 | — |
| `dcall.test` | 6 | 6 | 0 | 0 | 0 | — |
| `dict.test` | 373 | 113 | 260 | 80 | 180 | 30.8% |
| `dstring.test` | 46 | 46 | 0 | 0 | 0 | — |
| `encoding.test` | 232 | 229 | 3 | 3 | 0 | 100.0% |
| `env.test` | 32 | 31 | 1 | 0 | 1 | 0.0% |
| `error.test` | 317 | 317 | 0 | 0 | 0 | — |
| `eval.test` | 12 | 12 | 0 | 0 | 0 | — |
| `event.test` | 65 | 65 | 0 | 0 | 0 | — |
| `exec.test` | 145 | 145 | 0 | 0 | 0 | — |
| `execute.test` | 157 | 120 | 37 | 11 | 26 | 29.7% |
| `expr-old.test` | 461 | 129 | 332 | 236 | 96 | 71.1% |
| `expr.test` | 2168 | 1188 | 980 | 384 | 596 | 39.2% |
| `fCmd.test` | 306 | 306 | 0 | 0 | 0 | — |
| `fileName.test` | 306 | 306 | 0 | 0 | 0 | — |
| `fileSystem.test` | 140 | 140 | 0 | 0 | 0 | — |
| `fileSystemEncoding.test` | 1 | 1 | 0 | 0 | 0 | — |
| `for-old.test` | 9 | 9 | 0 | 0 | 0 | — |
| `for.test` | 88 | 82 | 6 | 0 | 6 | 0.0% |
| `foreach.test` | 43 | 40 | 3 | 3 | 0 | 100.0% |
| `format.test` | 269 | 269 | 0 | 0 | 0 | — |
| `get.test` | 23 | 23 | 0 | 0 | 0 | — |
| `history.test` | 62 | 57 | 5 | 5 | 0 | 100.0% |
| `http.test` | 528 | 528 | 0 | 0 | 0 | — |
| `http11.test` | 147 | 147 | 0 | 0 | 0 | — |
| `httpPipeline.test` | 5988 | 5988 | 0 | 0 | 0 | — |
| `httpProxy.test` | 150 | 150 | 0 | 0 | 0 | — |
| `httpcookie.test` | 60 | 60 | 0 | 0 | 0 | — |
| `icu.test` | 58 | 58 | 0 | 0 | 0 | — |
| `if-old.test` | 33 | 11 | 22 | 17 | 5 | 77.3% |
| `if.test` | 73 | 35 | 38 | 0 | 38 | 0.0% |
| `incr-old.test` | 14 | 10 | 4 | 4 | 0 | 100.0% |
| `incr.test` | 69 | 20 | 49 | 15 | 34 | 30.6% |
| `indexObj.test` | 65 | 65 | 0 | 0 | 0 | — |
| `info.test` | 287 | 283 | 4 | 0 | 4 | 0.0% |
| `init.test` | 10 | 10 | 0 | 0 | 0 | — |
| `interp.test` | 355 | 355 | 0 | 0 | 0 | — |
| `io.test` | 884 | 884 | 0 | 0 | 0 | — |
| `ioCmd.test` | 377 | 377 | 0 | 0 | 0 | — |
| `ioTrans.test` | 106 | 106 | 0 | 0 | 0 | — |
| `iogt.test` | 17 | 17 | 0 | 0 | 0 | — |
| `join.test` | 10 | 5 | 5 | 5 | 0 | 100.0% |
| `lindex.test` | 84 | 77 | 7 | 6 | 1 | 85.7% |
| `link.test` | 77 | 77 | 0 | 0 | 0 | — |
| `linsert.test` | 28 | 6 | 22 | 22 | 0 | 100.0% |
| `list.test` | 78 | 8 | 70 | 68 | 2 | 97.1% |
| `listObj.test` | 59 | 29 | 30 | 30 | 0 | 100.0% |
| `listRep.test` | 231 | 227 | 4 | 4 | 0 | 100.0% |
| `llength.test` | 6 | 3 | 3 | 3 | 0 | 100.0% |
| `lmap.test` | 66 | 66 | 0 | 0 | 0 | — |
| `load.test` | 30 | 30 | 0 | 0 | 0 | — |
| `lpop.test` | 19 | 19 | 0 | 0 | 0 | — |
| `lrange.test` | 1766 | 1742 | 24 | 22 | 2 | 91.7% |
| `lrepeat.test` | 12 | 12 | 0 | 0 | 0 | — |
| `lreplace.test` | 3579 | 3532 | 47 | 47 | 0 | 100.0% |
| `lsearch.test` | 165 | 18 | 147 | 45 | 102 | 30.6% |
| `lseq.test` | 136 | 136 | 0 | 0 | 0 | — |
| `lset.test` | 89 | 89 | 0 | 0 | 0 | — |
| `lsetComp.test` | 19 | 19 | 0 | 0 | 0 | — |
| `macOSXFCmd.test` | 14 | 14 | 0 | 0 | 0 | — |
| `macOSXLoad.test` | 57 | 57 | 0 | 0 | 0 | — |
| `main.test` | 67 | 67 | 0 | 0 | 0 | — |
| `mathop.test` | 385 | 267 | 118 | 0 | 118 | 0.0% |
| `misc.test` | 301 | 301 | 0 | 0 | 0 | — |
| `msgcat.test` | 135 | 135 | 0 | 0 | 0 | — |
| `mutex.test` | 12 | 12 | 0 | 0 | 0 | — |
| `namespace-old.test` | 126 | 125 | 1 | 0 | 1 | 0.0% |
| `namespace.test` | 314 | 314 | 0 | 0 | 0 | — |
| `notify.test` | 23 | 23 | 0 | 0 | 0 | — |
| `nre.test` | 28 | 28 | 0 | 0 | 0 | — |
| `obj.test` | 84 | 84 | 0 | 0 | 0 | — |
| `oo.test` | 388 | 386 | 2 | 0 | 2 | 0.0% |
| `ooNext2.test` | 62 | 62 | 0 | 0 | 0 | — |
| `ooProp.test` | 55 | 55 | 0 | 0 | 0 | — |
| `ooUtil.test` | 33 | 33 | 0 | 0 | 0 | — |
| `opt.test` | 31 | 31 | 0 | 0 | 0 | — |
| `package.test` | 0 | 0 | 0 | 0 | 0 | — |
| `parse.test` | 271 | 271 | 0 | 0 | 0 | — |
| `parseExpr.test` | 286 | 221 | 65 | 3 | 62 | 4.6% |
| `parseOld.test` | 158 | 62 | 96 | 95 | 1 | 99.0% |
| `pid.test` | 5 | 5 | 0 | 0 | 0 | — |
| `pkgMkIndex.test` | 27 | 27 | 0 | 0 | 0 | — |
| `platform.test` | 9 | 8 | 1 | 0 | 1 | 0.0% |
| `proc-old.test` | 74 | 74 | 0 | 0 | 0 | — |
| `proc.test` | 38 | 38 | 0 | 0 | 0 | — |
| `process.test` | 18 | 18 | 0 | 0 | 0 | — |
| `pwd.test` | 3 | 3 | 0 | 0 | 0 | — |
| `reg.test` | 1141 | 1141 | 0 | 0 | 0 | — |
| `regexp.test` | 257 | 257 | 0 | 0 | 0 | — |
| `regexpComp.test` | 179 | 179 | 0 | 0 | 0 | — |
| `registry.test` | 125 | 125 | 0 | 0 | 0 | — |
| `rename.test` | 19 | 18 | 1 | 1 | 0 | 100.0% |
| `resolver.test` | 10 | 10 | 0 | 0 | 0 | — |
| `result.test` | 26 | 26 | 0 | 0 | 0 | — |
| `safe-stock.test` | 11 | 11 | 0 | 0 | 0 | — |
| `safe-stock86.test` | 0 | 0 | 0 | 0 | 0 | — |
| `safe-zipfs.test` | 22 | 22 | 0 | 0 | 0 | — |
| `safe.test` | 155 | 155 | 0 | 0 | 0 | — |
| `scan.test` | 185 | 185 | 0 | 0 | 0 | — |
| `security.test` | 1 | 1 | 0 | 0 | 0 | — |
| `set-old.test` | 153 | 146 | 7 | 7 | 0 | 100.0% |
| `set.test` | 64 | 36 | 28 | 11 | 17 | 39.3% |
| `socket.test` | 189 | 189 | 0 | 0 | 0 | — |
| `source.test` | 23 | 23 | 0 | 0 | 0 | — |
| `split.test` | 18 | 4 | 14 | 14 | 0 | 100.0% |
| `stack.test` | 3 | 3 | 0 | 0 | 0 | — |
| `string.test` | 705 | 703 | 2 | 1 | 1 | 50.0% |
| `stringObj.test` | 81 | 81 | 0 | 0 | 0 | — |
| `subst.test` | 63 | 63 | 0 | 0 | 0 | — |
| `switch.test` | 113 | 113 | 0 | 0 | 0 | — |
| `tailcall.test` | 37 | 37 | 0 | 0 | 0 | — |
| `tcltest.test` | 127 | 119 | 8 | 7 | 1 | 87.5% |
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
| `uplevel.test` | 57 | 49 | 8 | 8 | 0 | 100.0% |
| `upvar.test` | 70 | 70 | 0 | 0 | 0 | — |
| `utf.test` | 399 | 396 | 3 | 3 | 0 | 100.0% |
| `utfext.test` | 842 | 842 | 0 | 0 | 0 | — |
| `util.test` | 462 | 421 | 41 | 41 | 0 | 100.0% |
| `var.test` | 221 | 189 | 32 | 1 | 31 | 3.1% |
| `while-old.test` | 15 | 6 | 9 | 8 | 1 | 88.9% |
| `while.test` | 46 | 21 | 25 | 0 | 25 | 0.0% |
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
