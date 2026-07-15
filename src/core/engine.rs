//! The step engine: run the run-set judges, fold verdicts into the judges, evaluate done/abort.
//!
//! This is the judge logic the loop calls once per STEP (after a worker session exits). `agg plan`
//! exercises it for a single dry-run step. There is NO recheck caching — every run-set judge runs
//! every step (§5.5's ponytail note); `skip_judges` is the only lever, and the caching upgrade path
//! is ROADMAP #8, landing INSIDE [`Engine::run_judges`] and nowhere else.

use crate::backend::AgentBackend;
use crate::core::judge;
use crate::core::model::{Judge, Lifecycle, Verdict};
use crate::core::stop::{self, StopContext};
use anyhow::Result;
use std::path::Path;

/// Evaluate a done/abort expression, treating an evaluation error as "not satisfied" but logging it
/// once so a loop that never stops (or never aborts) is diagnosable.
fn eval_or_log(expr: &str, ctx: &StopContext, which: &str) -> bool {
    match stop::evaluate(expr, ctx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("  ⚠ {which} `{expr}` failed to evaluate this step ({e}) — treating as false");
            false
        }
    }
}

/// Outcome of evaluating the run after a step.
#[derive(Debug, Default)]
pub struct CycleResult {
    /// the Definition of Done fired (success — exit 0).
    pub stop: bool,
    /// the abort guard fired (giving up — exit 3).
    pub halt: bool,
    pub halt_reason: Option<String>,
    /// per-judge changes this step (feeds the LLM summarizer).
    pub deltas: Vec<GoalDelta>,
    /// the verdicts a judge actually PRODUCED this step (name + verdict), in judge order. The GATE
    /// stamps and appends these to `verdicts.jsonl` (§5.8); a `skip_judges` step produces none.
    pub fresh_verdicts: Vec<(String, Verdict)>,
    /// ruler tokens the LLM judges spent this step (§5.6) — added to the run ceiling by the loop.
    pub judge_tokens: u64,
    /// ruler dollars the LLM judges spent this step, if the ruler prices itself.
    pub judge_cost: Option<f64>,
}

/// What changed for one judge across a step. The summarizer turns these into prose.
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
    pub fn changed(&self) -> bool {
        self.before_value != self.after_value || self.before_state != self.after_state
    }
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

/// Run-level facts the done/abort expressions reference (budget, dollar cost, iteration cap, wall).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunState {
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    pub cost_spent: f64,
    pub cost_limit: Option<f64>,
    pub sessions_done: u32,
    pub max_sessions: Option<u32>,
    pub wall_hours: f64,
}

pub struct Engine {
    /// the RUN-SET: every judge that can execute this run (done_if ∪ abort_if ∪ invariants ∪ every
    /// if-condition, §5.3). Each carries its `in_dod`/`invariant` membership.
    pub judges: Vec<Judge>,
    /// the Definition of Done (success stop).
    pub done_if: String,
    /// the giving-up guard.
    pub abort_if: Option<String>,
}

/// A snapshot of one judge's per-step runtime state (see [`Engine::snapshot_goal_state`]).
#[derive(Debug, Clone)]
pub struct GoalRuntime {
    pub state: Lifecycle,
    pub last_verdict: Option<Verdict>,
    pub ever_met: bool,
}

impl Engine {
    /// Build from a resolved run-set + the DoD expressions. Validates the expressions up front so a
    /// typo fails at load, not 3 sessions into a run.
    pub fn new(judges: Vec<Judge>, done_if: String, abort_if: Option<String>) -> Result<Self> {
        stop::validate(&done_if, &judges)?;
        if let Some(a) = &abort_if {
            stop::validate(a, &judges)?;
        }
        Ok(Engine { judges, done_if, abort_if })
    }

    /// Run every run-set judge (unless `skip_judges`), fold verdicts in, and evaluate done/abort
    /// against the current run-state.
    ///
    /// On a `skip_judges` step NO judge runs (§5.5): judge state is untouched, `fresh_verdicts` and
    /// `judge_errors` are empty, so `any_judge_error` is honestly false and the DoD terms read their
    /// prior (non-firing) values — only the run-state ceilings can newly trip. `cwd` is the project
    /// root; `ruler`/`judge_model`/`timeout` are the run-level `judge:` block; `session`/`step`
    /// populate the judge env contract.
    #[allow(clippy::too_many_arguments)]
    pub fn run_step(
        &mut self,
        cwd: &Path,
        run: &RunState,
        ruler: &dyn AgentBackend,
        judge_model: &str,
        timeout: u64,
        step: &str,
        session: Option<u32>,
        skip_judges: bool,
    ) -> CycleResult {
        let before: Vec<(f64, Lifecycle)> = self
            .judges
            .iter()
            .map(|g| (g.last_verdict.as_ref().and_then(|v| v.value).unwrap_or(0.0), g.state))
            .collect();

        let mut judge_errors: Vec<String> = Vec::new();
        let mut fresh: Vec<(String, Verdict)> = Vec::new();
        let mut judge_tokens = 0u64;
        let mut judge_cost: Option<f64> = None;
        if !skip_judges {
            let verdicts = self.run_judges(cwd, ruler, judge_model, timeout, session, step);
            for (judge, (v, spend)) in self.judges.iter_mut().zip(verdicts) {
                judge_tokens += spend.tokens;
                if let Some(c) = spend.cost_usd {
                    judge_cost = Some(judge_cost.unwrap_or(0.0) + c);
                }
                if v.error.is_some() {
                    judge_errors.push(judge.name.clone());
                }
                fresh.push((judge.name.clone(), v.clone()));
                judge.apply(v);
            }
        }

        let deltas: Vec<GoalDelta> = self
            .judges
            .iter()
            .zip(before)
            .map(|(g, (bv, bs))| GoalDelta {
                id: g.name.clone(),
                before_value: bv,
                after_value: g.last_verdict.as_ref().and_then(|v| v.value).unwrap_or(0.0),
                before_state: bs,
                after_state: g.state,
                rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
            })
            .collect();

        let mut result = self.conditions_with_deltas(run, deltas, &judge_errors);
        result.fresh_verdicts = fresh;
        result.judge_tokens = judge_tokens;
        result.judge_cost = judge_cost;
        result
    }

