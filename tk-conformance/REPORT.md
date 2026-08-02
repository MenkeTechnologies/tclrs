# tclrs conformance against the official Tk test suite

Reference: **tclsh 9.0.4 with the real Tk 9.0.4 loaded**. Suite: `tk9.0.4/tests` — the `tests/` directory of the matching Tk source release, fetched and checksum-verified by `tk-conformance/fetch-suite.sh`.

The candidate is not a reimplementation of Tk. It is the same `libtcl9tk9.0.dylib` the reference uses, loaded against tclrs's own Tcl stub table: 151 of the 691 `TclStubs` slots have bodies, and Tk reaches this frontend through them.

**1580 of 5040 attempted cases pass — 31.3%.** Over every case the suite contains, including the ones that cannot be run here, that is 1580 of 10046 — 15.7%.

## How the number is produced

The suite drives every test through the `tcltest` package. tclrs cannot load it — tcltest is Tcl code built on namespaces, `proc`, `catch`, `regexp` and channel IO, none of which this frontend has — so the cases are lifted out of the suite files instead of being run in place.

The lifting is done by `tk-conformance/extract.tcl`, running under tclsh with the real tcltest and the real Tk loaded and only `::tcltest::test` replaced by a recorder. The recorder is a port of tcltest's own argument parsing, so both the `-option value` form and the historical `test name desc ?constraints? body result` form are read exactly as tcltest reads them, and constraint state comes from tcltest's own evaluation rather than a re-implementation of it. Tk has to be loaded for the extraction and not only for the run: `tests/constraints.tcl` calls `tk windowingsystem`, `winfo`, `font` and `image` at a file's top level, so a tclsh without Tk extracts nothing at all. Every suite file is extracted; there is no option to select a subset, and the runner has no way to run one.

Each extracted case becomes a standalone program — its `-setup` followed by its `-body` — and is run twice. The reference run is a fresh child interpreter with Tk loaded into it, so the case has a main window and the widget commands. The candidate run is a separate process that opens `libtcl9tk9.0.dylib`, builds tclrs's hosting stub table, calls `Tk_Init`, and evaluates the case through the host's own evaluator. The outcome of a run is the triple (return code, result string, everything written to stdout), and a case passes only when the two triples are identical byte for byte. The suite's own `-result` and `-match` values are not consulted: the reference is the specification, and comparing against what it actually does is stricter than comparing against what the suite says it should.

Verdicts are assigned in a fixed order, and agreement is checked before any excuse for tclrs is considered, so no rule below can turn a pass into a skip. A case is set aside only when it genuinely cannot be run:

| Skip reason | What it means |
| --- | --- |
| tcltest constraint not met | tcltest's own constraint check says this build, platform or configuration cannot run the case. |
| the reference produced no outcome | the reference run hung and was killed, or died on the case, so there is nothing to compare against. |
| needs a command the reference has not got | the reference run failed with `invalid command name`: the case needs the internal commands of the `tk::test` package, or a helper an earlier test body would have defined. Set aside even when tclrs happens to report the same error, which costs passes rather than inventing them. |
| needs a package that is not installed | the reference run failed with `can't find package`. |
| tclrs has no such command | tclrs refused with `invalid command name` for a command it does not implement. |

**A stub-table trap is a failure, not a skip.** This is the one rule that differs from `conformance/`, and it is the stricter reading. Tk reaches this host through 691 function pointers; 540 of them have no body, and calling one ends the process (`src/tk/trace.rs:101-123`, which argues that answering a plausible zero from a slot whose contract is a live `Tcl_Obj *` turns a precise diagnosis into a crash several frames later). That is not tclrs declining and saying so, the way `invalid command name` is — it is the process dying, and a process that died measured nothing. Excusing it would move almost the whole suite into the skip column and leave a pass rate computed over a handful of cases. The slots that stopped a run are counted on their own below instead.

Everything else is attempted, and anything attempted either matches or fails.

