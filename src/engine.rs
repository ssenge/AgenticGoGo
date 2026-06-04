//! The cycle engine: run all judges, fold verdicts into goals, evaluate stop/halt.
//!
//! This is the goal logic the loop calls once per cycle (after a worker exits).
//! Phase 1 keeps the loop itself minimal; `agg plan` exercises this engine for a
//! single dry-run cycle, which is the walking skeleton's testable core.

use crate::config::GoalsConfig;
use crate::judge;
use crate::model::{Goal, Lifecycle};
use crate::stop::{self, StopContext};
use anyhow::Result;
use std::path::Path;

/// Outcome of evaluating the goal set after a cycle.
#[derive(Debug)]
pub struct CycleResult {
    pub stop: bool,           // success stop condition met
    pub halt: bool,           // halt/guard condition met (e.g. invariant regressed)
    pub halt_reason: Option<String>,
    /// per-goal changes this cycle (feeds the LLM summarizer)
    pub deltas: Vec<GoalDelta>,
}

/// What changed for one goal across a cycle. The summarizer turns these into
/// "tests_pass 8→9 cardinal; inv_build green; coverage still failing".
#[derive(Debug, Clone)]
pub struct GoalDelta {
    pub id: String,
    pub before_value: f64,
    pub after_value: f64,
    pub before_state: Lifecycle,
    pub after_state: Lifecycle,
    pub rationale: String,
}

impl GoalDelta {
    /// True if anything meaningful changed (value moved or state changed).
    pub fn changed(&self) -> bool {
        self.before_value != self.after_value || self.before_state != self.after_state
    }
    /// One-line human summary of the change.
    pub fn line(&self) -> String {
        let state = if self.before_state != self.after_state {
            format!("{:?}→{:?}", self.before_state, self.after_state)
        } else {
            format!("{:?}", self.after_state)
        };
        if self.before_value != self.after_value {
            format!("{}: {}→{} [{}] {}", self.id, self.before_value, self.after_value, state, self.rationale)
        } else {
            format!("{}: {} [{}] {}", self.id, self.after_value, state, self.rationale)
        }
    }
}

/// Run-level facts the stop/halt expressions can reference (budget #5).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunState {
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    pub wall_hours: f64,
}

pub struct Engine {
    pub goals: Vec<Goal>,
    pub stop_when: String,
    pub halt_when: Option<String>,
}

impl Engine {
    pub fn new(cfg: GoalsConfig) -> Result<Self> {
        // validate the stop/halt expressions up front so a typo fails at load,
        // not 3 sessions into a run.
        stop::validate(&cfg.stop_when, &cfg.goals)?;
        if let Some(h) = &cfg.halt_when {
            stop::validate(h, &cfg.goals)?;
        }
        Ok(Engine { goals: cfg.goals, stop_when: cfg.stop_when, halt_when: cfg.halt_when })
    }

    /// Run every goal's judge in `cwd`, fold verdicts in, and evaluate conditions
    /// against the current run-state (tokens/budget/wall-time). Computes per-goal
    /// deltas (before vs after) for the summarizer.
    pub fn evaluate_cycle(&mut self, cwd: &Path, run: &RunState) -> CycleResult {
        // snapshot before
        let before: Vec<(f64, Lifecycle)> = self
            .goals
            .iter()
            .map(|g| (g.last_verdict.as_ref().map(|v| v.value).unwrap_or(0.0), g.state))
            .collect();

        for goal in &mut self.goals {
            let verdict = judge::run(&goal.judge, cwd);
            goal.apply(verdict);
        }

        let deltas: Vec<GoalDelta> = self
            .goals
            .iter()
            .zip(before)
            .map(|(g, (bv, bs))| GoalDelta {
                id: g.id.clone(),
                before_value: bv,
                after_value: g.last_verdict.as_ref().map(|v| v.value).unwrap_or(0.0),
                before_state: bs,
                after_state: g.state,
                rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
            })
            .collect();

        self.conditions_with_deltas(run, deltas)
    }

    /// Evaluate stop/halt against the CURRENT goal states (no re-judging, no deltas).
    #[allow(dead_code)]
    pub fn conditions(&self, run: &RunState) -> CycleResult {
        self.conditions_with_deltas(run, vec![])
    }

    fn conditions_with_deltas(&self, run: &RunState, deltas: Vec<GoalDelta>) -> CycleResult {
        let ctx = StopContext {
            goals: &self.goals,
            tokens_spent: run.tokens_spent,
            budget_total: run.budget_total,
            wall_hours: run.wall_hours,
        };
        let stop = stop::evaluate(&self.stop_when, &ctx).unwrap_or(false);
        let (halt, halt_reason) = match &self.halt_when {
            Some(expr) => {
                let h = stop::evaluate(expr, &ctx).unwrap_or(false);
                (h, if h { Some(expr.clone()) } else { None })
            }
            None => (false, None),
        };
        CycleResult { stop, halt, halt_reason, deltas }
    }

    /// Counts for the scoreboard header: (met, total).
    pub fn tally(&self) -> (usize, usize) {
        let met = self.goals.iter().filter(|g| g.met()).count();
        (met, self.goals.len())
    }

    #[allow(dead_code)]
    pub fn any_regressed(&self) -> bool {
        self.goals.iter().any(|g| g.state == Lifecycle::Regressed)
    }

    /// Plain-text scoreboard (Phase 1 output; the TUI replaces this in Phase 4).
    pub fn scoreboard(&self) -> String {
        let (met, total) = self.tally();
        let mut out = format!("Goals: {met}/{total}   stop_when: {}\n", self.stop_when);
        for g in &self.goals {
            out.push_str("  ");
            out.push_str(&g.scoreboard_line());
            if let Some(v) = &g.last_verdict {
                if !v.rationale.is_empty() {
                    out.push_str(&format!("   — {}", v.rationale));
                }
            }
            out.push('\n');
        }
        out
    }
}
