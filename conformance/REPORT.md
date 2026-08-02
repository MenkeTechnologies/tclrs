# tclrs conformance against the official Tcl test suite

Reference interpreter: **tclsh 9.0.4**. Suite: `tcl9.0.4/tests` — the `tests/` directory of the matching Tcl source release, fetched and checksum-verified by `conformance/fetch-suite.sh`.

**7590 of 25553 attempted cases pass — 29.7%.** Over every case the suite contains, including the ones that cannot be run here, that is 7590 of 69424 — 10.9%.

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
| Skipped — cannot be run | 43871 | 63.2% |
| Attempted | 25553 | 36.8% |
| ⤷ passed | 7590 | 29.7% of attempted |
| ⤷ failed | 17963 | 70.3% of attempted |

Of the 17963 failures, 15408 are a feature tclrs documents as not built yet rather than a wrong answer. Counting those as skips instead would give 7590 of 10145 — 74.8% — and that looser number is stated here only so the choice of rule is visible. The headline above uses the strict rule.

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| tclrs has no such command | 25456 |
| tcltest constraint not met | 13663 |
| needs a command plain tclsh has not got | 4751 |
| tclsh produced no reference outcome | 1 |

### Commands tclrs does not have, by how many cases they block

A case is attributed to the first command tclrs refused, so a body using several missing commands is counted once, against the first of them.

| Command | Cases |
| --- | ---: |
| `encoding` | 16856 |
| `apply` | 5552 |
| `binary` | 658 |
| `namespace` | 364 |
| `interp` | 328 |
| `trace` | 210 |
| `scan` | 174 |
| `oo::class` | 158 |
| `subst` | 101 |
| `try` | 86 |
| `socket` | 80 |
| `chan` | 63 |
| `after` | 58 |
| `zipfs` | 53 |
| `open` | 39 |
| `tcl::prefix` | 36 |
| `upvar` | 36 |
| `oo::object` | 35 |
| `rename` | 34 |
| `fpclassify` | 30 |
| `variable` | 27 |
| `exec` | 24 |
| `history` | 23 |
| `safe::interpCreate` | 23 |
| `tcl::unsupported::getbytecode` | 22 |
| `tcl::unsupported::disassemble` | 20 |
| `::apply` | 17 |
| `::tcl::tm::path` | 17 |
| `load` | 16 |
| `timerate` | 14 |
| `unload` | 14 |
| `uplevel` | 14 |
| `tcl::unsupported::representation` | 13 |
| `zlib` | 13 |
| `package` | 12 |
| `tcl_startOfNextWord` | 12 |
| `tcl_endOfWord` | 11 |
| `tcl_startOfPreviousWord` | 11 |
| `tcl_wordBreakAfter` | 11 |
| `fconfigure` | 10 |
| *54 further commands* | 181 |

## Why cases failed

| Cause | Cases | Share of failures | For example |
| --- | ---: | ---: | --- |
| tclrs raised an error, tclsh did not | 10423 | 58.0% | `append.test` append-4.19, `append.test` append-4.20, `append.test` append-7.1 |
| both raised an error, messages differ | 6669 | 37.1% | `append.test` append-3.1, `append.test` append-3.2, `append.test` append-6.1 |
| results differ | 778 | 4.3% | `append.test` append-3.4, `append.test` append-3.5, `append.test` append-3.6 |
| tclsh raised an error, tclrs did not | 82 | 0.5% | `clock-ivm.test` clock-11.1.vm:0, `clock-ivm.test` clock-11.2.vm:0, `clock-ivm.test` clock-11.3.vm:0 |
| tclrs was killed or crashed | 11 | 0.1% | `clock-ivm.test` clock-6.0.vm:0, `clock.test` clock-6.0.vm:1, `lseq.test` lseq-3.34 |

Every failing case is written out in full — its program, the tclsh outcome and the tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this table.

### The most frequent failing messages

Error text with the quoted part elided and tclrs's trailing `(line N)` removed, so that one cause groups into one row.