Two things about the extraction are worth stating plainly, and both are inherited from `conformance/extract.tcl` unchanged. First, suite files set variables at their top level and then write bodies that read them, so each case carries the global variables its file had created by the time the test was declared, replayed ahead of the body as `set` and `array set` commands; only variables whose name appears in the case's own text are carried, and both runs get exactly the same program. Second, procs are not replayed and bodies are not executed during extraction, so a case that depends on a helper proc or on state an earlier body would have produced fails under the reference too, and is set aside as needing an unavailable command rather than counted against tclrs. `-cleanup` scripts are not run: they execute after the value under test is produced and cannot change it.

## Totals

| | Cases | Share |
| --- | ---: | ---: |
| Extracted from the suite | 10046 | 100% |
| Skipped — cannot be run | 5006 | 49.8% |
| Attempted | 5040 | 50.2% |
| ⤷ passed | 1580 | 31.3% of attempted |
| ⤷ failed | 3460 | 68.7% of attempted |

## Why cases were skipped

| Reason | Cases |
| --- | ---: |
| needs a command the reference has not got | 3189 |
| tcltest constraint not met | 1653 |
| tclrs has no such command | 146 |
| the reference produced no outcome | 18 |

### Constraints that set cases aside

| Name | Cases |
| --- | ---: |
| `win` | 283 |
| `testobjconfig` | 214 |
| `testImageType` | 135 |
| `fonts` | 121 |
| `nonUnixUserInteraction` | 97 |
| `x11` | 93 |
| `winSend` | 51 |
| `secureserver testsend` | 50 |
| `unix testwrapper` | 44 |
| `nt testwinevent` | 38 |
| `nonPortable` | 37 |
| `testtext` | 31 |
| `unix nonPortable` | 27 |
| `unix notAqua` | 24 |
| `testutils` | 21 |
| `notAqua` | 19 |
| `win getwindowinfo` | 19 |
| `win userInteraction` | 18 |
| `unix testmenubar` | 16 |
| `colorsFree` | 15 |
| `nt` | 12 |
| `defaultPseudocolor8 nonPortable` | 11 |
| `aquaKnownBug` | 10 |
| `testmetrics win` | 10 |
| `unix testembed` | 10 |
| `scriptImpl` | 9 |
| `knownBug` | 8 |
| `secureserver notAqua` | 8 |
| `x11 failsOnCILinux` | 8 |
| `altDisplay` | 7 |
| `testmakeexist` | 7 |
| `testwinevent` | 7 |
| `unix nonPortable testwrapper` | 7 |
| `unix testembed notAqua` | 7 |
| `win nonPortable` | 7 |
| `win testclipboard` | 7 |
| `testImageType nonPortable` | 6 |
| `testwrapper` | 6 |
| `testbitmap` | 5 |
| `testcursor` | 5 |
| *70 further* | 143 |

### Commands tclrs does not have, by how many cases they block

| Name | Cases |
| --- | ---: |
| `scan` | 18 |
| `namespace` | 17 |
| `safe::interpCreate` | 13 |
| `::tk::startOfNextWord` | 12 |
| `::tk::endOfWord` | 11 |
| `::tk::startOfPreviousWord` | 11 |
| `::tk::wordBreakAfter` | 11 |
| `::tk::wordBreakBefore` | 10 |
| `trace` | 10 |
| `interp` | 7 |
| `after` | 6 |
| `::tk::pkgconfig` | 3 |
| `binary` | 3 |
| `pause` | 3 |
| `tk_dialog` | 3 |
| `rename` | 2 |
| `apply` | 1 |
| `file` | 1 |
| `flush` | 1 |
| `tk::MotifFDialog_Create` | 1 |
| `tk_focusNext` | 1 |
| `tk_focusPrev` | 1 |

### Commands the reference has not got either

