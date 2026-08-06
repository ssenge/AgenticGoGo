//! `agg/private/calls.jsonl` — the durable record of every call that crossed the agg API boundary.
//!
//! This is what makes a Rust driver RESUMABLE. A driver's position in its own control flow is never
//! serialized; instead each completed call appends one line here, and on `--resume` the same calls
//! are answered from the file instead of being executed. The driver walks itself back to the
//! interruption point by *running*, because identical inputs produce identical branches.
//!
//! ## Why it lives in `agg/private/`
//! `paths.rs` states the rule: *if the worker writing it could change when the loop ends, what it
//! may spend, or what agg believes happened, it belongs here.* A worker able to append rows to this
//! file could make agg **skip work and fabricate verdicts** — strictly worse than the
//! `verdicts.jsonl` hole the private split exists to close.
//!
//! ## Relationship to `verdicts.jsonl`
//! Different questions, both durable. `verdicts.jsonl` records what a GATE decided about a span —
//! one row per judge per gate, the record of what landed. This file records what the DRIVER asked
//! for, at call time, in order. A judge consulted three times in one span appears once per gate
//! there and three times here.

use crate::core::model::Verdict;
use crate::driver::{GateOutcome, StepOutcome};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// Which notification call produced a [`CallRecord::Note`].
///
/// ⚠ `Log` is in this enum deliberately. The driver contract tells authors that an `agg.*` call is a
/// safe place to put a once-only side effect, and `agg.log` is one of them — leaving it out would
/// make that advice false. All four consume an ordinal and all four replay as no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLevel {
    Log,
    Info,
    Ask,
    Block,
}

impl NoteLevel {
    /// The wire spelling the facade uses for these calls — the same four strings `Agg::note` passes.
    pub fn tag(self) -> &'static str {
        match self {
            NoteLevel::Log => "log",
            NoteLevel::Info => "info",
            NoteLevel::Ask => "ask",
            NoteLevel::Block => "block",
        }
    }
}

/// One completed call. `ord` is the call's position in the driver's execution, `label` is the
/// breadcrumb (`Agg::label_path`) as of that call — the consistency check that a resumed driver is
/// really the same driver in the same place.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallRecord {
    Step { ord: u64, label: String, outcome: StepOutcome, ts: u64 },
    /// ⛔ `tokens`/`cost` are on Judge as well as on Step, and that is NOT redundancy. Judges spend:
    /// an LLM rubric is a real ruler call charged to the run counters. Restoring counters from the
    /// last `Step` record alone would drop every judge's bill on the floor, and `over_budget` would
    /// gain that much fresh headroom on every resume — a laundered ceiling is a moat hole.
    Judge { ord: u64, label: String, judge: String, verdict: Verdict, tokens: u64, cost: f64, ts: u64 },
    /// ⛔ REQUIRED, and not merely for symmetry: it is the truncate boundary. Without a Gate record
    /// [`truncate_to_base`] has no record kind to search for and the OD-12 rule is unimplementable
    /// against its own schema — a resumed gate would re-execute a real merge.
    Gate { ord: u64, label: String, outcome: GateOutcome, ts: u64 },
    Note { ord: u64, label: String, level: NoteLevel, msg: String, ts: u64 },
}

impl CallRecord {
    pub fn ord(&self) -> u64 {
        match self {
            CallRecord::Step { ord, .. }
            | CallRecord::Judge { ord, .. }
            | CallRecord::Gate { ord, .. }
            | CallRecord::Note { ord, .. } => *ord,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            CallRecord::Step { label, .. }
            | CallRecord::Judge { label, .. }
            | CallRecord::Gate { label, .. }
            | CallRecord::Note { label, .. } => label,
        }
    }

    /// WHAT this call was, as the identity a resumed run must match: a step's step-name, a judge's
    /// judge-name, `gate`, or `note:<level>`.
    ///
    /// The level is part of a note's identity on purpose — an `info` that has become an `ask` is a
    /// changed control flow, and answering the second from the first would replay a `block()` as a
    /// no-op without ever asking the human.
    pub fn what(&self) -> String {
        match self {
            CallRecord::Step { outcome, .. } => outcome.step.clone(),
            CallRecord::Judge { judge, .. } => judge.clone(),
            CallRecord::Gate { .. } => "gate".into(),
            CallRecord::Note { level, .. } => format!("note:{}", level.tag()),
        }
    }

    /// Spend to restore into the run counters when this record is fast-forwarded.
    ///
    /// A `Step`'s counters are CUMULATIVE as of that step (they are assigned, not added); a
    /// `Judge`'s are that judge's own bill and accumulate on top. `None` = assign, `Some` = add.
    pub fn spend(&self) -> Option<(u64, f64)> {
        match self {
            CallRecord::Judge { tokens, cost, .. } => Some((*tokens, *cost)),
            _ => None,
        }
    }
}

