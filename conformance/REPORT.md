# tclrs conformance against the official Tcl test suite

Reference interpreter: **tclsh 9.0.4**. Suite: `tcl9.0.4/tests` — the `tests/` directory of the matching Tcl source release, fetched and checksum-verified by `conformance/fetch-suite.sh`.

**12870 of 31514 attempted cases pass — 40.8%.** Over every case the suite contains, including the ones that cannot be run here, that is 12870 of 69424 — 18.5%.

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

Everything else is attempted, and anything attempted either matches or fails. A *feature* tclrs declines inside a command it does have — a missing math function, an `lsort` option, an integer too wide for `i64` — counts as a failure, not a skip. Those failures are also counted on their own below, so the effect of the looser rule is visible rather than assumed.

Three things about the extraction are worth stating plainly. First, suite files set variables at their top level and then write bodies that read them, so each case carries the global variables its file had created by the time the test was declared, replayed ahead of the body as `set` and `array set` commands. Only variables whose name appears in the case's own text are carried — without that a file which builds a large table at its top level would attach a copy of it to every one of its cases — and both runs get exactly the same program, so whatever is left out is left out of both. Second, procs are not replayed and bodies are not executed during extraction, so a case that depends on a helper proc or on state an earlier body would have produced fails under tclsh too, and is skipped as needing an unavailable command rather than counted against tclrs. Third, `-cleanup` scripts are not run: they execute after the value under test is produced and cannot change it.

## Totals

| | Cases | Share |
| --- | ---: | ---: |
| Extracted from the suite | 69424 | 100% |
| Skipped — cannot be run | 37910 | 54.6% |
| Attempted | 31514 | 45.4% |
| ⤷ passed | 12870 | 40.8% of attempted |
| ⤷ failed | 18644 | 59.2% of attempted |

Of the 18644 failures, 16187 are a feature tclrs documents as not built yet rather than a wrong answer. Counting those as skips instead would give 12870 of 15327 — 84.0% — and that looser number is stated here only so the choice of rule is visible. The headline above uses the strict rule.

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| tclrs has no such command | 19495 |
| tcltest constraint not met | 13663 |
| needs a command plain tclsh has not got | 4751 |
| tclsh produced no reference outcome | 1 |

### Commands tclrs does not have, by how many cases they block

A case is attributed to the first command tclrs refused, so a body using several missing commands is counted once, against the first of them.

| Command | Cases |
| --- | ---: |
| `encoding` | 16856 |
| `binary` | 670 |
| `interp` | 340 |
| `trace` | 245 |
| `scan` | 177 |
| `oo::class` | 171 |
| `socket` | 115 |
| `subst` | 103 |
| `try` | 91 |
| `chan` | 70 |
| `zipfs` | 54 |
| `oo::object` | 36 |
| `tcl::prefix` | 36 |
| `fpclassify` | 30 |
| `safe::interpCreate` | 26 |
| `exec` | 24 |
| `ns` | 24 |
| `history` | 23 |
| `tcl::unsupported::getbytecode` | 22 |
| `tcl::unsupported::disassemble` | 20 |
| `const` | 19 |
| `::tcl::tm::path` | 17 |
| `::apply` | 16 |
| `load` | 16 |
| `timerate` | 14 |
| `unload` | 14 |
| `tcl::unsupported::representation` | 13 |
| `zlib` | 13 |
| `tcl_startOfNextWord` | 12 |
| `tailcall` | 11 |
| `tcl_endOfWord` | 11 |
| `tcl_startOfPreviousWord` | 11 |
| `tcl_wordBreakAfter` | 11 |
| `tcl_wordBreakBefore` | 10 |
| `::tcl::mathfunc::abs` | 9 |
| `::tcl::mathop::ge` | 9 |
| `::tcl::mathop::gt` | 9 |
| `::tcl::mathop::le` | 9 |
| `::tcl::mathop::lt` | 9 |
| `auto_qualify` | 8 |
| *50 further commands* | 121 |

## Why cases failed

| Cause | Cases | Share of failures | For example |
| --- | ---: | ---: | --- |
| tclrs raised an error, tclsh did not | 11016 | 59.1% | `append.test` append-4.19, `append.test` append-4.20, `append.test` append-7.1 |
| both raised an error, messages differ | 6684 | 35.9% | `append.test` append-3.1, `append.test` append-3.2, `append.test` append-6.1 |
| results differ | 824 | 4.4% | `append.test` append-3.4, `append.test` append-3.5, `append.test` append-3.6 |
| tclsh raised an error, tclrs did not | 98 | 0.5% | `appendComp.test` appendComp-10.4, `clock-ivm.test` clock-11.1.vm:0, `clock-ivm.test` clock-11.2.vm:0 |
| tclrs was killed or crashed | 22 | 0.1% | `clock-ivm.test` clock-6.0.vm:0, `clock.test` clock-6.0.vm:1, `compile.test` compile-21.1 |