| Name | Cases |
| --- | ---: |
| `deleteWindows` | 803 |
| `.t` | 767 |
| `.m1` | 230 |
| `.s` | 221 |
| `.l` | 220 |
| `.c` | 212 |
| `.f` | 73 |
| `.mb` | 69 |
| `.p` | 58 |
| `selectionSetup` | 46 |
| `imageCleanup` | 42 |
| `csetup` | 36 |
| `::_test_tmp::clearPrimarySelection` | 30 |
| `::_test_tmp::setPrimarySelection` | 30 |
| `setup1` | 30 |
| `setup` | 29 |
| `put` | 24 |
| `makeFile` | 21 |
| `text_test_word` | 20 |
| `msetup` | 16 |
| `raiseDelay` | 16 |
| `.t.l` | 14 |
| `loadTkCommand` | 13 |
| `raise_setup` | 13 |
| `setupBig` | 13 |
| `clearnondefaultfonts` | 12 |
| `setup_win_mousepointer` | 12 |
| `.t.f` | 11 |
| `i1` | 10 |
| `tcltest::configure` | 10 |
| `.top.t` | 9 |
| `focusClear` | 9 |
| `bo` | 7 |
| `getword` | 7 |
| `cleanup` | 6 |
| `mkPartial` | 6 |
| `checkImgTrans` | 5 |
| `controlPointerWarpTiming` | 5 |
| `.t.s` | 4 |
| `makeToplevels` | 4 |
| *17 further* | 26 |

## Why cases failed

| Cause | Cases | For example |
| --- | ---: | --- |
| tclrs was killed or crashed | 2387 | the stage process died on this case |
| tclrs raised an error, the reference did not | 594 | "…" outside of a procedure is not supported |
| both raised an error, messages differ | 457 | "…" outside of a procedure is not supported |
| results differ | 19 |  |
| the reference raised an error, tclrs did not | 3 | can't set "…": variable is array |

## Which stub slot stopped the run

2884 cases took their worker process down by calling a `TclStubs` slot that has no body. Each is attributed to the slot named on the `tktrap` line that followed its `tkcase` marker, so this is a ranked list of what to implement next rather than an estimate.

| Slot | Cases |
| --- | ---: |
| `tcl_SplitList` | 1845 |
| `tcl_WrongNumArgs` | 226 |
| `tcl_DeleteCommandFromToken` | 215 |
| `tcl_GetDouble` | 130 |
| `tcl_GetBytesFromObj` | 106 |
| `tcl_GetIntForIndex` | 86 |
| `tcl_GetCharLength` | 63 |
| `tcl_OpenFileChannel` | 35 |
| `tcl_GetInt` | 31 |
| `tcl_UniCharIsPrint` | 29 |
| `tcl_AttemptAlloc` | 26 |
| `tcl_UniCharIsUpper` | 24 |
| `tcl_GetEncoding` | 15 |
| `tcl_TranslateFileName` | 15 |
| `tcl_AppendObjToErrorInfo` | 14 |
| `tcl_Merge` | 7 |
| `tcl_SaveInterpState` | 7 |
| `tcl_AppendPrintfToObj` | 3 |
| `tcl_AppendResult` | 3 |
| `tcl_ScanElement` | 3 |
| `tcl_InterpDeleted` | 1 |

## Tk's own widget demonstration

`demos/widget` is the sample application `wish` ships with: 713 lines, which `info complete` divides into 65 statements (`tk-conformance/boundaries.tcl`; runs of blank lines and comments are not counted as statements). It is run here one statement at a time against one host, in order, the way `wish` runs it — and every statement is attempted, including the ones after the first refusal, so the answer is more than one bit. A statement that ends the process is stepped over when the run is restarted, so one fatal statement does not take its successors with it.

**It gets 1 of 65 statements in, and stops at line 13 of the file.** That statement was refused:

```text
invalid command name "package"
```

Attempted individually, 25 of the 65 statements complete and 40 do not. The refusals, ranked:

| Refusal | Statements |
| --- | ---: |
| `invalid command name "…"` | 18 |
| `command "…" is an ensemble, which this host does not dispatch yet` | 7 |
| `{*} argument expansion is not supported yet` | 4 |
| `command name must be a literal in this phase` | 3 |
| `called the stub slot tcl_GetDouble, which has no body` | 2 |
| `called the stub slot tcl_SplitList, which has no body` | 2 |
| `can't read "…": no such variable` | 2 |
| `bad window path name "…"` | 1 |
| `unknown or unsupported subcommand "…": only "…" is supported` | 1 |

## By suite file

