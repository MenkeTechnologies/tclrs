# tclrs conformance against the official Tcl test suite

Reference interpreter: **tclsh 9.0.4**. Suite: `tcl9.0.4/tests` — the `tests/` directory of the matching Tcl source release, fetched and checksum-verified by `conformance/fetch-suite.sh`.

**30760 of 49094 attempted cases pass — 62.7%.** Over every case the suite contains, including the ones that cannot be run here, that is 30760 of 69424 — 44.3%.

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
| Skipped — cannot be run | 20330 | 29.3% |
| Attempted | 49094 | 70.7% |
| ⤷ passed | 30760 | 62.7% of attempted |
| ⤷ failed | 18334 | 37.3% of attempted |

Of the 18334 failures, 15542 are a feature tclrs documents as not built yet rather than a wrong answer. Counting those as skips instead would give 30760 of 33552 — 91.7% — and that looser number is stated here only so the choice of rule is visible. The headline above uses the strict rule.

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| tcltest constraint not met | 13663 |
| needs a command plain tclsh has not got | 4751 |
| tclrs has no such command | 1915 |
| tclsh produced no reference outcome | 1 |

### Commands tclrs does not have, by how many cases they block

A case is attributed to the first command tclrs refused, so a body using several missing commands is counted once, against the first of them.

| Command | Cases |
| --- | ---: |
| `interp` | 358 |
| `oo::class` | 289 |
| `trace` | 274 |
| `socket` | 123 |
| `try` | 91 |
| `chan` | 72 |
| `oo::object` | 57 |
| `zipfs` | 55 |
| `tcl::prefix` | 36 |
| `fpclassify` | 30 |
| `safe::interpCreate` | 27 |
| `const` | 26 |
| `exec` | 24 |
| `ns` | 24 |
| `history` | 23 |
| `tcl::unsupported::disassemble` | 22 |
| `tcl::unsupported::getbytecode` | 22 |
| `::apply` | 19 |
| `::tcl::tm::path` | 17 |
| `load` | 16 |
| `tailcall` | 16 |
| `tcl::unsupported::representation` | 16 |
| `timerate` | 15 |
| `unload` | 14 |
| `zlib` | 14 |
| `::tcl::Bgerror` | 13 |
| `tcl_startOfNextWord` | 12 |
| `tcl_endOfWord` | 11 |
| `tcl_startOfPreviousWord` | 11 |
| `tcl_wordBreakAfter` | 11 |
| `tcl_wordBreakBefore` | 10 |
| `::tcl::mathfunc::abs` | 9 |
| `::tcl::mathop::ge` | 9 |
| `::tcl::mathop::gt` | 9 |
| `::tcl::mathop::le` | 9 |
| `::tcl::mathop::lt` | 9 |
| `tcl::tm::path` | 9 |
| `auto_qualify` | 8 |
| `tcl::process` | 7 |
| `time` | 7 |
| *47 further commands* | 91 |

## Why cases failed

| Cause | Cases | Share of failures | For example |
| --- | ---: | ---: | --- |
| tclrs raised an error, tclsh did not | 10406 | 56.8% | `append.test` append-7.1, `append.test` append-10.1, `apply.test` apply-2.2 |
| both raised an error, messages differ | 6600 | 36.0% | `append.test` append-3.1, `append.test` append-6.1, `append.test` append-10.2 |
| results differ | 1143 | 6.2% | `append.test` append-3.4, `append.test` append-3.5, `append.test` append-3.6 |
| tclsh raised an error, tclrs did not | 154 | 0.8% | `appendComp.test` appendComp-10.4, `binary.test` binary-73.29, `binary.test` binary-75.25 |
| tclrs was killed or crashed | 31 | 0.2% | `clock-ivm.test` clock-6.0.vm:0, `clock-ivm.test` clock-6.9.vm:0, `clock-ivm.test` clock-6.10.vm:0 |

Every failing case is written out in full — its program, the tclsh outcome and the tclrs outcome — to `conformance/work/failures.txt` by the same run that produced this table.

### The most frequent failing messages

Error text with the quoted part elided and tclrs's trailing `(line N)` removed, so that one cause groups into one row.

