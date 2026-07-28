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

use fusevm::{Chunk, ChunkBuilder, JitCompiler, Op, VM};

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
    /// Op kinds in this chunk that no JIT tier accepts, by occurrence count.
    /// These are what keeps the chunk out of native code.
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
            writeln!(f, "JIT-ineligible ops      none")?;
        } else {
            writeln!(f, "JIT-ineligible ops")?;
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
    let chunk = crate::runtime::compile(src)?;
    let mut vm = VM::new(chunk.clone());
    let errors = crate::runtime::install_hooks(&mut vm);
    vm.set_output_sink(Box::new(|_: &str| {}));
    let outcome = vm.run();
    crate::runtime::finish(outcome, &errors)?;
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

    /// The same counted loop written against fusevm slots instead of Tcl
    /// variables. The tiers accept it — which is what makes the negative
    /// results below evidence about Tcl's lowering rather than about a report
    /// that only ever says no.
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

    /// The Tcl spelling of that same loop. Its arithmetic is native ops, but
    /// its counter is a VM global, and no tier takes `GetVar`/`SetVar` — so
    /// the chunk is refused whole and the loop body is refused as a trace.
    #[test]
    fn the_tcl_counter_loop_reaches_no_tier() {
        let report = report("set i 0\nwhile {$i < 1000} {incr i}").expect("runs");
        assert!(!report.block_eligible, "{report}");
        assert!(
            report.ineligible.contains_key("GetVar") && report.ineligible.contains_key("SetVar"),
            "the variable ops are what disqualify it: {report}"
        );
        assert_eq!(report.loops.len(), 1, "{report}");
        assert!(!report.loops[0].trace_eligible, "{report}");
        assert!(!report.reaches_native(), "{report}");
    }

    /// Tcl arithmetic lowers to ops the JIT accepts: everything in an `expr`
    /// but the frontend's own result-normalizing extension op is eligible.
    #[test]
    fn expr_arithmetic_lowers_to_eligible_ops() {
        let report = report("expr {2 + 3 * 4 << 1}").expect("runs");
        assert_eq!(
            report.ineligible.keys().collect::<Vec<_>>(),
            vec!["Extended"],
            "{report}"
        );
    }
}