| File | Extracted | Skipped | Attempted | Passed | Rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| `bell.test` | 8 | 1 | 7 | 5 | 71.4% |
| `bgerror.test` | 3 | 0 | 3 | 0 | 0.0% |
| `bind.test` | 558 | 13 | 545 | 388 | 71.2% |
| `bitmap.test` | 7 | 5 | 2 | 0 | 0.0% |
| `border.test` | 14 | 7 | 7 | 1 | 14.3% |
| `busy.test` | 59 | 5 | 54 | 0 | 0.0% |
| `button.test` | 403 | 18 | 385 | 10 | 2.6% |
| `canvImg.test` | 84 | 84 | 0 | 0 | — |
| `canvMoveto.test` | 8 | 8 | 0 | 0 | — |
| `canvPs.test` | 11 | 8 | 3 | 0 | 0.0% |
| `canvRect.test` | 53 | 53 | 0 | 0 | — |
| `canvText.test` | 98 | 90 | 8 | 2 | 25.0% |
| `canvWind.test` | 5 | 0 | 5 | 0 | 0.0% |
| `canvas.test` | 161 | 63 | 98 | 0 | 0.0% |
| `choosedir.test` | 14 | 8 | 6 | 6 | 100.0% |
| `clipboard.test` | 41 | 1 | 40 | 21 | 52.5% |
| `clrpick.test` | 16 | 7 | 9 | 9 | 100.0% |
| `cluster.test` | 73 | 55 | 18 | 0 | 0.0% |
| `cmds.test` | 6 | 2 | 4 | 1 | 25.0% |
| `color.test` | 21 | 17 | 4 | 4 | 100.0% |
| `config.test` | 242 | 229 | 13 | 0 | 0.0% |
| `cursor.test` | 100 | 16 | 84 | 0 | 0.0% |
| `dialog.test` | 6 | 4 | 2 | 0 | 0.0% |
| `embed.test` | 7 | 7 | 0 | 0 | — |
| `entry.test` | 296 | 38 | 258 | 1 | 0.4% |
| `event.test` | 33 | 32 | 1 | 1 | 100.0% |
| `filebox.test` | 115 | 51 | 64 | 62 | 96.9% |
| `focus.test` | 58 | 42 | 16 | 8 | 50.0% |
| `focusTcl.test` | 42 | 42 | 0 | 0 | — |
| `font.test` | 292 | 109 | 183 | 131 | 71.6% |
| `fontchooser.test` | 19 | 10 | 9 | 0 | 0.0% |
| `frame.test` | 204 | 202 | 2 | 1 | 50.0% |
| `geometry.test` | 14 | 0 | 14 | 13 | 92.9% |
| `get.test` | 15 | 0 | 15 | 1 | 6.7% |
| `grab.test` | 29 | 2 | 27 | 18 | 66.7% |
| `grid.test` | 203 | 2 | 201 | 63 | 31.3% |
| `image.test` | 52 | 36 | 16 | 7 | 43.8% |
| `imgBmap.test` | 60 | 34 | 26 | 4 | 15.4% |
| `imgListFormat.test` | 67 | 4 | 63 | 12 | 19.0% |
| `imgPNG.test` | 11 | 1 | 10 | 8 | 80.0% |
| `imgPPM.test` | 34 | 24 | 10 | 0 | 0.0% |
| `imgPhoto.test` | 243 | 50 | 193 | 33 | 17.1% |
| `imgSVGnano.test` | 26 | 0 | 26 | 23 | 88.5% |
| `listbox.test` | 376 | 263 | 113 | 6 | 5.3% |
| `main.test` | 7 | 5 | 2 | 0 | 0.0% |
| `menu.test` | 553 | 413 | 140 | 1 | 0.7% |
| `menuDraw.test` | 67 | 67 | 0 | 0 | — |
| `menubut.test` | 106 | 100 | 6 | 2 | 33.3% |
| `message.test` | 53 | 0 | 53 | 2 | 3.8% |
| `msgbox.test` | 63 | 47 | 16 | 16 | 100.0% |
| `obj.test` | 4 | 0 | 4 | 4 | 100.0% |
| `option.test` | 104 | 4 | 100 | 85 | 85.0% |
| `pack.test` | 199 | 2 | 197 | 179 | 90.9% |
| `packgrid.test` | 18 | 0 | 18 | 1 | 5.6% |
| `panedwindow.test` | 427 | 427 | 0 | 0 | — |
| `pkgconfig.test` | 9 | 3 | 6 | 0 | 0.0% |
| `place.test` | 55 | 1 | 54 | 47 | 87.0% |
| `raise.test` | 34 | 26 | 8 | 4 | 50.0% |
| `safe.test` | 16 | 11 | 5 | 0 | 0.0% |
| `safePrimarySelection.test` | 60 | 60 | 0 | 0 | — |
| `scale.test` | 208 | 190 | 18 | 11 | 61.1% |
| `scrollbar.test` | 169 | 125 | 44 | 34 | 77.3% |
| `select.test` | 111 | 85 | 26 | 19 | 73.1% |
| `send.test` | 74 | 68 | 6 | 1 | 16.7% |
| `spinbox.test` | 319 | 30 | 289 | 1 | 0.3% |
| `systray.test` | 21 | 3 | 18 | 0 | 0.0% |
| `testutils.test` | 21 | 21 | 0 | 0 | — |
| `text.test` | 695 | 11 | 684 | 1 | 0.1% |
| `textBTree.test` | 138 | 121 | 17 | 0 | 0.0% |
| `textDisp.test` | 397 | 330 | 67 | 33 | 49.3% |
| `textImage.test` | 26 | 1 | 25 | 0 | 0.0% |
| `textIndex.test` | 190 | 137 | 53 | 39 | 73.6% |
| `textMark.test` | 46 | 46 | 0 | 0 | — |
| `textTag.test` | 175 | 171 | 4 | 3 | 75.0% |
| `textWind.test` | 112 | 101 | 11 | 0 | 0.0% |
| `tk.test` | 38 | 4 | 34 | 0 | 0.0% |
| `unixButton.test` | 12 | 12 | 0 | 0 | — |
| `unixEmbed.test` | 56 | 56 | 0 | 0 | — |
| `unixFont.test` | 48 | 48 | 0 | 0 | — |
| `unixMenu.test` | 122 | 8 | 114 | 6 | 5.3% |
| `unixSelect.test` | 19 | 18 | 1 | 0 | 0.0% |
| `unixWm.test` | 283 | 110 | 173 | 134 | 77.5% |
| `util.test` | 12 | 12 | 0 | 0 | — |
| `visual.test` | 41 | 40 | 1 | 1 | 100.0% |
| `visual_bb.test` | 1 | 1 | 0 | 0 | — |
| `winButton.test` | 9 | 9 | 0 | 0 | — |
| `winClipboard.test` | 8 | 8 | 0 | 0 | — |
| `winDialog.test` | 66 | 66 | 0 | 0 | — |
| `winFont.test` | 30 | 30 | 0 | 0 | — |
| `winMenu.test` | 144 | 144 | 0 | 0 | — |
| `winMsgbox.test` | 19 | 19 | 0 | 0 | — |
| `winSend.test` | 51 | 51 | 0 | 0 | — |
| `winWm.test` | 29 | 29 | 0 | 0 | — |
| `window.test` | 18 | 14 | 4 | 0 | 0.0% |
| `winfo.test` | 72 | 14 | 58 | 25 | 43.1% |
| `wm.test` | 296 | 56 | 240 | 92 | 38.3% |
| `xmfbox.test` | 8 | 8 | 0 | 0 | — |

## What this measurement does not cover

Every suite file was read to its end, so no file's case count is a floor.

These files declare some of their tests inside a child interpreter, where the recorder cannot see them, so their case counts are a floor too: `focus.test`, `safe.test`, `send.test`.

A case whose stage process made no progress for 20s is killed and recorded as an abort against the case it was on — a failure on the tclrs side, a set-aside on the reference side. Nothing is dropped.

Every failing case is written out in full — its program, the reference outcome and the tclrs outcome — to `tk-conformance/work/failures.txt`, so any number above can be checked one case at a time rather than taken on trust.

## Reproducing this

```sh
tk-conformance/run.sh
```

From a fresh checkout, with a `tclsh` that can `package require Tk` on `PATH` and a stable Rust toolchain, that is the whole reproduction: it fetches the suite, verifies its checksum, extracts every case, runs both sides, and rewrites this file. Intermediate artifacts land in `tk-conformance/work/` and are reused on a rerun, so an interrupted run is cheap to resume; delete that directory to force everything to be recomputed.

The run needs a window server. Both sides open real windows — that is the point of hosting the real Tk — so a headless machine measures nothing here.

