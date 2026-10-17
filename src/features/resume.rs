//! `agg run --resume` — the YAML path continues an interrupted run (A-18.3, decision 5).
//!
//! It REUSES the driver's call ledger (`agg/private/calls.jsonl`) instead of adding a mechanism: the
//! YAML loop writes one `step` record per session and one `gate` record per judged session, and a
//! resume truncates to the last KEPT gate — the same OD-12 rule, for the same reason: only work that
//! landed on base is sound to skip. What it restores:
//!
//! | what | from |
//! |---|---|
//! | the walk position | the last replayed `step` record's label (`sequence[<idx>] rep <n>`) |
//! | the ratchet + gate history | the last replayed `gate` record |
//! | session counter, tokens, cost | `state.json` as the run left it — never laundered: work after the last kept gate re-executes, but its spend stays spent and its session numbers stay used |
//! | the run id, the clock | `clock.json` |
//!
//! An interrupted session's work is never judged: it was saved under `refs/agg/partial/<run>/<n>`
//! when it stopped (or by `GitSetup` on the resume, after a crash), and the resume starts session
//! n+1 from the gated tree.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::core::calls::CallRecord;
use crate::core::ratchet::GateState;
use crate::driver::{GateOutcome, Landing, StepOutcome};
use crate::loop_::{AGGScratch, LoopState};
use crate::state::AgentUsage;

/// What a resumed YAML run restores, read before this run writes anything.
#[derive(Debug)]
pub struct Resume {
    /// the walk position after the last replayed step: (entry, repetitions, step name).
    walk: Option<(usize, u32, String)>,
    /// the gate state as of the last kept gate.
    gate: Option<GateState>,
    /// records that survive the truncation — the next ordinal.
    ord: u64,
    session: u32,
    tokens: u64,
    cost: f64,
    per_agent: BTreeMap<String, AgentUsage>,
}

/// The YAML path's ledger cursor, kept in the run's `ext` store.
#[derive(Default)]
pub struct Ledger {
    pub ord: u64,
}

/// Read what a resume continues from, refusing a project with nothing to continue.
pub fn load(dir: &Path) -> Result<Resume> {
    let last = crate::project::Project::load(dir).runs.pop();
    let Some(last) = last else {
        bail!("--resume: this project has no previous run to continue — start one with `agg run`")
    };
    if last.end_reason == "goals-met" {
        bail!("--resume: the last run (#{}) already met done_if — there is nothing to continue", last.run);
    }
    let Some(state) = crate::state::DashboardState::read(dir) else {
        bail!("--resume: no {} from the previous run to continue from", crate::paths::state_json(dir).display())
    };
    let replay = crate::core::calls::truncate_to_base(dir)?;
    eprintln!("  [resume] continuing run #{} after session #{} — {} ledger record(s) kept", last.run, state.session, replay.len());
    Ok(Resume {
        walk: replay.iter().rev().find_map(walk_of),
        gate: replay.iter().rev().find_map(|r| match r {
            CallRecord::Gate { gate, .. } => gate.clone(),
            _ => None,
        }),
        ord: replay.len() as u64,
        session: state.session,
        tokens: state.tokens_spent,
        cost: state.cost_spent,
        per_agent: state.per_agent,
    })
}

/// The walk position a `step` record carries in its label.
fn walk_of(r: &CallRecord) -> Option<(usize, u32, String)> {
    let CallRecord::Step { label, outcome, .. } = r else { return None };
    let (idx, reps) = parse_label(label)?;
    Some((idx, reps, outcome.step.clone()))
}

/// The label a YAML `step` record carries: where the walk stood after dispatching it.
pub fn label(idx: usize, reps: u32) -> String {
    format!("sequence[{idx}] rep {reps}")
}

fn parse_label(label: &str) -> Option<(usize, u32)> {
    let rest = label.strip_prefix("sequence[")?;
    let (idx, reps) = rest.split_once("] rep ")?;
    Some((idx.parse().ok()?, reps.parse().ok()?))
}

/// Restore the counters BEFORE the baseline pass, so the baseline's own spend adds on top of them.
pub fn restore_counters(st: &mut LoopState, r: &Resume) {
    st.session = r.session;
    st.tokens_spent = r.tokens;
    st.cost_spent = r.cost;
    st.per_agent = r.per_agent.clone();
    st.ledger.inherit(r.session, r.tokens);
    st.lifetime_base = st.lifetime_base.saturating_sub(r.session);
    st.dash.session = r.session;
    st.dash.lifetime_session = st.lifetime_base + r.session;
    st.ext.get::<Ledger>().ord = r.ord;
}

