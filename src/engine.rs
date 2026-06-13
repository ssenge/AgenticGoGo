//! The cycle engine: run all judges, fold verdicts into goals, evaluate stop/halt.
//!
//! This is the goal logic the loop calls once per cycle (after a worker exits).
//! `agg plan` exercises this engine for a single dry-run cycle.

use crate::config::GoalsConfig;
use crate::judge;
use crate::model::{Goal, Lifecycle, RecheckPolicy};
use crate::stop::{self, StopContext};
use anyhow::Result;
use std::path::Path;

/// Evaluate a stop/halt expression, treating an evaluation error as "not satisfied" but
/// logging it once so a loop that never stops (or never halts) is diagnosable. Parse-time
/// errors are already caught by `Engine::new`; a runtime Err here is a genuine surprise.
fn eval_or_log(expr: &str, ctx: &StopContext, which: &str) -> bool {
    match stop::evaluate(expr, ctx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("  ⚠ {which} `{expr}` failed to evaluate this cycle ({e}) — treating as false");
            false
        }
    }
}

/// Reject configs that would silently break correctness:
/// - `once_met` on an invariant (a latched invariant can't detect its own regression).
/// - `on_change` with no `recheck_inputs` (nothing to watch → would never re-judge).
pub fn validate_recheck(goals: &[Goal]) -> Result<()> {
    for g in goals {
        if g.invariant && g.recheck == RecheckPolicy::OnceMet {
            anyhow::bail!(
                "goal '{}' is an invariant but uses `recheck: once_met` — invariants must \
                 stay re-checkable so a regression is caught. Use `recheck: always` (default) \
                 or `on_change`.",
                g.id
            );
        }
        if g.recheck == RecheckPolicy::OnChange && g.recheck_inputs.is_empty() {
            anyhow::bail!(
                "goal '{}' uses `recheck: on_change` but has no `recheck_inputs` — add the \
                 file(s) whose change should trigger re-judging.",
                g.id
            );
        }
    }
    Ok(())
}

/// Decide whether this cycle should SKIP re-running a goal's judge, per its recheck policy.
/// `always` → never skip. `once_met` → skip once latched (first met). `on_change` → skip
/// while the declared inputs' content signature is unchanged since the last judging.
fn goal_should_skip_judge(goal: &Goal, cwd: &Path) -> bool {
    match goal.recheck {
        RecheckPolicy::Always => false,
        RecheckPolicy::OnceMet => goal.latched, // set true once first met (below)
        RecheckPolicy::OnChange => {
            // never skip the first judging (no signature yet); afterwards skip iff unchanged.
            match goal.recheck_sig {
                Some(prev) => recheck_signature(goal, cwd) == prev,
                None => false,
            }
        }
    }
}

/// After judging, record recheck state: latch a `once_met` goal that is now met, and stamp
/// the input signature for an `on_change` goal.
fn update_recheck_state(goal: &mut Goal, cwd: &Path) {
    match goal.recheck {
        RecheckPolicy::OnceMet => {
            if goal.state == Lifecycle::Met {
                goal.latched = true;
            }
        }
        RecheckPolicy::OnChange => {
            goal.recheck_sig = Some(recheck_signature(goal, cwd));
        }
        RecheckPolicy::Always => {}
    }
}