    /// Run every run-set judge and return their verdicts POSITIONALLY (`verdicts[i]` belongs to
    /// `judges[i]`).
    ///
    /// # seam
    /// The single choke point through which every judge in a step runs: ROADMAP #8 (judge
    /// parallelism + result caching) lands INSIDE this function and nowhere else. `&self` (not
    /// `&mut`) because judging is a pure read of judge state — the mutation stays in the caller,
    /// sequential and in order. Today it is a plain sequential map; the seam is the deliverable.
    fn run_judges(
        &self,
        cwd: &Path,
        ruler: &dyn AgentBackend,
        judge_model: &str,
        timeout: u64,
        session: Option<u32>,
        step: &str,
    ) -> Vec<(Verdict, crate::backend::Spend)> {
        self.judges
            .iter()
            .map(|g| judge::run(&g.kind, &g.name, cwd, ruler, judge_model, timeout, session, step))
            .collect()
    }

    /// Snapshot every judge's per-step RUNTIME state (everything `apply` mutates). Paired with
    /// [`restore_goal_state`] to undo a step's judging when the rollback gate discards a staged merge
    /// (§5.7 / W5) — so the engine never reports success on discarded work.
    pub fn snapshot_goal_state(&self) -> Vec<GoalRuntime> {
        self.judges
            .iter()
            .map(|g| GoalRuntime { state: g.state, last_verdict: g.last_verdict.clone(), ever_met: g.ever_met })
            .collect()
    }

    /// Restore a snapshot taken by [`snapshot_goal_state`]. No-op if the shape doesn't match.
    pub fn restore_goal_state(&mut self, snap: &[GoalRuntime]) {
        if snap.len() != self.judges.len() {
            return;
        }
        for (g, s) in self.judges.iter_mut().zip(snap) {
            g.state = s.state;
            g.last_verdict = s.last_verdict.clone();
            g.ever_met = s.ever_met;
        }
    }

    /// Re-evaluate done/abort against the CURRENT judge state without running any judges (no LLM
    /// cost). Used after a rollback restores base truth. No judge ran ⇒ `any_judge_error` is false.
    pub fn conditions_only(&self, run: &RunState) -> CycleResult {
        self.conditions_with_deltas(run, Vec::new(), &[])
    }

    fn conditions_with_deltas(
        &self,
        run: &RunState,
        deltas: Vec<GoalDelta>,
        judge_errors: &[String],
    ) -> CycleResult {
        let ctx = StopContext {
            judges: &self.judges,
            judge_errors,
            tokens_spent: run.tokens_spent,
            budget_total: run.budget_total,
            cost_spent: run.cost_spent,
            cost_limit: run.cost_limit,
            sessions_done: run.sessions_done,
            max_sessions: run.max_sessions,
            wall_hours: run.wall_hours,
        };
        let stop = eval_or_log(&self.done_if, &ctx, "done_if");
        let (halt, halt_reason) = match &self.abort_if {
            Some(expr) => {
                let h = eval_or_log(expr, &ctx, "abort_if");
                (h, if h { Some(expr.clone()) } else { None })
            }
            None => (false, None),
        };
        CycleResult { stop, halt, halt_reason, deltas, fresh_verdicts: Vec::new(), judge_tokens: 0, judge_cost: None }
    }

    /// Counts for the scoreboard header: (met, total) over the DoD-set — a run-set-only judge like
    /// `stalled` is machinery, not a goal, so it is not counted.
    pub fn tally(&self) -> (usize, usize) {
        let met = self.judges.iter().filter(|g| g.in_dod && g.met()).count();
        let total = self.judges.iter().filter(|g| g.in_dod).count();
        (met, total)
    }

    /// Plain-text scoreboard.
    pub fn scoreboard(&self) -> String {
        let (met, total) = self.tally();
        let mut out = format!("Goals: {met}/{total}   done_if: {}\n", self.done_if);
        for g in &self.judges {
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
