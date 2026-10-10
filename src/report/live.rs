//! The parts of the report that are ALSO published live, in `state.json`, so `agg status --json` can
//! show them mid-run without re-judging: the stop detectors, each ratcheted judge's reference, and
//! a wait the loop is sitting in.
//!
//! They are computed from the engine's in-memory gate state at every publish — never re-read from
//! `gate.json` or `verdicts.jsonl` — so the live view and the final report say the same thing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::ratchet::{Direction, Gate};
use crate::state::JudgeView;

/// The stop detectors as they stand: the gate's own history plus the `stalled` judge's last word.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detectors {
    /// the last verdict of a judge named `stalled`, if the run-set has one and it has run; `null`
    /// otherwise. Unlike the rest, `stalled` is a JUDGE (it reads `verdicts.jsonl`), not a gate term.
    pub stalled: Option<bool>,
    /// A→B→A over the unmet DoD-set within `sequence.oscillation_window`.
    pub oscillating: bool,
    /// sessions the regression policy rolled back, this run.
    pub rollbacks: u32,
    /// rollbacks since the last kept gate.
    pub consecutive_rollbacks: u32,
    /// the most recent judged gate regressed a ratchet.
    pub ratchet_regressed: bool,
}

/// A ratcheted judge's ratchet: what it is held to now, and the rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatchetView {
    /// the value at the last KEPT gate (or the seed / baseline before the first keep). `null` until
    /// one exists — on the driver path, before the judge's first pre-step verdict.
    pub reference: Option<f64>,
    pub direction: Direction,
    pub tolerance: f64,
    pub tolerance_pct: Option<f64>,
}

/// The loop is waiting for something outside it before the next session — `null` when it is not.
/// The only reason today is `rate_limit`: the agent's provider refused, and agg waits for the reset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waiting {
    /// why: `rate_limit`.
    pub reason: String,
    /// the agent that is limited.
    pub agent: String,
    /// epoch seconds the wait ends, if known.
    pub until: Option<u64>,
}

/// The detectors, from the gate's in-memory state and the published scoreboard.
pub fn detectors(gate: &Gate, judges: &[JudgeView]) -> Detectors {
    Detectors {
        stalled: judges.iter().find(|j| j.name == "stalled" && j.judged).map(|j| j.met),
        oscillating: gate.oscillating(),
        rollbacks: gate.state.rollbacks,
        consecutive_rollbacks: gate.state.consecutive_rollbacks,
        ratchet_regressed: gate.state.ratchet_regressed(),
    }
}

/// Every ratcheted judge's ratchet, by name.
pub fn ratchets(gate: &Gate) -> BTreeMap<String, RatchetView> {
    gate.specs
        .iter()
        .map(|(name, spec)| {
            let view = RatchetView {
                reference: gate.state.reference(name),
                direction: spec.direction,
                tolerance: spec.tolerance,
                tolerance_pct: spec.tolerance_pct,
            };
            (name.clone(), view)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ratchet::{GateDecision, Ratchet};

    /// The gate's counters and the `stalled` judge's verdict land in one place; a `stalled` judge
    /// that has not run yet (or does not exist) is `null`, not `false`.
    #[test]
    fn detectors_read_the_gate_and_the_stalled_judge() {
        let mut gate = Gate::default();
        gate.record(GateDecision::RolledBack, &[], &[], vec![]);
        gate.record(GateDecision::RolledBack, &[], &[], vec![]);
        let stalled = |judged, met| JudgeView { name: "stalled".into(), judged, met, ..Default::default() };

        let d = detectors(&gate, &[stalled(true, true)]);
        assert_eq!((d.rollbacks, d.consecutive_rollbacks, d.stalled), (2, 2, Some(true)));
        assert!(!d.oscillating && !d.ratchet_regressed);
        assert_eq!(detectors(&gate, &[stalled(true, false)]).stalled, Some(false), "ran, not stalled");
        assert_eq!(detectors(&gate, &[stalled(false, false)]).stalled, None, "not run yet");
        assert_eq!(detectors(&gate, &[]).stalled, None, "no stalled judge in the run-set");
    }

    /// A seeded ratchet shows its seed as the reference; an unseeded one has none until measured.
    #[test]
    fn ratchets_show_the_reference_and_the_rule() {
        let mut specs = BTreeMap::new();
        specs.insert("cc".to_string(), Ratchet::down(0.5).baseline(12.0));
        specs.insert("cov".to_string(), Ratchet::up(0.0).tolerance_pct(1.0));
        let r = ratchets(&Gate::new(specs, 4));
        assert_eq!(r["cc"], RatchetView { reference: Some(12.0), direction: Direction::Down, tolerance: 0.5, tolerance_pct: None });
        assert_eq!((r["cov"].reference, r["cov"].tolerance_pct), (None, Some(1.0)));
    }
}