| Message | Cases |
| --- | ---: |
| clock: the locale "…" is not supported yet; only the root locale is built in | 13374 |
| unknown or unsupported subcommand "…": only "…" is supported | 561 |
| command name must be a literal in this phase | 539 |
| can't read "…": no such variable | 212 |
| "…" outside of a procedure is not supported | 202 |
| {*} argument expansion is not supported yet | 198 |
| wrong # args: should be "…"; the options variable is not supported | 185 |
| clock scan: the free-form parser is not supported yet; use -format | 160 |
| clock scan: -base is not supported yet | 157 |
| expression must be a literal in this phase | 150 |
| invalid bareword "…" | 109 |
| identical text apart from tclrs's trailing (line N) | 82 |
| clock scan: the format token "…" is not supported yet | 59 |
| missing operand at _@_ | 52 |
| unable to convert input string: ambiguous day | 48 |
| script body must be a literal in this phase | 47 |
| dict getdef is not supported yet | 45 |
| file attributes is not supported yet: it needs an interface this frontend has not built | 43 |
| time zone "…" not found: no zone file names it, and a POSIX time zone rule is not supported yet | 36 |
| lsearch -index is not supported yet | 35 |
| lsort -dictionary is not supported yet | 34 |
| lsort -index is not supported yet | 33 |
| input string does not match supplied format | 30 |
| array startsearch is not supported yet | 29 |
| lsearch -subindices is not supported yet | 29 |
| integer value too large to represent | 25 |
| dict filter is not supported yet | 24 |
| file link is not supported yet: it needs an interface this frontend has not built | 24 |
| this command does not take an array element yet | 22 |
| "…" is only supported at the top level of a script | 21 |

## Command coverage

Independently of the suite: of the 109 commands the reference interpreter defines in the global namespace, tclrs answers to 55 — 50.5%. A name counts as answered when tclrs does not refuse it with `invalid command name`.

Implemented: `append`, `array`, `break`, `catch`, `cd`, `clock`, `concat`, `continue`, `coroutine`, `dict`, `error`, `eval`, `expr`, `file`, `for`, `foreach`, `format`, `glob`, `global`, `if`, `incr`, `info`, `join`, `lappend`, `lassign`, `ledit`, `lindex`, `linsert`, `list`, `llength`, `lmap`, `lpop`, `lrange`, `lremove`, `lrepeat`, `lreplace`, `lreverse`, `lsearch`, `lseq`, `lset`, `lsort`, `proc`, `puts`, `pwd`, `regexp`, `regsub`, `return`, `set`, `split`, `string`, `switch`, `unset`, `while`, `yield`, `yieldto`

## Per file

| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `aaa_exit.test` | 2 | 2 | 0 | 0 | 0 | — |
| `abstractlist.test` | 123 | 123 | 0 | 0 | 0 | — |
| `append.test` | 52 | 3 | 49 | 26 | 23 | 53.1% |
| `appendComp.test` | 48 | 11 | 37 | 22 | 15 | 59.5% |
| `apply.test` | 42 | 36 | 6 | 0 | 6 | 0.0% |
| `assemble.test` | 283 | 235 | 48 | 2 | 46 | 4.2% |
| `assocd.test` | 11 | 11 | 0 | 0 | 0 | — |
| `async.test` | 12 | 12 | 0 | 0 | 0 | — |
| `autoMkindex.test` | 11 | 9 | 2 | 1 | 1 | 50.0% |
| `basic.test` | 147 | 131 | 16 | 2 | 14 | 12.5% |
| `bigdata.test` | 113 | 113 | 0 | 0 | 0 | — |
| `binary.test` | 750 | 747 | 3 | 0 | 3 | 0.0% |
| `brodnik.test` | 422 | 422 | 0 | 0 | 0 | — |
| `chan.test` | 42 | 40 | 2 | 0 | 2 | 0.0% |
| `chanio.test` | 779 | 444 | 335 | 309 | 26 | 92.2% |
| `clock-ivm.test` | 8744 | 70 | 8674 | 1454 | 7220 | 16.8% |
| `clock-no-tzdata.test` | 0 | 0 | 0 | 0 | 0 | — |
| `clock.test` | 8744 | 82 | 8662 | 1454 | 7208 | 16.8% |
| `cmdAH.test` | 17001 | 16889 | 112 | 52 | 60 | 46.4% |
| `cmdIL.test` | 168 | 33 | 135 | 49 | 86 | 36.3% |
| `cmdInfo.test` | 12 | 12 | 0 | 0 | 0 | — |
| `cmdMZ.test` | 97 | 37 | 60 | 26 | 34 | 43.3% |
| `compExpr-old.test` | 184 | 4 | 180 | 112 | 68 | 62.2% |
| `compExpr.test` | 82 | 7 | 75 | 62 | 13 | 82.7% |
| `compile.test` | 171 | 139 | 32 | 6 | 26 | 18.8% |
| `concat.test` | 9 | 0 | 9 | 9 | 0 | 100.0% |
| `config.test` | 9 | 3 | 6 | 0 | 6 | 0.0% |
| `coroutine.test` | 77 | 48 | 29 | 2 | 27 | 6.9% |
| `dcall.test` | 6 | 6 | 0 | 0 | 0 | — |
| `dict.test` | 373 | 85 | 288 | 85 | 203 | 29.5% |
| `dstring.test` | 46 | 46 | 0 | 0 | 0 | — |
| `encoding.test` | 232 | 219 | 13 | 4 | 9 | 30.8% |
| `env.test` | 32 | 28 | 4 | 0 | 4 | 0.0% |
| `error.test` | 317 | 121 | 196 | 11 | 185 | 5.6% |
| `eval.test` | 12 | 0 | 12 | 11 | 1 | 91.7% |
| `event.test` | 65 | 51 | 14 | 0 | 14 | 0.0% |
| `exec.test` | 145 | 140 | 5 | 0 | 5 | 0.0% |
| `execute.test` | 157 | 97 | 60 | 36 | 24 | 60.0% |
| `expr-old.test` | 461 | 31 | 430 | 383 | 47 | 89.1% |
| `expr.test` | 2168 | 1095 | 1073 | 849 | 224 | 79.1% |
| `fCmd.test` | 306 | 220 | 86 | 54 | 32 | 62.8% |
| `fileName.test` | 306 | 198 | 108 | 70 | 38 | 64.8% |
| `fileSystem.test` | 140 | 83 | 57 | 38 | 19 | 66.7% |
| `fileSystemEncoding.test` | 1 | 1 | 0 | 0 | 0 | — |
| `for-old.test` | 9 | 0 | 9 | 7 | 2 | 77.8% |
| `for.test` | 88 | 39 | 49 | 14 | 35 | 28.6% |
| `foreach.test` | 43 | 3 | 40 | 31 | 9 | 77.5% |
| `format.test` | 269 | 1 | 268 | 261 | 7 | 97.4% |
| `get.test` | 23 | 17 | 6 | 6 | 0 | 100.0% |
| `history.test` | 62 | 25 | 37 | 18 | 19 | 48.6% |
| `http.test` | 528 | 501 | 27 | 9 | 18 | 33.3% |
| `http11.test` | 147 | 147 | 0 | 0 | 0 | — |
| `httpPipeline.test` | 5988 | 5988 | 0 | 0 | 0 | — |
| `httpProxy.test` | 150 | 150 | 0 | 0 | 0 | — |
| `httpcookie.test` | 60 | 56 | 4 | 0 | 4 | 0.0% |
| `icu.test` | 58 | 58 | 0 | 0 | 0 | — |
| `if-old.test` | 33 | 0 | 33 | 20 | 13 | 60.6% |
| `if.test` | 73 | 3 | 70 | 2 | 68 | 2.9% |
| `incr-old.test` | 14 | 1 | 13 | 7 | 6 | 53.8% |
| `incr.test` | 69 | 2 | 67 | 22 | 45 | 32.8% |
| `indexObj.test` | 65 | 65 | 0 | 0 | 0 | — |
| `info.test` | 287 | 143 | 144 | 0 | 144 | 0.0% |
| `init.test` | 10 | 10 | 0 | 0 | 0 | — |
| `interp.test` | 355 | 298 | 57 | 0 | 57 | 0.0% |
| `io.test` | 884 | 492 | 392 | 360 | 32 | 91.8% |
| `ioCmd.test` | 377 | 292 | 85 | 0 | 85 | 0.0% |
| `ioTrans.test` | 106 | 104 | 2 | 0 | 2 | 0.0% |
| `iogt.test` | 17 | 17 | 0 | 0 | 0 | — |
| `join.test` | 10 | 0 | 10 | 7 | 3 | 70.0% |
| `lindex.test` | 84 | 38 | 46 | 46 | 0 | 100.0% |
| `link.test` | 77 | 77 | 0 | 0 | 0 | — |
| `linsert.test` | 28 | 0 | 28 | 28 | 0 | 100.0% |
| `list.test` | 78 | 1 | 77 | 75 | 2 | 97.4% |
| `listObj.test` | 59 | 17 | 42 | 42 | 0 | 100.0% |
| `listRep.test` | 231 | 227 | 4 | 4 | 0 | 100.0% |
| `llength.test` | 6 | 0 | 6 | 6 | 0 | 100.0% |
| `lmap.test` | 66 | 33 | 33 | 20 | 13 | 60.6% |
| `load.test` | 30 | 30 | 0 | 0 | 0 | — |
| `lpop.test` | 19 | 2 | 17 | 16 | 1 | 94.1% |
| `lrange.test` | 1766 | 1731 | 35 | 29 | 6 | 82.9% |
| `lrepeat.test` | 12 | 1 | 11 | 10 | 1 | 90.9% |
| `lreplace.test` | 3579 | 3461 | 118 | 115 | 3 | 97.5% |
| `lsearch.test` | 165 | 0 | 165 | 65 | 100 | 39.4% |
| `lseq.test` | 136 | 22 | 114 | 81 | 33 | 71.1% |
| `lset.test` | 89 | 89 | 0 | 0 | 0 | — |
| `lsetComp.test` | 19 | 19 | 0 | 0 | 0 | — |
| `macOSXFCmd.test` | 14 | 1 | 13 | 0 | 13 | 0.0% |
| `macOSXLoad.test` | 57 | 57 | 0 | 0 | 0 | — |
| `main.test` | 67 | 64 | 3 | 3 | 0 | 100.0% |
| `mathop.test` | 385 | 222 | 163 | 13 | 150 | 8.0% |
| `misc.test` | 301 | 299 | 2 | 0 | 2 | 0.0% |
| `msgcat.test` | 135 | 134 | 1 | 0 | 1 | 0.0% |
| `mutex.test` | 12 | 12 | 0 | 0 | 0 | — |
| `namespace-old.test` | 126 | 85 | 41 | 7 | 34 | 17.1% |
| `namespace.test` | 314 | 210 | 104 | 0 | 104 | 0.0% |
| `notify.test` | 23 | 23 | 0 | 0 | 0 | — |
| `nre.test` | 28 | 24 | 4 | 0 | 4 | 0.0% |
| `obj.test` | 84 | 76 | 8 | 7 | 1 | 87.5% |
| `oo.test` | 388 | 192 | 196 | 0 | 196 | 0.0% |
| `ooNext2.test` | 62 | 9 | 53 | 0 | 53 | 0.0% |
| `ooProp.test` | 55 | 27 | 28 | 0 | 28 | 0.0% |
| `ooUtil.test` | 33 | 12 | 21 | 0 | 21 | 0.0% |
| `opt.test` | 31 | 26 | 5 | 3 | 2 | 60.0% |
| `package.test` | 0 | 0 | 0 | 0 | 0 | — |
| `parse.test` | 271 | 201 | 70 | 5 | 65 | 7.1% |
| `parseExpr.test` | 286 | 219 | 67 | 3 | 64 | 4.5% |
| `parseOld.test` | 158 | 9 | 149 | 134 | 15 | 89.9% |
| `pid.test` | 5 | 3 | 2 | 0 | 2 | 0.0% |
| `pkgMkIndex.test` | 27 | 27 | 0 | 0 | 0 | — |
| `platform.test` | 9 | 8 | 1 | 0 | 1 | 0.0% |
| `proc-old.test` | 74 | 15 | 59 | 42 | 17 | 71.2% |
| `proc.test` | 38 | 11 | 27 | 1 | 26 | 3.7% |
| `process.test` | 18 | 18 | 0 | 0 | 0 | — |
| `pwd.test` | 3 | 0 | 3 | 2 | 1 | 66.7% |
| `reg.test` | 1141 | 1107 | 34 | 21 | 13 | 61.8% |
| `regexp.test` | 257 | 7 | 250 | 217 | 33 | 86.8% |
| `regexpComp.test` | 179 | 150 | 29 | 25 | 4 | 86.2% |
| `registry.test` | 125 | 125 | 0 | 0 | 0 | — |
| `rename.test` | 19 | 12 | 7 | 3 | 4 | 42.9% |
| `resolver.test` | 10 | 10 | 0 | 0 | 0 | — |
| `result.test` | 26 | 22 | 4 | 0 | 4 | 0.0% |
| `safe-stock.test` | 11 | 5 | 6 | 0 | 6 | 0.0% |
| `safe-stock86.test` | 0 | 0 | 0 | 0 | 0 | — |
| `safe-zipfs.test` | 22 | 1 | 21 | 1 | 20 | 4.8% |
| `safe.test` | 155 | 71 | 84 | 0 | 84 | 0.0% |
| `scan.test` | 185 | 172 | 13 | 0 | 13 | 0.0% |
| `security.test` | 1 | 1 | 0 | 0 | 0 | — |
| `set-old.test` | 153 | 6 | 147 | 80 | 67 | 54.4% |
| `set.test` | 64 | 4 | 60 | 26 | 34 | 43.3% |
| `socket.test` | 189 | 173 | 16 | 2 | 14 | 12.5% |
| `source.test` | 23 | 23 | 0 | 0 | 0 | — |
| `split.test` | 18 | 0 | 18 | 16 | 2 | 88.9% |
| `stack.test` | 3 | 3 | 0 | 0 | 0 | — |
| `string.test` | 705 | 600 | 105 | 98 | 7 | 93.3% |
| `stringObj.test` | 81 | 81 | 0 | 0 | 0 | — |
| `subst.test` | 63 | 51 | 12 | 0 | 12 | 0.0% |
| `switch.test` | 113 | 54 | 59 | 9 | 50 | 15.3% |
| `tailcall.test` | 37 | 30 | 7 | 0 | 7 | 0.0% |
| `tcltest.test` | 127 | 56 | 71 | 53 | 18 | 74.6% |
| `thread.test` | 52 | 52 | 0 | 0 | 0 | — |
| `timer.test` | 54 | 40 | 14 | 0 | 14 | 0.0% |
| `tm.test` | 21 | 19 | 2 | 0 | 2 | 0.0% |
| `trace.test` | 290 | 214 | 76 | 0 | 76 | 0.0% |
| `unixFCmd.test` | 49 | 25 | 24 | 0 | 24 | 0.0% |
| `unixFile.test` | 7 | 7 | 0 | 0 | 0 | — |
| `unixForkEvent.test` | 1 | 1 | 0 | 0 | 0 | — |
| `unixInit.test` | 8 | 7 | 1 | 0 | 1 | 0.0% |
| `unixNotfy.test` | 4 | 4 | 0 | 0 | 0 | — |
| `unknown.test` | 7 | 5 | 2 | 1 | 1 | 50.0% |
| `unload.test` | 27 | 27 | 0 | 0 | 0 | — |
| `uplevel.test` | 57 | 49 | 8 | 8 | 0 | 100.0% |
| `upvar.test` | 70 | 57 | 13 | 0 | 13 | 0.0% |
| `utf.test` | 399 | 251 | 148 | 131 | 17 | 88.5% |
| `utfext.test` | 842 | 842 | 0 | 0 | 0 | — |
| `util.test` | 462 | 340 | 122 | 122 | 0 | 100.0% |
| `var.test` | 221 | 154 | 67 | 9 | 58 | 13.4% |
| `while-old.test` | 15 | 0 | 15 | 13 | 2 | 86.7% |
| `while.test` | 46 | 0 | 46 | 1 | 45 | 2.2% |
| `winConsole.test` | 46 | 46 | 0 | 0 | 0 | — |
| `winDde.test` | 50 | 50 | 0 | 0 | 0 | — |
| `winFCmd.test` | 173 | 173 | 0 | 0 | 0 | — |
| `winFile.test` | 11 | 11 | 0 | 0 | 0 | — |
| `winNotify.test` | 14 | 14 | 0 | 0 | 0 | — |
| `winPipe.test` | 56 | 56 | 0 | 0 | 0 | — |
| `winTime.test` | 3 | 3 | 0 | 0 | 0 | — |
| `word.test` | 55 | 55 | 0 | 0 | 0 | — |
| `zipfs.test` | 528 | 324 | 204 | 104 | 100 | 51.0% |
| `zlib.test` | 74 | 53 | 21 | 0 | 21 | 0.0% |

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

A stage that goes 15s without producing an outcome is killed and the case it was on is recorded as an abort, so that one pathological body cannot stall the run. Aborts on the tclrs side count as failures rather than skips, and this run had 11 of them; aborts on the reference side are the `tclsh produced no reference outcome` skips above. That timeout is the only bound in the pipeline, and nothing is dropped without landing in one of those two counts.

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
