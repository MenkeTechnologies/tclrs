//! Which fusevm execution tier a script's bytecode actually reaches.
//!
//! Enabling the JIT is not the same as being compiled by it, and the only
//! honest way to tell the two apart is to ask the VM. This module runs a script
//! and then queries fusevm's own eligibility and cache predicates —
//! `is_block_eligible`, `block_jit_is_compiled`, `trace_is_compiled`,
//! `find_jit_region` — so the answer comes from the compiler that would have
//! done the work rather than from an assumption about it.
//!
//! `tclrs --tiers script.tcl` prints the report; the README quotes it.

use std::collections::BTreeMap;

use fusevm::{Chunk, ChunkBuilder, JitCompiler, Op};

/// A loop header — the target of a backward branch — and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// Op index of the loop header the backward branch jumps to.
    pub anchor: usize,
    /// Whether fusevm would accept this loop's body as a trace. Asked of
    /// `is_trace_eligible` with the body's ops — the same predicate the
    /// recorder applies to what it recorded, which for a loop whose body has
    /// no early exit is the same op sequence.
    pub trace_eligible: bool,
    /// Whether a compiled trace is installed for this header after the run.
    pub traced: bool,
    /// Whether the tracing JIT gave up on this header.
    pub blacklisted: bool,
}

/// What the tiers did with one script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Ops in the compiled chunk.
    pub ops: usize,
    /// Whether every op in the chunk is block-JIT eligible, which is what the
    /// whole-chunk block tier requires.
    pub block_eligible: bool,
    /// Whether the block tier holds compiled native code for this chunk.
    pub block_compiled: bool,
    /// The largest contiguous block-eligible op range, if any is large enough
    /// for fusevm to consider it worth compiling.
    pub largest_eligible_region: Option<(usize, usize)>,
    /// Every loop header, and whether the tracing JIT compiled it.
    pub loops: Vec<Loop>,
    /// Op kinds the **block** tier refuses, by occurrence count — what keeps
    /// the whole chunk from being compiled in one piece.
    ///
    /// Not the same question as whether a loop is traced: the tracing tier
    /// takes `GetVar` / `SetVar` (fusevm 0.15.0 promotes a referenced global to
    /// a register at trace entry and spills it at every exit), so a chunk can
    /// list those here and still reach native code through a trace.
    pub ineligible: BTreeMap<String, usize>,
}

impl Report {
    /// Whether any tier holds compiled native code for this script.
    pub fn reaches_native(&self) -> bool {
        self.block_compiled || self.loops.iter().any(|l| l.traced)
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "ops                     {}", self.ops)?;
        writeln!(f, "block-JIT eligible      {}", self.block_eligible)?;
        writeln!(f, "block-JIT compiled      {}", self.block_compiled)?;
        match self.largest_eligible_region {
            Some((s, e)) => writeln!(f, "largest eligible region {s}..{e} ({} ops)", e - s)?,
            None => writeln!(f, "largest eligible region none")?,
        }
        if self.loops.is_empty() {
            writeln!(f, "loops                   none")?;
        }
        for l in &self.loops {
            writeln!(
                f,
                "loop @{:<4}             trace-eligible={} traced={} blacklisted={}",
                l.anchor, l.trace_eligible, l.traced, l.blacklisted
            )?;
        }
        if self.ineligible.is_empty() {
            writeln!(f, "block-ineligible ops    none")?;
        } else {
            writeln!(f, "block-ineligible ops")?;
            for (name, count) in &self.ineligible {
                writeln!(f, "  {name:<22}{count}")?;
            }
        }
        write!(f, "reaches native code     {}", self.reaches_native())
    }
}

/// Compile and run `src`, then report which tiers took it.
///
/// The script is run because tier membership is a runtime fact: the block tier
/// compiles after its warmup threshold and the tracing tier only after a loop
/// has gone round enough times to be recorded. Output is discarded.
pub fn report(src: &str) -> Result<Report, String> {
    // The script runs through the ordinary interpreter, so what is measured is
    // what an ordinary run does — including a script that uses `catch` or a
    // coroutine, which needs the driver. The chunk lowered here is the same
    // bytecode, and fusevm keys its compiled code by the chunk's op hash, so
    // asking this copy is asking about the run that just happened.
    let chunk = crate::runtime::compile(src)?;
    let mut interp = crate::Interp::capturing();
    interp.eval(src).map_err(|e| e.to_string())?;
    Ok(inspect(&chunk))
}