/// Append one record, durably.
///
/// Discipline copied verbatim from `verdicts::append`, including the reasoning: **a failed write is
/// a hard `Err`, never a swallow.** An un-fsynced record means work is redone (tolerable) or, far
/// worse, an out-of-order ordinal (not). This is deliberately the opposite of `bus.rs::append_log`,
/// which swallows everything — correct for an audit log, wrong for state the loop reads back.
pub fn append(dir: &Path, rec: &CallRecord) -> Result<()> {
    let path = crate::paths::calls_jsonl(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let line = serde_json::to_string(rec).context("serializing call record")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(line.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .with_context(|| format!("appending to {}", path.display()))?;
    f.sync_data().with_context(|| format!("fsync {}", path.display()))?;
    Ok(())
}

/// Every record, in order.
///
/// ⚠ **A corrupt FINAL line is DROPPED, not an error.** That is the signature of a power cut
/// mid-append, and dropping it is exactly right: the call it describes did not complete, so it must
/// re-execute. A corrupt line anywhere else is a real error — the file is append-only, so a bad line
/// with good lines after it means something other than a crash wrote here.
pub fn load(dir: &Path) -> Result<Vec<CallRecord>> {
    let Ok(text) = std::fs::read_to_string(crate::paths::calls_jsonl(dir)) else {
        return Ok(Vec::new());
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<CallRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) if i + 1 == lines.len() => {
                eprintln!("  [resume] dropping a torn final ledger line (a crash mid-append): {e}");
            }
            Err(e) => anyhow::bail!("calls.jsonl line {} is corrupt (not a torn tail): {e}", i + 1),
        }
    }
    Ok(out)
}