/// Content signature of a goal's `recheck_inputs` (file contents hashed). Missing files
/// contribute a stable sentinel, so creating/deleting an input also changes the signature.
fn recheck_signature(goal: &Goal, cwd: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    for pat in &goal.recheck_inputs {
        pat.hash(&mut h);
        match std::fs::read(cwd.join(pat)) {
            Ok(bytes) => bytes.hash(&mut h),
            Err(_) => 0u8.hash(&mut h), // missing → sentinel (stable, but flips on create)
        }
    }
    h.finish()
}

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
        validate_recheck(&cfg.goals)?;
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
            if goal_should_skip_judge(goal, cwd) {
                // status can't have changed → keep the last verdict, skip the (maybe
                // expensive) judge. `last_verdict`/`state` are left intact.
                continue;
            }
            let verdict = judge::run(&goal.judge, cwd);
            goal.apply(verdict);
            update_recheck_state(goal, cwd);
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

    fn conditions_with_deltas(&self, run: &RunState, deltas: Vec<GoalDelta>) -> CycleResult {
        let ctx = StopContext {
            goals: &self.goals,
            tokens_spent: run.tokens_spent,
            budget_total: run.budget_total,
            wall_hours: run.wall_hours,
        };
        // A malformed expression is rejected at config load (Engine::new validates), so an
        // Err here means a runtime surprise (e.g. a judge emitted a non-finite value feeding a
        // comparison). Treat it as "not satisfied" — but LOG it, so a loop that never stops is
        // debuggable instead of silently evaluating to false forever.
        let stop = eval_or_log(&self.stop_when, &ctx, "stop_when");
        let (halt, halt_reason) = match &self.halt_when {
            Some(expr) => {
                let h = eval_or_log(expr, &ctx, "halt_when");
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

    /// Plain-text scoreboard, printed by `agg plan`/`status` and in the loop's stdout log.
    /// (The live TUI dashboard renders the same goal state separately from `state.json`.)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GoalsConfig;
    use crate::model::{Goal, GoalType, JudgeSpec, RecheckPolicy};

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("agg-engine-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A script judge that counts its invocations (appends to `counter`) and always reports met.
    fn counting_goal(id: &str, dir: &std::path::Path, recheck: RecheckPolicy, inputs: Vec<String>) -> Goal {
        let counter = dir.join(format!("{id}.count"));
        let cmd = format!(
            "printf x >> {c}; echo '{{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"ok\"}}'",
            c = counter.display()
        );
        Goal {
            id: id.into(), goal_type: GoalType::Binary,
            judge: JudgeSpec::Script { cmd, timeout: 10 },
            target: 1.0, weight: 1.0, invariant: false, description: String::new(),
            recheck, recheck_inputs: inputs,
            state: Lifecycle::Pending, last_verdict: None, ever_met: false,
            latched: false, recheck_sig: None,
        }
    }

    fn run_count(dir: &std::path::Path, id: &str) -> usize {
        std::fs::read(dir.join(format!("{id}.count"))).map(|b| b.len()).unwrap_or(0)
    }

    #[test]
    fn once_met_latches_and_skips_judge() {
        let dir = tmpdir("oncemet");
        let g = counting_goal("paper", &dir, RecheckPolicy::OnceMet, vec![]);
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "paper".into(), halt_when: None,
        }).unwrap();
        let rs = RunState::default();
        eng.evaluate_cycle(&dir, &rs);  // cycle 1: judges (count=1), latches
        eng.evaluate_cycle(&dir, &rs);  // cycle 2: skipped
        eng.evaluate_cycle(&dir, &rs);  // cycle 3: skipped
        assert_eq!(run_count(&dir, "paper"), 1, "once_met judge must run exactly once then latch");
        assert!(eng.goals[0].latched);
        assert!(eng.goals[0].met()); // still reported met after skipping
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn always_reruns_every_cycle() {
        let dir = tmpdir("always");
        let g = counting_goal("tests", &dir, RecheckPolicy::Always, vec![]);
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "tests".into(), halt_when: None,
        }).unwrap();
        let rs = RunState::default();
        for _ in 0..3 { eng.evaluate_cycle(&dir, &rs); }
        assert_eq!(run_count(&dir, "tests"), 3, "always must re-judge every cycle");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_change_reruns_only_when_input_changes() {
        let dir = tmpdir("onchange");
        let watched = dir.join("artifact.txt");
        std::fs::write(&watched, "v1").unwrap();
        let g = counting_goal("artifact_ok", &dir, RecheckPolicy::OnChange, vec!["artifact.txt".into()]);
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "artifact_ok".into(), halt_when: None,
        }).unwrap();
        let rs = RunState::default();
        eng.evaluate_cycle(&dir, &rs);                 // cycle 1: judges (count=1), records sig
        eng.evaluate_cycle(&dir, &rs);                 // cycle 2: input unchanged → skip
        assert_eq!(run_count(&dir, "artifact_ok"), 1, "unchanged input → no re-judge");
        std::fs::write(&watched, "v2-changed").unwrap();
        eng.evaluate_cycle(&dir, &rs);                 // cycle 3: input changed → re-judge
        assert_eq!(run_count(&dir, "artifact_ok"), 2, "changed input → re-judge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_once_met_on_invariant() {
        let mut g = counting_goal("inv", std::path::Path::new("."), RecheckPolicy::OnceMet, vec![]);
        g.invariant = true;
        let err = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "inv".into(), halt_when: None,
        });
        assert!(err.is_err(), "once_met on an invariant must be rejected");
    }

    #[test]
    fn validate_rejects_on_change_without_inputs() {
        let g = counting_goal("x", std::path::Path::new("."), RecheckPolicy::OnChange, vec![]);
        let err = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "x".into(), halt_when: None,
        });
        assert!(err.is_err(), "on_change with no recheck_inputs must be rejected");
    }
}