/// Restore the gate and the walk AFTER the baseline pass — the baseline seeds the ratchet from the
/// tree, and a resumed run's references are the ones the last kept gate held, not a fresh measure.
pub fn restore_position(st: &mut LoopState, r: &Resume) -> Result<()> {
    if let Some(gate) = &r.gate {
        // a reference taken under another scoring definition is reset, not restored.
        for w in st.eng.gate.restore(gate.clone()) {
            eprintln!("  ⚠ {w}");
        }
        crate::core::ratchet::save(&st.dir, &st.eng.gate.state)?;
    }
    if let Some((idx, reps, step)) = &r.walk {
        st.cursor.seek(*idx, *reps, step)?;
    }
    st.publish();
    Ok(())
}

/// Record one finished YAML session in the call ledger: its `step`, and — for a judged session —
/// the `gate` that decided it. A `skip_judges` session is staged, not gated: its gate is the next
/// judged session's.
pub fn record(st: &mut LoopState, outcome: &StepOutcome) -> Result<()> {
    let judged = !st.scratch.get::<AGGScratch>().skip_judges;
    let (idx, reps) = st.cursor.position();
    let ts = crate::util::now_epoch();
    let mut ord = st.ext.get::<Ledger>().ord;
    let step = CallRecord::Step { ord, label: label(idx, reps), outcome: outcome.clone(), ts };
    crate::core::calls::append(&st.dir, &step)?;
    ord += 1;
    if judged {
        let decided = match outcome.landed {
            Landing::Base => GateOutcome::Kept,
            Landing::RolledBack => GateOutcome::RolledBack,
            Landing::Nothing | Landing::Span => GateOutcome::Nothing,
        };
        let gate = Some(st.eng.gate.state.clone());
        crate::core::calls::append(&st.dir, &CallRecord::Gate { ord, label: label(idx, reps), outcome: decided, gate, ts })?;
        ord += 1;
    }
    st.ext.get::<Ledger>().ord = ord;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_round_trips_the_walk_position() {
        assert_eq!(parse_label(&label(3, 2)), Some((3, 2)));
        assert_eq!(parse_label("cycle 1/9"), None, "a driver's breadcrumb is not a walk position");
        assert_eq!(parse_label("sequence[x] rep 1"), None);
    }

    fn step(ord: u64, name: &str, label: &str, session: u32) -> CallRecord {
        let outcome = StepOutcome {
            step: name.into(),
            session,
            landed: Landing::Base,
            verdicts: vec![],
            tokens: 0,
            cost: 0.0,
            secs: 0,
            exit: 0,
            agent_session: None,
        };
        CallRecord::Step { ord, label: label.into(), outcome, ts: 0 }
    }

    /// The walk resumes from the last step BEFORE the truncation point, and the gate state from
    /// the last kept gate; a project whose last run met its DoD has nothing to resume.
    #[test]
    fn load_reads_the_walk_and_the_gate_as_of_the_last_kept_gate() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        assert!(load(dir).unwrap_err().to_string().contains("no previous run"));

        let mut l = crate::project::RunLedger::begin(dir, "p", 1, 1);
        l.update(3, 300, 0, 1);
        l.finish(2, "interrupted");
        drop(l);
        let state = crate::state::DashboardState { session: 3, tokens_spent: 300, ..Default::default() };
        std::fs::create_dir_all(crate::paths::private_dir(dir)).unwrap();
        std::fs::write(crate::paths::state_json(dir), serde_json::to_string(&state).unwrap()).unwrap();
        let kept = GateState { rollbacks: 1, ..Default::default() };
        for r in [
            step(0, "plan", &label(0, 1), 1),
            CallRecord::Gate { ord: 1, label: label(0, 1), outcome: GateOutcome::Kept, gate: Some(kept.clone()), ts: 0 },
            step(2, "build", &label(1, 1), 2),
            CallRecord::Gate { ord: 3, label: label(1, 1), outcome: GateOutcome::RolledBack, gate: None, ts: 0 },
        ] {
            crate::core::calls::append(dir, &r).unwrap();
        }

        let r = load(dir).unwrap();
        assert_eq!(r.walk, Some((0, 1, "plan".into())), "the rolled-back `build` re-executes");
        assert_eq!(r.gate, Some(kept));
        assert_eq!((r.ord, r.session, r.tokens), (2, 3, 300), "counters are the interrupted run's, not the gate's");

        let mut l = crate::project::RunLedger::begin(dir, "p", 1, 3);
        l.finish(4, "goals-met");
        drop(l);
        assert!(load(dir).unwrap_err().to_string().contains("already met done_if"));
    }
}