| Message | Cases |
| --- | ---: |
| clock: the locale "…" is not supported yet; only the root locale is built in | 13374 |
| command name must be a literal in this phase | 1163 |
| identical text apart from tclrs's trailing (line N) | 359 |
| can't read "…": no such variable | 293 |
| encoding convertfrom: the tcl8 profile decodes this input to the lone surrogate U+D800, which a string in this frontend cannot hold | 181 |
| clock scan: the free-form parser is not supported yet; use -format | 160 |
| clock scan: -base is not supported yet | 159 |
| expression must be a literal in this phase | 132 |
| encoding convertfrom: the tcl8 profile decodes this input to the lone surrogate U+DC00, which a string in this frontend cannot hold | 116 |
| clock scan: the format token "…" is not supported yet | 59 |
| unable to convert input string: ambiguous day | 48 |
| key "…" not known in dictionary | 39 |
| time zone "…" not found: no zone file names it, and a POSIX time zone rule is not supported yet | 36 |
| return option "…" is not supported | 35 |
| script body must be a literal in this phase | 34 |
| encoding convertfrom: the tcl8 profile decodes this input to the lone surrogate U+DBFF, which a string in this frontend cannot hold | 32 |
| invalid bareword "…" in expression "…"; should be "…" or "…" or "…" or ... | 31 |
| input string does not match supplied format | 30 |
| "…" is not supported yet: this frontend resolves namespaces while compiling, so the name has to be written out | 28 |
| a coroutine of the built-in command "…" is not supported; its body must be a procedure this script defines | 25 |
| integer value too large to represent | 25 |
| "…" into an array element is not supported yet | 24 |
| array default is not supported yet | 24 |
| array startsearch is not supported yet | 24 |
| file attributes is not supported yet: it needs an interface this frontend has not built | 24 |
| the namespace "…" of a lambda is not supported yet: this frontend has only "…" | 22 |
| "…" with a level number is not supported: no record of the command that entered a level is kept | 20 |
| file link is not supported yet: it needs an interface this frontend has not built | 19 |
| info frame is not supported yet: it reports on the stack of *commands*, and only the stack of call frames is kept | 19 |
| wrong # args: should be "…" | 19 |

## Command coverage

Independently of the suite: of the 109 commands the reference interpreter defines in the global namespace, tclrs answers to 80 — 73.4%. A name counts as answered when tclrs does not refuse it with `invalid command name`.

Implemented: `after`, `append`, `apply`, `array`, `binary`, `break`, `catch`, `cd`, `clock`, `close`, `concat`, `continue`, `coroutine`, `dict`, `encoding`, `eof`, `error`, `eval`, `expr`, `fconfigure`, `file`, `flush`, `for`, `foreach`, `format`, `gets`, `glob`, `global`, `if`, `incr`, `info`, `join`, `lappend`, `lassign`, `ledit`, `lindex`, `linsert`, `list`, `llength`, `lmap`, `lpop`, `lrange`, `lremove`, `lrepeat`, `lreplace`, `lreverse`, `lsearch`, `lseq`, `lset`, `lsort`, `namespace`, `open`, `package`, `proc`, `puts`, `pwd`, `read`, `regexp`, `regsub`, `rename`, `return`, `scan`, `seek`, `set`, `source`, `split`, `string`, `subst`, `switch`, `tell`, `throw`, `unset`, `update`, `uplevel`, `upvar`, `variable`, `vwait`, `while`, `yield`, `yieldto`

## Per file