/// Report on an already-executed chunk.
pub fn inspect(chunk: &Chunk) -> Report {
    let jit = JitCompiler::new();
    let loops = loop_anchors(&chunk.ops)
        .into_iter()
        .map(|anchor| Loop {
            anchor,
            trace_eligible: body_of(&chunk.ops, anchor)
                .is_some_and(|body| jit.is_trace_eligible(body, anchor)),
            traced: jit.trace_is_compiled(chunk, anchor),
            blacklisted: jit.trace_is_blacklisted(chunk, anchor),
        })
        .collect();

    let mut ineligible: BTreeMap<String, usize> = BTreeMap::new();
    for op in &chunk.ops {
        if !op_is_eligible(&jit, op) {
            *ineligible.entry(op_name(op)).or_default() += 1;
        }
    }

    Report {
        ops: chunk.ops.len(),
        block_eligible: jit.is_block_eligible(chunk),
        block_compiled: jit.block_jit_is_compiled(chunk),
        largest_eligible_region: jit.find_jit_region(chunk),
        loops,
        ineligible,
    }
}

/// Every op index a backward branch jumps to — fusevm anchors a trace at each.
fn loop_anchors(ops: &[Op]) -> Vec<usize> {
    let mut anchors: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(ip, op)| match op {
            Op::Jump(t)
            | Op::JumpIfTrue(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfTrueKeep(t)
            | Op::JumpIfFalseKeep(t)
                if *t <= ip =>
            {
                Some(*t)
            }
            _ => None,
        })
        .collect();
    anchors.sort_unstable();
    anchors.dedup();
    anchors
}

/// The op sequence one iteration of the loop at `anchor` runs: from the header
/// through the backward branch that closes it. `None` when nothing closes it.
fn body_of(ops: &[Op], anchor: usize) -> Option<&[Op]> {
    let close = ops.iter().enumerate().position(|(ip, op)| {
        ip >= anchor
            && matches!(
                op,
                Op::Jump(t) | Op::JumpIfTrue(t) | Op::JumpIfFalse(t)
                    if *t == anchor
            )
    })?;
    Some(&ops[anchor..=close])
}

/// Whether fusevm's block tier accepts this op, asked by handing the JIT a
/// chunk holding just that op. Whole-chunk eligibility is the conjunction of
/// the per-op decision, so a one-op chunk isolates it.
fn op_is_eligible(jit: &JitCompiler, op: &Op) -> bool {
    let mut b = ChunkBuilder::new();
    b.emit(op.clone(), 1);
    jit.is_block_eligible(&b.build())
}