/// Start a fresh ledger — a run that is NOT resuming must not fast-forward against an old one.
pub fn truncate(dir: &Path) -> Result<()> {
    let path = crate::paths::calls_jsonl(dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// **The OD-12 rule, and it is not optional.** Drop every record after the last `Gate` that returned
/// [`GateOutcome::Kept`], and return what survives.
///
/// # Why the boundary is a kept gate and nothing else
/// Fast-forward is sound only for a call whose effect LANDED ON BASE. Since `step()` always stages,
/// every step's work sits on a per-run span branch until a gate lands it — and span state is per-run
/// and rebuilt empty, so the ledger does not carry it. Replaying a staged step as "done" tells the
/// next session that work exists which is in fact an orphaned ref: the exploration is silently gone,
/// and it is then graded, merged and recorded as if it were there. That is not a lossy resume, it is
/// a wrong one.
///
/// So the resume rule is one sentence instead of a per-call predicate: **truncate back to the last
/// gate that kept.**
///
/// ⚠ It prints what it drops, because the cost is real and invisible otherwise: those calls
/// re-execute, and an already-ANSWERED `block()` is among them, with the answer unrecoverable.
pub fn truncate_to_base(dir: &Path) -> Result<Vec<CallRecord>> {
    let all = load(dir)?;
    let keep = all
        .iter()
        .rposition(|r| matches!(r, CallRecord::Gate { outcome: GateOutcome::Kept, .. }))
        .map(|i| i + 1)
        .unwrap_or(0);
    if keep == all.len() {
        return Ok(all);
    }
    let dropped = &all[keep..];
    eprintln!(
        "  [resume] dropping {} ledger record(s) after the last kept gate — they will RE-EXECUTE:",
        dropped.len()
    );
    for r in dropped {
        eprintln!("             ord {} · {} `{}`", r.ord(), kind_of(r), r.what());
    }
    if dropped.iter().any(|r| matches!(r, CallRecord::Note { level: NoteLevel::Block, .. })) {
        eprintln!("           ⚠ one of them is an ANSWERED block() — it will ask again, and the answer is not recoverable.");
    }
    let kept: Vec<CallRecord> = all.into_iter().take(keep).collect();
    rewrite(dir, &kept)?;
    Ok(kept)
}

fn kind_of(r: &CallRecord) -> &'static str {
    match r {
        CallRecord::Step { .. } => "step",
        CallRecord::Judge { .. } => "judge",
        CallRecord::Gate { .. } => "gate",
        CallRecord::Note { .. } => "note",
    }
}

/// Rewrite the ledger to exactly `keep`. Write-to-temp + rename, so a crash here cannot leave a
/// half-written ledger — the one file whose truncation must be atomic.
fn rewrite(dir: &Path, keep: &[CallRecord]) -> Result<()> {
    let path = crate::paths::calls_jsonl(dir);
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        for r in keep {
            let line = serde_json::to_string(r).context("serializing call record")?;
            f.write_all(line.as_bytes()).and_then(|_| f.write_all(b"\n"))?;
        }
        f.sync_data().with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{GateFailure, Landing};

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("agg-calls-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn step(ord: u64, name: &str) -> CallRecord {
        CallRecord::Step {
            ord,
            label: format!("cycle {ord}/9"),
            outcome: StepOutcome {
                step: name.into(),
                session: ord as u32,
                landed: Landing::Span,
                verdicts: vec![],
                tokens: ord * 100,
                cost: 0.5,
                secs: 1,
                exit: 0,
            },
            ts: 1,
        }
    }
    fn gate(ord: u64, outcome: GateOutcome) -> CallRecord {
        CallRecord::Gate { ord, label: "g".into(), outcome, ts: 1 }
    }

    /// A ledger round-trips, in order, through a real file.
    #[test]
    fn records_round_trip_in_order() {
        let d = tmp("roundtrip");
        for r in [step(0, "survey"), gate(1, GateOutcome::Kept), step(2, "implement")] {
            append(&d, &r).unwrap();
        }
        let back = load(&d).unwrap();
        assert_eq!(back.iter().map(|r| r.ord()).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(back[0].what(), "survey");
        assert_eq!(back[2].what(), "implement");
    }

    /// A torn FINAL line is the signature of a power cut mid-append: drop it, because the call it
    /// describes never completed and must re-execute. Everything before it survives.
    #[test]
    fn a_torn_final_line_is_dropped_not_an_error() {
        let d = tmp("torn");
        append(&d, &step(0, "a")).unwrap();
        append(&d, &step(1, "b")).unwrap();
        let p = crate::paths::calls_jsonl(&d);
        let mut text = std::fs::read_to_string(&p).unwrap();
        text.push_str("{\"kind\":\"step\",\"ord\":2,\"lab");   // cut mid-write
        std::fs::write(&p, text).unwrap();

        let back = load(&d).unwrap();
        assert_eq!(back.len(), 2, "the two COMPLETE records survive: {back:?}");
    }

    /// …but a corrupt line with good lines AFTER it is not a crash — the file is append-only, so
    /// something else wrote here. That must be loud, not silently skipped.
    #[test]
    fn a_corrupt_line_in_the_middle_is_an_error() {
        let d = tmp("midcorrupt");
        append(&d, &step(0, "a")).unwrap();
        let p = crate::paths::calls_jsonl(&d);
        let good = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("{{not json\n{good}")).unwrap();
        assert!(load(&d).unwrap_err().to_string().contains("corrupt"));
    }

    /// ⛔ THE OD-12 RULE. Fast-forward is sound only for a call whose effect LANDED ON BASE, and
    /// since `step()` always stages, that boundary is the last gate that KEPT. Everything after it
    /// describes work parked on per-run span branches the ledger cannot carry — replaying it as
    /// "done" would tell the next session that orphaned refs are real work.
    #[test]
    fn truncate_to_base_drops_everything_after_the_last_kept_gate() {
        let d = tmp("od12");
        for r in [
            step(0, "a"),
            gate(1, GateOutcome::Kept),      // ← the boundary
            step(2, "b"),
            gate(3, GateOutcome::RolledBack), // NOT a boundary: nothing landed
            step(4, "c"),
        ] {
            append(&d, &r).unwrap();
        }
        let kept = truncate_to_base(&d).unwrap();
        assert_eq!(kept.iter().map(|r| r.ord()).collect::<Vec<_>>(), [0, 1]);
        // …and the FILE was rewritten, not just the returned vec — a resume must not see them again.
        assert_eq!(load(&d).unwrap().len(), 2);
    }

    /// A `Failed` gate is not a boundary either: nothing merged, so the span is still open.
    #[test]
    fn only_a_kept_gate_is_the_boundary() {
        let d = tmp("failedgate");
        for r in [step(0, "a"), gate(1, GateOutcome::Failed(GateFailure::Conflict)), step(2, "b")] {
            append(&d, &r).unwrap();
        }
        assert!(truncate_to_base(&d).unwrap().is_empty(), "no kept gate ⇒ nothing is fast-forwardable");
    }

    /// With no gate at all the whole ledger is unusable — every step is still staged.
    #[test]
    fn a_ledger_with_no_kept_gate_truncates_to_nothing() {
        let d = tmp("nogate");
        append(&d, &step(0, "a")).unwrap();
        append(&d, &step(1, "b")).unwrap();
        assert!(truncate_to_base(&d).unwrap().is_empty());
        assert!(load(&d).unwrap().is_empty(), "the file is rewritten empty, not left stale");
    }

    /// A judge's spend rides on its OWN record. Restoring counters from `Step` records alone would
    /// drop every judge's bill and hand `over_budget` fresh headroom on every resume.
    #[test]
    fn a_judge_record_carries_its_own_spend() {
        let d = tmp("judgespend");
        let j = CallRecord::Judge {
            ord: 0,
            label: "cycle 1/2".into(),
            judge: "load_ok".into(),
            verdict: Verdict::binary(true),
            tokens: 137_000,
            cost: 2.81,
            ts: 1,
        };
        append(&d, &j).unwrap();
        let back = load(&d).unwrap();
        assert_eq!(back[0].spend(), Some((137_000, 2.81)));
        assert_eq!(back[0].what(), "load_ok");
        assert_eq!(step(0, "x").spend(), None, "a step's counters are CUMULATIVE — assigned, not added");
    }

    /// A non-resume run must not fast-forward against a previous run's ledger.
    #[test]
    fn truncate_clears_the_ledger_and_tolerates_absence() {
        let d = tmp("fresh");
        truncate(&d).unwrap(); // no file yet — not an error
        append(&d, &step(0, "a")).unwrap();
        truncate(&d).unwrap();
        assert!(load(&d).unwrap().is_empty());
    }
}