| File | Extracted | Skipped | Attempted | Passed | Failed | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `aaa_exit.test` | 2 | 2 | 0 | 0 | 0 | — |
| `abstractlist.test` | 123 | 123 | 0 | 0 | 0 | — |
| `append.test` | 52 | 9 | 43 | 33 | 10 | 76.7% |
| `appendComp.test` | 48 | 12 | 36 | 32 | 4 | 88.9% |
| `apply.test` | 42 | 6 | 36 | 11 | 25 | 30.6% |
| `assemble.test` | 283 | 235 | 48 | 2 | 46 | 4.2% |
| `assocd.test` | 11 | 11 | 0 | 0 | 0 | — |
| `async.test` | 12 | 12 | 0 | 0 | 0 | — |
| `autoMkindex.test` | 11 | 10 | 1 | 1 | 0 | 100.0% |
| `basic.test` | 147 | 132 | 15 | 4 | 11 | 26.7% |
| `bigdata.test` | 113 | 113 | 0 | 0 | 0 | — |
| `binary.test` | 750 | 90 | 660 | 610 | 50 | 92.4% |
| `brodnik.test` | 422 | 422 | 0 | 0 | 0 | — |
| `chan.test` | 42 | 40 | 2 | 2 | 0 | 100.0% |
| `chanio.test` | 779 | 439 | 340 | 334 | 6 | 98.2% |
| `clock-ivm.test` | 8744 | 64 | 8680 | 1451 | 7229 | 16.7% |
| `clock-no-tzdata.test` | 0 | 0 | 0 | 0 | 0 | — |
| `clock.test` | 8744 | 76 | 8668 | 1451 | 7217 | 16.7% |
| `cmdAH.test` | 17001 | 206 | 16795 | 16245 | 550 | 96.7% |
| `cmdIL.test` | 168 | 8 | 160 | 131 | 29 | 81.9% |
| `cmdInfo.test` | 12 | 12 | 0 | 0 | 0 | — |
| `cmdMZ.test` | 97 | 29 | 68 | 15 | 53 | 22.1% |
| `compExpr-old.test` | 184 | 4 | 180 | 116 | 64 | 64.4% |
| `compExpr.test` | 82 | 7 | 75 | 63 | 12 | 84.0% |
| `compile.test` | 171 | 120 | 51 | 37 | 14 | 72.5% |
| `concat.test` | 9 | 0 | 9 | 9 | 0 | 100.0% |
| `config.test` | 9 | 3 | 6 | 0 | 6 | 0.0% |
| `coroutine.test` | 77 | 33 | 44 | 5 | 39 | 11.4% |
| `dcall.test` | 6 | 6 | 0 | 0 | 0 | — |
| `dict.test` | 373 | 24 | 349 | 254 | 95 | 72.8% |
| `dstring.test` | 46 | 46 | 0 | 0 | 0 | — |
| `encoding.test` | 232 | 46 | 186 | 130 | 56 | 69.9% |
| `env.test` | 32 | 29 | 3 | 2 | 1 | 66.7% |
| `error.test` | 317 | 95 | 222 | 23 | 199 | 10.4% |
| `eval.test` | 12 | 0 | 12 | 11 | 1 | 91.7% |
| `event.test` | 65 | 50 | 15 | 7 | 8 | 46.7% |
| `exec.test` | 145 | 140 | 5 | 0 | 5 | 0.0% |
| `execute.test` | 157 | 96 | 61 | 40 | 21 | 65.6% |
| `expr-old.test` | 461 | 31 | 430 | 385 | 45 | 89.5% |
| `expr.test` | 2168 | 1091 | 1077 | 877 | 200 | 81.4% |
| `fCmd.test` | 306 | 219 | 87 | 55 | 32 | 63.2% |
| `fileName.test` | 306 | 199 | 107 | 78 | 29 | 72.9% |
| `fileSystem.test` | 140 | 82 | 58 | 40 | 18 | 69.0% |
| `fileSystemEncoding.test` | 1 | 1 | 0 | 0 | 0 | — |
| `for-old.test` | 9 | 0 | 9 | 7 | 2 | 77.8% |
| `for.test` | 88 | 26 | 62 | 24 | 38 | 38.7% |
| `foreach.test` | 43 | 1 | 42 | 36 | 6 | 85.7% |
| `format.test` | 269 | 1 | 268 | 264 | 4 | 98.5% |
| `get.test` | 23 | 17 | 6 | 6 | 0 | 100.0% |
| `history.test` | 62 | 25 | 37 | 18 | 19 | 48.6% |
| `http.test` | 528 | 501 | 27 | 9 | 18 | 33.3% |
| `http11.test` | 147 | 147 | 0 | 0 | 0 | — |
| `httpPipeline.test` | 5988 | 5988 | 0 | 0 | 0 | — |
| `httpProxy.test` | 150 | 150 | 0 | 0 | 0 | — |
| `httpcookie.test` | 60 | 54 | 6 | 0 | 6 | 0.0% |
| `icu.test` | 58 | 58 | 0 | 0 | 0 | — |
| `if-old.test` | 33 | 0 | 33 | 30 | 3 | 90.9% |
| `if.test` | 73 | 4 | 69 | 18 | 51 | 26.1% |
| `incr-old.test` | 14 | 1 | 13 | 11 | 2 | 84.6% |
| `incr.test` | 69 | 2 | 67 | 26 | 41 | 38.8% |
| `indexObj.test` | 65 | 65 | 0 | 0 | 0 | — |
| `info.test` | 287 | 133 | 154 | 68 | 86 | 44.2% |
| `init.test` | 10 | 10 | 0 | 0 | 0 | — |
| `interp.test` | 355 | 304 | 51 | 0 | 51 | 0.0% |
| `io.test` | 884 | 482 | 402 | 380 | 22 | 94.5% |
| `ioCmd.test` | 377 | 244 | 133 | 56 | 77 | 42.1% |
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
| `lmap.test` | 66 | 1 | 65 | 51 | 14 | 78.5% |
| `load.test` | 30 | 30 | 0 | 0 | 0 | — |
| `lpop.test` | 19 | 2 | 17 | 16 | 1 | 94.1% |
| `lrange.test` | 1766 | 2 | 1764 | 1182 | 582 | 67.0% |
| `lrepeat.test` | 12 | 1 | 11 | 10 | 1 | 90.9% |
| `lreplace.test` | 3579 | 0 | 3579 | 3578 | 1 | 100.0% |
| `lsearch.test` | 165 | 0 | 165 | 156 | 9 | 94.5% |
| `lseq.test` | 136 | 25 | 111 | 81 | 30 | 73.0% |
| `lset.test` | 89 | 89 | 0 | 0 | 0 | — |
| `lsetComp.test` | 19 | 19 | 0 | 0 | 0 | — |
| `macOSXFCmd.test` | 14 | 1 | 13 | 0 | 13 | 0.0% |
| `macOSXLoad.test` | 57 | 57 | 0 | 0 | 0 | — |
| `main.test` | 67 | 62 | 5 | 5 | 0 | 100.0% |
| `mathop.test` | 385 | 222 | 163 | 13 | 150 | 8.0% |
| `misc.test` | 301 | 299 | 2 | 0 | 2 | 0.0% |
| `msgcat.test` | 135 | 123 | 12 | 12 | 0 | 100.0% |
| `mutex.test` | 12 | 12 | 0 | 0 | 0 | — |
| `namespace-old.test` | 126 | 32 | 94 | 63 | 31 | 67.0% |
| `namespace.test` | 314 | 72 | 242 | 115 | 127 | 47.5% |
| `notify.test` | 23 | 23 | 0 | 0 | 0 | — |
| `nre.test` | 28 | 23 | 5 | 0 | 5 | 0.0% |
| `obj.test` | 84 | 76 | 8 | 7 | 1 | 87.5% |
| `oo.test` | 388 | 297 | 91 | 0 | 91 | 0.0% |
| `ooNext2.test` | 62 | 33 | 29 | 0 | 29 | 0.0% |
| `ooProp.test` | 55 | 38 | 17 | 0 | 17 | 0.0% |
| `ooUtil.test` | 33 | 19 | 14 | 0 | 14 | 0.0% |
| `opt.test` | 31 | 26 | 5 | 3 | 2 | 60.0% |
| `package.test` | 0 | 0 | 0 | 0 | 0 | — |
| `parse.test` | 271 | 181 | 90 | 75 | 15 | 83.3% |
| `parseExpr.test` | 286 | 219 | 67 | 3 | 64 | 4.5% |
| `parseOld.test` | 158 | 9 | 149 | 142 | 7 | 95.3% |
| `pid.test` | 5 | 3 | 2 | 0 | 2 | 0.0% |
| `pkgMkIndex.test` | 27 | 27 | 0 | 0 | 0 | — |
| `platform.test` | 9 | 8 | 1 | 0 | 1 | 0.0% |
| `proc-old.test` | 74 | 13 | 61 | 48 | 13 | 78.7% |
| `proc.test` | 38 | 12 | 26 | 10 | 16 | 38.5% |
| `process.test` | 18 | 18 | 0 | 0 | 0 | — |
| `pwd.test` | 3 | 0 | 3 | 2 | 1 | 66.7% |
| `reg.test` | 1141 | 1107 | 34 | 22 | 12 | 64.7% |
| `regexp.test` | 257 | 7 | 250 | 232 | 18 | 92.8% |
| `regexpComp.test` | 179 | 150 | 29 | 26 | 3 | 89.7% |
| `registry.test` | 125 | 125 | 0 | 0 | 0 | — |
| `rename.test` | 19 | 9 | 10 | 5 | 5 | 50.0% |
| `resolver.test` | 10 | 10 | 0 | 0 | 0 | — |
| `result.test` | 26 | 22 | 4 | 0 | 4 | 0.0% |
| `safe-stock.test` | 11 | 7 | 4 | 4 | 0 | 100.0% |
| `safe-stock86.test` | 0 | 0 | 0 | 0 | 0 | — |
| `safe-zipfs.test` | 22 | 6 | 16 | 9 | 7 | 56.2% |
| `safe.test` | 155 | 76 | 79 | 44 | 35 | 55.7% |
| `scan.test` | 185 | 3 | 182 | 169 | 13 | 92.9% |
| `security.test` | 1 | 1 | 0 | 0 | 0 | — |
| `set-old.test` | 153 | 6 | 147 | 94 | 53 | 63.9% |
| `set.test` | 64 | 3 | 61 | 29 | 32 | 47.5% |
| `socket.test` | 189 | 181 | 8 | 6 | 2 | 75.0% |
| `source.test` | 23 | 22 | 1 | 0 | 1 | 0.0% |
| `split.test` | 18 | 0 | 18 | 16 | 2 | 88.9% |
| `stack.test` | 3 | 3 | 0 | 0 | 0 | — |
| `string.test` | 705 | 596 | 109 | 102 | 7 | 93.6% |
| `stringObj.test` | 81 | 81 | 0 | 0 | 0 | — |
| `subst.test` | 63 | 2 | 61 | 54 | 7 | 88.5% |
| `switch.test` | 113 | 11 | 102 | 72 | 30 | 70.6% |
| `tailcall.test` | 37 | 29 | 8 | 0 | 8 | 0.0% |
| `tcltest.test` | 127 | 57 | 70 | 50 | 20 | 71.4% |
| `thread.test` | 52 | 52 | 0 | 0 | 0 | — |
| `timer.test` | 54 | 4 | 50 | 44 | 6 | 88.0% |
| `tm.test` | 21 | 19 | 2 | 0 | 2 | 0.0% |
| `trace.test` | 290 | 225 | 65 | 1 | 64 | 1.5% |
| `unixFCmd.test` | 49 | 25 | 24 | 5 | 19 | 20.8% |
| `unixFile.test` | 7 | 7 | 0 | 0 | 0 | — |
| `unixForkEvent.test` | 1 | 1 | 0 | 0 | 0 | — |
| `unixInit.test` | 8 | 7 | 1 | 0 | 1 | 0.0% |
| `unixNotfy.test` | 4 | 4 | 0 | 0 | 0 | — |
| `unknown.test` | 7 | 5 | 2 | 1 | 1 | 50.0% |
| `unload.test` | 27 | 27 | 0 | 0 | 0 | — |
| `uplevel.test` | 57 | 11 | 46 | 34 | 12 | 73.9% |
| `upvar.test` | 70 | 17 | 53 | 22 | 31 | 41.5% |
| `utf.test` | 399 | 251 | 148 | 131 | 17 | 88.5% |
| `utfext.test` | 842 | 842 | 0 | 0 | 0 | — |
| `util.test` | 462 | 327 | 135 | 135 | 0 | 100.0% |
| `var.test` | 221 | 66 | 155 | 37 | 118 | 23.9% |
| `while-old.test` | 15 | 0 | 15 | 13 | 2 | 86.7% |
| `while.test` | 46 | 0 | 46 | 15 | 31 | 32.6% |
| `winConsole.test` | 46 | 46 | 0 | 0 | 0 | — |
| `winDde.test` | 50 | 50 | 0 | 0 | 0 | — |
| `winFCmd.test` | 173 | 173 | 0 | 0 | 0 | — |
| `winFile.test` | 11 | 11 | 0 | 0 | 0 | — |
| `winNotify.test` | 14 | 14 | 0 | 0 | 0 | — |
| `winPipe.test` | 56 | 56 | 0 | 0 | 0 | — |
| `winTime.test` | 3 | 3 | 0 | 0 | 0 | — |
| `word.test` | 55 | 55 | 0 | 0 | 0 | — |
| `zipfs.test` | 528 | 326 | 202 | 198 | 4 | 98.0% |
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

A stage that goes 15s without producing an outcome is killed and the case it was on is recorded as an abort, so that one pathological body cannot stall the run. Aborts on the tclrs side count as failures rather than skips, and this run had 31 of them; aborts on the reference side are the `tclsh produced no reference outcome` skips above. That timeout is the only bound in the pipeline, and nothing is dropped without landing in one of those two counts.

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