/// An op's variant name, without its operands, so occurrences group.
fn op_name(op: &Op) -> String {
    let text = format!("{op:?}");
    match text.split_once('(') {
        Some((name, _)) => name.to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counted loop written against fusevm slots instead of Tcl variables, in
    /// the unrotated shape. Both tiers report it eligible — which is what makes
    /// the refusals below evidence about Tcl's lowering, and about the trace
    /// compiler's own decision, rather than about a report that only ever says
    /// no. Eligibility is not installation: see
    /// [`the_unrotated_shape_of_that_loop_installs_no_trace`].
    #[test]
    fn a_slot_counter_loop_is_accepted_by_both_tiers() {
        let mut b = ChunkBuilder::new();
        b.emit(Op::GetSlot(0), 1); //  0  header
        b.emit(Op::LoadInt(1000), 1); //  1
        b.emit(Op::NumLt, 1); //  2
        b.emit(Op::JumpIfFalse(9), 1); //  3
        b.emit(Op::GetSlot(0), 1); //  4
        b.emit(Op::LoadInt(1), 1); //  5
        b.emit(Op::Add, 1); //  6
        b.emit(Op::SetSlot(0), 1); //  7
        b.emit(Op::Jump(0), 1); //  8  backward branch
        b.emit(Op::GetSlot(0), 1); //  9
        let report = inspect(&b.build());
        assert!(report.block_eligible, "{report}");
        assert!(report.ineligible.is_empty(), "{report}");
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(report.loops[0].trace_eligible, "{report}");
    }

    /// The Tcl spelling of that same loop. Its counter is a VM global rather
    /// than a frame slot, which the *block* tier still refuses — so the chunk
    /// as a whole is not compiled — but the tracing tier promotes a referenced
    /// global to a register at trace entry and spills it at every exit, so the
    /// loop itself reaches native code.
    #[test]
    fn the_tcl_counter_loop_is_traced_through_its_globals() {
        let report = report("set i 0\nwhile {$i < 1000} {incr i}").expect("runs");
        assert!(!report.block_eligible, "{report}");
        assert!(
            report.ineligible.contains_key("GetVar") && report.ineligible.contains_key("SetVar"),
            "the variable ops are what keep the whole chunk out: {report}"
        );
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(report.loops[0].trace_eligible, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// The same loop inside a `proc` reaches native code. Two things have to
    /// hold at once. A procedure's locals are frame slots, so the counter is
    /// `GetSlot`/`SetSlot` rather than the globals the tiers refuse; and the
    /// loop is emitted rotated — entered at its test, closed by a conditional
    /// backward branch — which is the shape fusevm's trace compiler accepts.
    /// The unrotated `while` shape, a forward `JumpIfFalse` exit closed by an
    /// unconditional backward `Jump`, records an eligible op sequence that the
    /// trace compiler then declines, so nothing is installed.
    ///
    /// The chunk as a whole stays block-ineligible for a different reason: the
    /// call and the `puts` around the loop.
    #[test]
    fn a_proc_local_counter_loop_reaches_a_compiled_trace() {
        let report =
            report("proc f {} {set i 0; while {$i < 200000} {incr i}; return $i}\nputs [f]")
                .expect("runs");
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(
            report.loops[0].trace_eligible,
            "slot ops make the body traceable: {report}"
        );
        assert!(
            !report.ineligible.contains_key("GetVar") && !report.ineligible.contains_key("SetVar"),
            "a procedure's locals are slots, not globals: {report}"
        );
        assert!(report.loops[0].traced, "{report}");
        assert!(!report.loops[0].blacklisted, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// The rotation is what installs the trace, not the slot ops alone. The
    /// same body built by hand in the unrotated shape — the one `while` used to
    /// emit — records and is then declined, so no trace exists for it. Without
    /// this the test above could pass for a reason that has nothing to do with
    /// the loop's shape.
    #[test]
    fn the_unrotated_shape_of_that_loop_installs_no_trace() {
        // `i = 0; while (i < N) { i += 1 }` with a forward exit and an
        // unconditional backward close.
        let mut b = ChunkBuilder::new();
        b.emit(Op::LoadInt(0), 1);
        b.emit(Op::SetSlot(0), 1);
        let anchor = b.current_pos();
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(200_000), 1);
        b.emit(Op::NumLt, 1);
        let exit = b.emit(Op::JumpIfFalse(usize::MAX), 1);
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(1), 1);
        b.emit(Op::Add, 1);
        b.emit(Op::SetSlot(0), 1);
        b.emit(Op::Jump(anchor), 1);
        let end = b.current_pos();
        b.patch_jump(exit, end);
        b.emit(Op::GetSlot(0), 1);
        let chunk = b.build();

        let mut vm = fusevm::VM::new(chunk.clone());
        vm.enable_tracing_jit();
        vm.run();

        let report = inspect(&chunk);
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(
            report.loops[0].trace_eligible,
            "the recorded sequence is eligible; it is the compile that declines: {report}"
        );
        assert!(!report.loops[0].traced, "{report}");
    }

    /// And the rotated spelling of that same hand-built loop does install one,
    /// with no Tcl in the picture at all.
    #[test]
    fn the_rotated_shape_of_that_loop_installs_a_trace() {
        let mut b = ChunkBuilder::new();
        b.emit(Op::LoadInt(0), 1);
        b.emit(Op::SetSlot(0), 1);
        let enter = b.emit(Op::Jump(usize::MAX), 1);
        let body = b.current_pos();
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(1), 1);
        b.emit(Op::Add, 1);
        b.emit(Op::SetSlot(0), 1);
        let cond = b.current_pos();
        b.patch_jump(enter, cond);
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(200_000), 1);
        b.emit(Op::NumLt, 1);
        b.emit(Op::JumpIfTrue(body), 1);
        b.emit(Op::GetSlot(0), 1);
        let chunk = b.build();

        let mut vm = fusevm::VM::new(chunk.clone());
        vm.enable_tracing_jit();
        vm.run();

        let report = inspect(&chunk);
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(report.loops[0].traced, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// Tcl arithmetic lowers to ops the JIT accepts — all of them. An `expr`
    /// used to end in an extension op that normalized its result, and that one
    /// op was enough to keep every arithmetic loop out of both the JIT and the
    /// ahead-of-time compiler; Tcl's string form is applied where a string is
    /// asked for instead, so nothing ineligible is left in the expression.
    #[test]
    fn expr_arithmetic_lowers_to_eligible_ops() {
        let report = report("expr {2 + 3 * 4 << 1}").expect("runs");
        assert!(
            report.ineligible.is_empty(),
            "an expression should lower to eligible ops only: {report}"
        );
        assert!(report.block_eligible, "{report}");
    }

    /// The same for an assignment whose value is an expression, which is the
    /// shape a counted loop is written in: `set i [expr {$i + 1}]` reaches the
    /// ahead-of-time compiler only if nothing in it is an extension op.
    #[test]
    fn an_expr_assignment_lowers_to_eligible_ops() {
        let report = report("set i 0\nset i [expr {$i + 1}]").expect("runs");
        assert!(
            report
                .ineligible
                .keys()
                .all(|op| op == "GetVar" || op == "SetVar"),
            "only the global-variable ops should be left: {report}"
        );
    }
}