Every failing case is written out in full — its program, the tclsh outcome and the tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this table.

### The most frequent failing messages

Error text with the quoted part elided and tclrs's trailing `(line N)` removed, so that one cause groups into one row.

| Message | Cases |
| --- | ---: |
| clock: the locale "…" is not supported yet; only the root locale is built in | 13374 |
| command name must be a literal in this phase | 1150 |
| can't read "…": no such variable | 260 |
| "…" outside of a procedure is not supported | 207 |
| wrong # args: should be "…"; the options variable is not supported | 193 |
| clock scan: the free-form parser is not supported yet; use -format | 160 |
| clock scan: -base is not supported yet | 157 |
| expression must be a literal in this phase | 150 |
| identical text apart from tclrs's trailing (line N) | 128 |
| invalid bareword "…" | 110 |
| info object is not supported yet | 75 |
| clock scan: the format token "…" is not supported yet | 59 |
| info class is not supported yet | 55 |
| missing operand at _@_ | 52 |
| script body must be a literal in this phase | 50 |
| unable to convert input string: ambiguous day | 48 |
| dict getdef is not supported yet | 47 |
| file attributes is not supported yet: it needs an interface this frontend has not built | 43 |
| "…" is not supported yet: this frontend resolves namespaces while compiling, so the name has to be written out | 42 |
| "…" is not supported: only "…" — a link to a global — can be bound while the script is read, and a link to a procedure's frame slots cannot be expressed at all | 37 |
| "…" of a computed lambda is not supported: the body would be compiled as a chunk of its own, which cannot reach frame slots | 36 |
| time zone "…" not found: no zone file names it, and a POSIX time zone rule is not supported yet | 36 |
| lsearch -index is not supported yet | 35 |
| lsort -dictionary is not supported yet | 35 |
| lsort -index is not supported yet | 33 |
| "…" with no level is not supported: the default level 1 is the caller's frame, whose variables are slots this frontend cannot address by name | 32 |
| array startsearch is not supported yet | 31 |
| input string does not match supplied format | 30 |
| lsearch -subindices is not supported yet | 29 |
| variable name must be a literal in this phase | 29 |

## Command coverage

Independently of the suite: of the 109 commands the reference interpreter defines in the global namespace, tclrs answers to 75 — 68.8%. A name counts as answered when tclrs does not refuse it with `invalid command name`.

Implemented: `after`, `append`, `apply`, `array`, `break`, `catch`, `cd`, `clock`, `close`, `concat`, `continue`, `coroutine`, `dict`, `eof`, `error`, `eval`, `expr`, `fconfigure`, `file`, `flush`, `for`, `foreach`, `format`, `gets`, `glob`, `global`, `if`, `incr`, `info`, `join`, `lappend`, `lassign`, `ledit`, `lindex`, `linsert`, `list`, `llength`, `lmap`, `lpop`, `lrange`, `lremove`, `lrepeat`, `lreplace`, `lreverse`, `lsearch`, `lseq`, `lset`, `lsort`, `namespace`, `open`, `package`, `proc`, `puts`, `pwd`, `read`, `regexp`, `regsub`, `rename`, `return`, `seek`, `set`, `source`, `split`, `string`, `switch`, `tell`, `unset`, `update`, `uplevel`, `upvar`, `variable`, `vwait`, `while`, `yield`, `yieldto`

## Per file

| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `aaa_exit.test` | 2 | 2 | 0 | 0 | 0 | — |
| `abstractlist.test` | 123 | 123 | 0 | 0 | 0 | — |
| `append.test` | 52 | 6 | 46 | 26 | 20 | 56.5% |
| `appendComp.test` | 48 | 8 | 40 | 25 | 15 | 62.5% |
| `apply.test` | 42 | 6 | 36 | 0 | 36 | 0.0% |
| `assemble.test` | 283 | 235 | 48 | 2 | 46 | 4.2% |
| `assocd.test` | 11 | 11 | 0 | 0 | 0 | — |
| `async.test` | 12 | 12 | 0 | 0 | 0 | — |
| `autoMkindex.test` | 11 | 8 | 3 | 1 | 2 | 33.3% |
| `basic.test` | 147 | 132 | 15 | 4 | 11 | 26.7% |
| `bigdata.test` | 113 | 113 | 0 | 0 | 0 | — |
| `binary.test` | 750 | 748 | 2 | 0 | 2 | 0.0% |
| `brodnik.test` | 422 | 422 | 0 | 0 | 0 | — |
| `chan.test` | 42 | 40 | 2 | 2 | 0 | 100.0% |
| `chanio.test` | 779 | 439 | 340 | 313 | 27 | 92.1% |
| `clock-ivm.test` | 8744 | 64 | 8680 | 1448 | 7232 | 16.7% |
| `clock-no-tzdata.test` | 0 | 0 | 0 | 0 | 0 | — |
| `clock.test` | 8744 | 76 | 8668 | 1448 | 7220 | 16.7% |
| `cmdAH.test` | 17001 | 16889 | 112 | 52 | 60 | 46.4% |
| `cmdIL.test` | 168 | 8 | 160 | 56 | 104 | 35.0% |
| `cmdInfo.test` | 12 | 12 | 0 | 0 | 0 | — |
| `cmdMZ.test` | 97 | 28 | 69 | 29 | 40 | 42.0% |
| `compExpr-old.test` | 184 | 4 | 180 | 112 | 68 | 62.2% |
| `compExpr.test` | 82 | 7 | 75 | 63 | 12 | 84.0% |
| `compile.test` | 171 | 118 | 53 | 16 | 37 | 30.2% |
| `concat.test` | 9 | 0 | 9 | 9 | 0 | 100.0% |
| `config.test` | 9 | 3 | 6 | 0 | 6 | 0.0% |
| `coroutine.test` | 77 | 30 | 47 | 3 | 44 | 6.4% |
| `dcall.test` | 6 | 6 | 0 | 0 | 0 | — |
| `dict.test` | 373 | 22 | 351 | 106 | 245 | 30.2% |
| `dstring.test` | 46 | 46 | 0 | 0 | 0 | — |
| `encoding.test` | 232 | 216 | 16 | 4 | 12 | 25.0% |
| `env.test` | 32 | 29 | 3 | 0 | 3 | 0.0% |
| `error.test` | 317 | 102 | 215 | 11 | 204 | 5.1% |
| `eval.test` | 12 | 0 | 12 | 11 | 1 | 91.7% |
| `event.test` | 65 | 42 | 23 | 7 | 16 | 30.4% |
| `exec.test` | 145 | 140 | 5 | 0 | 5 | 0.0% |
| `execute.test` | 157 | 95 | 62 | 39 | 23 | 62.9% |
| `expr-old.test` | 461 | 31 | 430 | 384 | 46 | 89.3% |
| `expr.test` | 2168 | 1094 | 1074 | 849 | 225 | 79.1% |
| `fCmd.test` | 306 | 219 | 87 | 55 | 32 | 63.2% |
| `fileName.test` | 306 | 199 | 107 | 74 | 33 | 69.2% |
| `fileSystem.test` | 140 | 82 | 58 | 39 | 19 | 67.2% |
| `fileSystemEncoding.test` | 1 | 1 | 0 | 0 | 0 | — |
| `for-old.test` | 9 | 0 | 9 | 7 | 2 | 77.8% |
| `for.test` | 88 | 26 | 62 | 16 | 46 | 25.8% |
| `foreach.test` | 43 | 1 | 42 | 32 | 10 | 76.2% |
| `format.test` | 269 | 1 | 268 | 261 | 7 | 97.4% |
| `get.test` | 23 | 17 | 6 | 6 | 0 | 100.0% |
| `history.test` | 62 | 25 | 37 | 18 | 19 | 48.6% |
| `http.test` | 528 | 501 | 27 | 9 | 18 | 33.3% |
| `http11.test` | 147 | 147 | 0 | 0 | 0 | — |
| `httpPipeline.test` | 5988 | 5988 | 0 | 0 | 0 | — |
| `httpProxy.test` | 150 | 150 | 0 | 0 | 0 | — |
| `httpcookie.test` | 60 | 54 | 6 | 0 | 6 | 0.0% |
| `icu.test` | 58 | 58 | 0 | 0 | 0 | — |
| `if-old.test` | 33 | 0 | 33 | 20 | 13 | 60.6% |
| `if.test` | 73 | 3 | 70 | 2 | 68 | 2.9% |
| `incr-old.test` | 14 | 1 | 13 | 7 | 6 | 53.8% |
| `incr.test` | 69 | 2 | 67 | 22 | 45 | 32.8% |
| `indexObj.test` | 65 | 65 | 0 | 0 | 0 | — |
| `info.test` | 287 | 133 | 154 | 49 | 105 | 31.8% |
| `init.test` | 10 | 10 | 0 | 0 | 0 | — |
| `interp.test` | 355 | 297 | 58 | 0 | 58 | 0.0% |
| `io.test` | 884 | 481 | 403 | 373 | 30 | 92.6% |
| `ioCmd.test` | 377 | 246 | 131 | 53 | 78 | 40.5% |
| `ioTrans.test` | 106 | 104 | 2 | 0 | 2 | 0.0% |
| `iogt.test` | 17 | 17 | 0 | 0 | 0 | — |
| `join.test` | 10 | 0 | 10 | 7 | 3 | 70.0% |
| `lindex.test` | 84 | 37 | 47 | 47 | 0 | 100.0% |
| `link.test` | 77 | 77 | 0 | 0 | 0 | — |
| `linsert.test` | 28 | 0 | 28 | 28 | 0 | 100.0% |
| `list.test` | 78 | 1 | 77 | 77 | 0 | 100.0% |
| `listObj.test` | 59 | 17 | 42 | 42 | 0 | 100.0% |
| `listRep.test` | 231 | 227 | 4 | 4 | 0 | 100.0% |
| `llength.test` | 6 | 0 | 6 | 6 | 0 | 100.0% |
| `lmap.test` | 66 | 1 | 65 | 38 | 27 | 58.5% |
| `load.test` | 30 | 30 | 0 | 0 | 0 | — |
| `lpop.test` | 19 | 2 | 17 | 16 | 1 | 94.1% |
| `lrange.test` | 1766 | 2 | 1764 | 1182 | 582 | 67.0% |
| `lrepeat.test` | 12 | 1 | 11 | 10 | 1 | 90.9% |
| `lreplace.test` | 3579 | 0 | 3579 | 3576 | 3 | 99.9% |
| `lsearch.test` | 165 | 0 | 165 | 65 | 100 | 39.4% |
| `lseq.test` | 136 | 22 | 114 | 81 | 33 | 71.1% |
| `lset.test` | 89 | 89 | 0 | 0 | 0 | — |
| `lsetComp.test` | 19 | 19 | 0 | 0 | 0 | — |
| `macOSXFCmd.test` | 14 | 1 | 13 | 0 | 13 | 0.0% |
| `macOSXLoad.test` | 57 | 57 | 0 | 0 | 0 | — |
| `main.test` | 67 | 62 | 5 | 5 | 0 | 100.0% |
| `mathop.test` | 385 | 222 | 163 | 13 | 150 | 8.0% |
| `misc.test` | 301 | 299 | 2 | 0 | 2 | 0.0% |
| `msgcat.test` | 135 | 123 | 12 | 4 | 8 | 33.3% |
| `mutex.test` | 12 | 12 | 0 | 0 | 0 | — |
| `namespace-old.test` | 126 | 32 | 94 | 55 | 39 | 58.5% |
| `namespace.test` | 314 | 66 | 248 | 114 | 134 | 46.0% |
| `notify.test` | 23 | 23 | 0 | 0 | 0 | — |
| `nre.test` | 28 | 23 | 5 | 0 | 5 | 0.0% |
| `obj.test` | 84 | 76 | 8 | 7 | 1 | 87.5% |
| `oo.test` | 388 | 198 | 190 | 0 | 190 | 0.0% |
| `ooNext2.test` | 62 | 9 | 53 | 0 | 53 | 0.0% |
| `ooProp.test` | 55 | 24 | 31 | 0 | 31 | 0.0% |
| `ooUtil.test` | 33 | 17 | 16 | 0 | 16 | 0.0% |
| `opt.test` | 31 | 26 | 5 | 3 | 2 | 60.0% |
| `package.test` | 0 | 0 | 0 | 0 | 0 | — |
| `parse.test` | 271 | 201 | 70 | 56 | 14 | 80.0% |
| `parseExpr.test` | 286 | 219 | 67 | 3 | 64 | 4.5% |
| `parseOld.test` | 158 | 9 | 149 | 138 | 11 | 92.6% |
| `pid.test` | 5 | 3 | 2 | 0 | 2 | 0.0% |
| `pkgMkIndex.test` | 27 | 27 | 0 | 0 | 0 | — |
| `platform.test` | 9 | 8 | 1 | 0 | 1 | 0.0% |
| `proc-old.test` | 74 | 13 | 61 | 46 | 15 | 75.4% |
| `proc.test` | 38 | 12 | 26 | 7 | 19 | 26.9% |
| `process.test` | 18 | 18 | 0 | 0 | 0 | — |
| `pwd.test` | 3 | 0 | 3 | 2 | 1 | 66.7% |
| `reg.test` | 1141 | 1107 | 34 | 21 | 13 | 61.8% |
| `regexp.test` | 257 | 7 | 250 | 218 | 32 | 87.2% |
| `regexpComp.test` | 179 | 150 | 29 | 26 | 3 | 89.7% |
| `registry.test` | 125 | 125 | 0 | 0 | 0 | — |
| `rename.test` | 19 | 9 | 10 | 5 | 5 | 50.0% |
| `resolver.test` | 10 | 10 | 0 | 0 | 0 | — |
| `result.test` | 26 | 22 | 4 | 0 | 4 | 0.0% |
| `safe-stock.test` | 11 | 6 | 5 | 0 | 5 | 0.0% |
| `safe-stock86.test` | 0 | 0 | 0 | 0 | 0 | — |
| `safe-zipfs.test` | 22 | 6 | 16 | 6 | 10 | 37.5% |
| `safe.test` | 155 | 75 | 80 | 40 | 40 | 50.0% |
| `scan.test` | 185 | 174 | 11 | 0 | 11 | 0.0% |
| `security.test` | 1 | 1 | 0 | 0 | 0 | — |
| `set-old.test` | 153 | 5 | 148 | 92 | 56 | 62.2% |
| `set.test` | 64 | 3 | 61 | 26 | 35 | 42.6% |
| `socket.test` | 189 | 171 | 18 | 2 | 16 | 11.1% |
| `source.test` | 23 | 22 | 1 | 0 | 1 | 0.0% |
| `split.test` | 18 | 0 | 18 | 16 | 2 | 88.9% |
| `stack.test` | 3 | 3 | 0 | 0 | 0 | — |
| `string.test` | 705 | 596 | 109 | 102 | 7 | 93.6% |
| `stringObj.test` | 81 | 81 | 0 | 0 | 0 | — |
| `subst.test` | 63 | 51 | 12 | 0 | 12 | 0.0% |
| `switch.test` | 113 | 41 | 72 | 22 | 50 | 30.6% |
| `tailcall.test` | 37 | 24 | 13 | 0 | 13 | 0.0% |
| `tcltest.test` | 127 | 57 | 70 | 40 | 30 | 57.1% |
| `thread.test` | 52 | 52 | 0 | 0 | 0 | — |
| `timer.test` | 54 | 3 | 51 | 36 | 15 | 70.6% |
| `tm.test` | 21 | 19 | 2 | 0 | 2 | 0.0% |
| `trace.test` | 290 | 221 | 69 | 1 | 68 | 1.4% |
| `unixFCmd.test` | 49 | 25 | 24 | 0 | 24 | 0.0% |
| `unixFile.test` | 7 | 7 | 0 | 0 | 0 | — |
| `unixForkEvent.test` | 1 | 1 | 0 | 0 | 0 | — |
| `unixInit.test` | 8 | 7 | 1 | 0 | 1 | 0.0% |
| `unixNotfy.test` | 4 | 4 | 0 | 0 | 0 | — |
| `unknown.test` | 7 | 5 | 2 | 1 | 1 | 50.0% |
| `unload.test` | 27 | 27 | 0 | 0 | 0 | — |
| `uplevel.test` | 57 | 12 | 45 | 28 | 17 | 62.2% |
| `upvar.test` | 70 | 9 | 61 | 0 | 61 | 0.0% |
| `utf.test` | 399 | 251 | 148 | 131 | 17 | 88.5% |
| `utfext.test` | 842 | 842 | 0 | 0 | 0 | — |
| `util.test` | 462 | 336 | 126 | 126 | 0 | 100.0% |
| `var.test` | 221 | 58 | 163 | 24 | 139 | 14.7% |
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
| `zipfs.test` | 528 | 325 | 203 | 198 | 5 | 97.5% |
| `zlib.test` | 74 | 52 | 22 | 0 | 22 | 0.0% |

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

A stage that goes 15s without producing an outcome is killed and the case it was on is recorded as an abort, so that one pathological body cannot stall the run. Aborts on the tclrs side count as failures rather than skips, and this run had 22 of them; aborts on the reference side are the `tclsh produced no reference outcome` skips above. That timeout is the only bound in the pipeline, and nothing is dropped without landing in one of those two counts.

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
