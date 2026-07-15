//! The cycle engine: run all judges, fold verdicts into goals, evaluate stop/halt.
//!
//! This is the goal logic the loop calls once per cycle (after a worker exits).
//! `agg plan` exercises this engine for a single dry-run cycle.

use crate::backend::AgentBackend;
use crate::core::config::GoalsConfig;
use crate::core::judge;
use crate::core::model::{Goal, Lifecycle, RecheckPolicy, Verdict};
use crate::core::stop::{self, StopContext};
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
///
/// A judge that ERRORED records NOTHING — it must be retried next cycle. Stamping its signature
/// would skip it until its inputs happen to change, and the two halves of the safety argument do
/// not cover that gap: `judge.value` on an errored verdict is `Missing` (every comparison false)
/// *forever*, while `any_judge_error` only reports judges that ran THIS step, so it goes false the
/// moment the judge stops running. `done_if` could never be true, `abort_if` would never fire, and
/// the loop would burn to its budget ceiling without ever saying why.
fn update_recheck_state(goal: &mut Goal, cwd: &Path) {
    if goal.last_verdict.as_ref().is_some_and(|v| v.error.is_some()) {
        return;
    }
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
    /// the verdicts that a judge actually PRODUCED this cycle (id + verdict), in goal order —
    /// skipped judges (recheck) are absent. This is what the GATE stamps and appends to
    /// `verdicts.jsonl` (§5.8), and its "produced this cycle" membership is the freshness signal
    /// the gate's regression check needs: a skipped judge can never be read as "now fails".
    pub fresh_verdicts: Vec<(String, Verdict)>,
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

/// Run-level facts the stop/halt expressions can reference (budget #5, dollar-cost #2,
/// iteration cap). Each ceiling exposes one `over_*` predicate to the user (over_budget /
/// over_cost / over_iterations); the raw counters back those predicates.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunState {
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    /// cumulative dollars spent this run (`total_cost_usd` summed across sessions)
    pub cost_spent: f64,
    /// dollar ceiling, if configured (`cost.total`) — backs `over_cost`
    pub cost_limit: Option<f64>,
    /// sessions completed so far this run — backs `over_iterations`
    pub sessions_done: u32,
    /// the `max_sessions` cap, if any (0/None = unlimited) — backs `over_iterations`
    pub max_sessions: Option<u32>,
    pub wall_hours: f64,
}

pub struct Engine {
    pub goals: Vec<Goal>,
    pub stop_when: String,
    pub halt_when: Option<String>,
}

/// A snapshot of one goal's per-cycle runtime state (see [`Engine::snapshot_goal_state`]).
#[derive(Debug, Clone)]
pub struct GoalRuntime {
    pub state: Lifecycle,
    pub last_verdict: Option<crate::core::model::Verdict>,
    pub ever_met: bool,
    pub latched: bool,
    pub recheck_sig: Option<u64>,
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

    /// Run every goal's judge and fold verdicts in, then evaluate conditions against the
    /// current run-state (tokens/budget/wall-time). Computes per-goal deltas for the summarizer.
    ///
    /// `cwd` is the project root: judge scripts run there, `inputs`/`recheck_inputs` resolve
    /// there. `config_base` is the `agg/` folder, where config-adjacent files live: LLM-judge
    /// rubric files resolve against it. `ruler` is the backend the LLM judges call — see
    /// [`judge::run`].
    pub fn evaluate_cycle(
        &mut self,
        cwd: &Path,
        config_base: &Path,
        run: &RunState,
        ruler: &dyn AgentBackend,
    ) -> CycleResult {
        // snapshot before
        let before: Vec<(f64, Lifecycle)> = self
            .goals
            .iter()
            .map(|g| (g.last_verdict.as_ref().and_then(|v| v.value).unwrap_or(0.0), g.state))
            .collect();

        // Judging (the expensive part) is separated from folding the results in (the cheap part)
        // so the former has ONE choke point — see `run_judges`.
        let verdicts = self.run_judges(cwd, config_base, ruler);
        // Judges that RAN this cycle and errored — backs `any_judge_error`. Only the ones that ran:
        // a skipped judge cannot have errored, and reporting a stale error would be a lie.
        let mut judge_errors: Vec<String> = Vec::new();
        // The verdicts produced this cycle, captured BEFORE `apply` consumes them — the GATE
        // stamps and appends these to `verdicts.jsonl`, and their membership is the freshness
        // signal for the regression check (a skipped judge is simply absent).
        let mut fresh: Vec<(String, Verdict)> = Vec::new();
        for (goal, verdict) in self.goals.iter_mut().zip(verdicts) {
            // None = skipped: status can't have changed → keep the last verdict, skip the (maybe
            // expensive) judge. `last_verdict`/`state` are left intact.
            if let Some(v) = verdict {
                if v.error.is_some() {
                    judge_errors.push(goal.id.clone());
                }
                fresh.push((goal.id.clone(), v.clone()));
                goal.apply(v);
                update_recheck_state(goal, cwd);
            }
        }

        // ponytail: an ERRORED judge's delta reads `28 → 0 [Met] judge failed: …`. Its value is now
        // `None` (§5.2), which this `unwrap_or(0.0)` renders as a 0 — a placeholder, not a
        // measurement. The state column is honest (`Goal::apply` no longer moves it) and the
        // rationale says "judge failed", so the line is self-describing and the breakage stays LOUD
        // in memory/summary — what you want when your grader is broken. Making it read `28 → 28`
        // (carry the last real value) is the readers' later rework, not this cut. Nothing downstream
        // branches on it — deltas feed only the prose.
        let deltas: Vec<GoalDelta> = self
            .goals
            .iter()
            .zip(before)
            .map(|(g, (bv, bs))| GoalDelta {
                id: g.id.clone(),
                before_value: bv,
                after_value: g.last_verdict.as_ref().and_then(|v| v.value).unwrap_or(0.0),
                before_state: bs,
                after_state: g.state,
                rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
            })
            .collect();

        let mut result = self.conditions_with_deltas(run, deltas, &judge_errors);
        result.fresh_verdicts = fresh;
        result
    }

    /// Run the judge for every goal that needs one, and return the verdicts POSITIONALLY —
    /// `verdicts[i]` belongs to `goals[i]`, and `None` means "skipped, keep the last verdict".
    ///
    /// # seam
    /// This is the single choke point through which every judge in a cycle runs, and it is the
    /// only reason it exists as its own fn: ROADMAP #8 (judge parallelism + result caching) lands
    /// INSIDE this function and nowhere else. That is why it takes `&self` rather than `&mut self`
    /// — judging is a pure read of goal state, so the calls are independently spawnable; the
    /// mutation (`apply` + `update_recheck_state`) stays in the caller, sequential and in goal
    /// order, so parallelizing this cannot reorder state updates or change what the loop records.
    ///
    /// Today it is a plain sequential map. That is deliberate: the seam is the deliverable, not
    /// the parallelism.
    fn run_judges(&self, cwd: &Path, config_base: &Path, ruler: &dyn AgentBackend) -> Vec<Option<Verdict>> {
        self.goals
            .iter()
            .map(|goal| {
                if goal_should_skip_judge(goal, cwd) {
                    None
                } else {
                    Some(judge::run(&goal.judge, cwd, config_base, ruler))
                }
            })
            .collect()
    }

    /// Snapshot every goal's per-cycle RUNTIME state (everything `apply`/`update_recheck_state`
    /// mutate). Paired with [`restore_goal_state`] to undo a cycle's judging — used by the
    /// rollback gate: when a staged merge is discarded, the engine must be reset to base truth so
    /// it never reports success on discarded work, never poisons memory with phantom deltas, and
    /// never spuriously rolls back the NEXT session (W4/W5). Captured in goal order.
    pub fn snapshot_goal_state(&self) -> Vec<GoalRuntime> {
        self.goals
            .iter()
            .map(|g| GoalRuntime {
                state: g.state,
                last_verdict: g.last_verdict.clone(),
                ever_met: g.ever_met,
                latched: g.latched,
                recheck_sig: g.recheck_sig,
            })
            .collect()
    }

    /// Restore a snapshot taken by [`snapshot_goal_state`]. No-op if the shape doesn't match
    /// (goal count changed — impossible within a run, but defensive).
    pub fn restore_goal_state(&mut self, snap: &[GoalRuntime]) {
        if snap.len() != self.goals.len() {
            return;
        }
        for (g, s) in self.goals.iter_mut().zip(snap) {
            g.state = s.state;
            g.last_verdict = s.last_verdict.clone();
            g.ever_met = s.ever_met;
            g.latched = s.latched;
            g.recheck_sig = s.recheck_sig;
        }
    }

    /// Re-evaluate stop/halt against the CURRENT goal state without running any judges (no LLM
    /// cost). Used after a rollback restores base truth, so `res.stop`/`res.halt` reflect what
    /// actually landed on base — not the discarded merge. Returns empty deltas.
    pub fn conditions_only(&self, run: &RunState) -> CycleResult {
        // no judge ran here (that is the whole point) → `any_judge_error` is honestly false.
        self.conditions_with_deltas(run, Vec::new(), &[])
    }

    fn conditions_with_deltas(
        &self,
        run: &RunState,
        deltas: Vec<GoalDelta>,
        judge_errors: &[String],
    ) -> CycleResult {
        let ctx = StopContext {
            goals: &self.goals,
            judge_errors,
            tokens_spent: run.tokens_spent,
            budget_total: run.budget_total,
            cost_spent: run.cost_spent,
            cost_limit: run.cost_limit,
            sessions_done: run.sessions_done,
            max_sessions: run.max_sessions,
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
        // fresh_verdicts is filled by `evaluate_cycle` (which has the verdicts); a
        // conditions-only re-evaluation ran no judges, so it is empty here.
        CycleResult { stop, halt, halt_reason, deltas, fresh_verdicts: Vec::new() }
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
    use crate::core::config::GoalsConfig;
    use crate::core::model::{Goal, GoalType, JudgeSpec, RecheckPolicy};

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("agg-engine-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every goal below uses a SCRIPT judge, which never calls the ruler — but the engine takes it
    /// explicitly now, so there is no ambient agent left to fall back on.
    fn ruler() -> &'static dyn AgentBackend {
        crate::backend::for_name("claude").unwrap()
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
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());  // cycle 1: judges (count=1), latches
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());  // cycle 2: skipped
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());  // cycle 3: skipped
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
        for _ in 0..3 { eng.evaluate_cycle(&dir, &dir, &rs, ruler()); }
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
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());                 // cycle 1: judges (count=1), records sig
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());                 // cycle 2: input unchanged → skip
        assert_eq!(run_count(&dir, "artifact_ok"), 1, "unchanged input → no re-judge");
        std::fs::write(&watched, "v2-changed").unwrap();
        eng.evaluate_cycle(&dir, &dir, &rs, ruler());                 // cycle 3: input changed → re-judge
        assert_eq!(run_count(&dir, "artifact_ok"), 2, "changed input → re-judge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A script judge that counts its invocations and then CRASHES (emits no verdict JSON).
    fn crashing_goal(id: &str, dir: &std::path::Path, inputs: Vec<String>) -> Goal {
        let counter = dir.join(format!("{id}.count"));
        let cmd = format!("printf x >> {c}; echo boom >&2; exit 1", c = counter.display());
        Goal {
            id: id.into(), goal_type: GoalType::Binary,
            judge: JudgeSpec::Script { cmd, timeout: 10 },
            target: 1.0, weight: 1.0, invariant: false, description: String::new(),
            recheck: RecheckPolicy::OnChange, recheck_inputs: inputs,
            state: Lifecycle::Pending, last_verdict: None, ever_met: false,
            latched: false, recheck_sig: None,
        }
    }

    #[test]
    fn on_change_retries_a_crashed_judge() {
        // A judge that BLEW UP must not stamp its signature. If it did, it would be skipped until
        // its inputs happened to change — and then `coverage.value` stays Missing (every comparison
        // false) forever, while `any_judge_error` is per-step and goes false as soon as the judge
        // stops running. done_if could never be true, abort_if would never fire, and the loop would
        // burn to the budget ceiling with nothing saying why.
        let dir = tmpdir("crashed");
        std::fs::write(dir.join("artifact.txt"), "v1").unwrap();
        let g = crashing_goal("coverage", &dir, vec!["artifact.txt".into()]);
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![g], stop_when: "coverage".into(), halt_when: None,
        }).unwrap();
        let rs = RunState::default();
        eng.evaluate_cycle(&dir, &dir, &rs, ruler()); // cycle 1: the judge crashes
        assert!(eng.goals[0].last_verdict.as_ref().unwrap().error.is_some(), "judge should have errored");
        assert!(eng.goals[0].recheck_sig.is_none(), "a crashed judge must not stamp its signature");
        eng.evaluate_cycle(&dir, &dir, &rs, ruler()); // cycle 2: input UNCHANGED → must retry anyway
        assert_eq!(run_count(&dir, "coverage"), 2, "a crashed judge must be retried, not skipped forever");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- a broken judge is not a failing judge ----------------

    /// An INVARIANT goal whose script judge reads its behaviour from a `mode` file, so one engine
    /// can be walked from "judge works, goal is met" into each of the three ways a judge can be
    /// BROKEN rather than merely not-met.
    fn moded_invariant(id: &str, dir: &std::path::Path) -> Goal {
        let mode = dir.join("mode");
        let cmd = format!(
            r#"case "$(cat {m} 2>/dev/null || echo ok)" in
                 crash)   echo 'boom' >&2; exit 1 ;;
                 garbage) echo 'compiling... 47 warnings emitted'; exit 0 ;;
                 timeout) sleep 30 ;;
                 *)       echo '{{"met":true,"value":1,"max":1,"target":1,"rationale":"build ok"}}' ;;
               esac"#,
            m = mode.display()
        );
        Goal {
            id: id.into(), goal_type: GoalType::Binary,
            // 1s so the `timeout` mode is a fast test — proc.rs group-kills the sleep.
            judge: JudgeSpec::Script { cmd, timeout: 1 },
            target: 1.0, weight: 1.0, invariant: true, description: String::new(),
            recheck: RecheckPolicy::Always, recheck_inputs: vec![],
            state: Lifecycle::Pending, last_verdict: None, ever_met: false,
            latched: false, recheck_sig: None,
        }
    }

    /// A judge that is honestly, cleanly NOT met — so `all_goals` stays false and the run has a
    /// reason to keep going. (Without it, `stop_when` would be satisfied and the halt question moot.)
    fn never_met(id: &str) -> Goal {
        Goal {
            id: id.into(), goal_type: GoalType::Binary,
            judge: JudgeSpec::Script {
                cmd: r#"echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'"#.into(),
                timeout: 10,
            },
            target: 1.0, weight: 1.0, invariant: false, description: String::new(),
            recheck: RecheckPolicy::Always, recheck_inputs: vec![],
            state: Lifecycle::Pending, last_verdict: None, ever_met: false,
            latched: false, recheck_sig: None,
        }
    }

    /// THE SHIPPED BUG, at the level that decides the run's fate. `Verdict::failed` carries
    /// `met: false`; `Goal::apply` used to fold that into `Lifecycle::Regressed` for a previously-met
    /// goal, which `any_regressed(invariants)` — the regression term of the `halt_when` `agg init`
    /// writes (`init.rs`) — turns into a HALT. So a judge that merely *crashed* killed the run,
    /// wearing a regression's clothes. Run each way a judge can break, through the real judge runner.
    fn broken_judge_does_not_halt(tag: &str, mode: &str) {
        let dir = tmpdir(tag);
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![moded_invariant("build_ok", &dir), never_met("feature")],
            stop_when: "all_goals".into(),
            // the regression term of the halt_when `agg init` ships, verbatim.
            halt_when: Some("any_regressed(invariants)".into()),
        })
        .unwrap();
        let rs = RunState::default();

        // cycle 1: the judge works and the invariant is MET.
        let first = eng.evaluate_cycle(&dir, &dir, &rs, ruler());
        assert!(eng.goals[0].met(), "{mode}: cycle 1 must leave the invariant met");
        assert!(!first.halt, "{mode}: nothing to halt on yet");

        // cycle 2: the judge BREAKS. The code under it did not change.
        std::fs::write(dir.join("mode"), mode).unwrap();
        let res = eng.evaluate_cycle(&dir, &dir, &rs, ruler());

        let v = eng.goals[0].last_verdict.as_ref().unwrap();
        assert!(v.error.is_some(), "{mode}: the judge must report an ERROR, not a clean not-met");
        assert_eq!(eng.goals[0].state, Lifecycle::Met, "{mode}: a met goal STAYS met when its judge breaks");
        assert!(!eng.goals[0].regressed(), "{mode}: a broken judge is not a regression");
        assert!(!res.halt, "{mode}: THE BUG — the run must NOT halt blaming a regression that never happened");
        assert!(!res.stop, "{mode}: `feature` is honestly not met, so the run is not done either");

        // ...and the breakage is not swallowed: it comes out the front door instead.
        let errs = ["build_ok".to_string()];
        let ctx = StopContext { judge_errors: &errs, ..StopContext::from_goals(&eng.goals) };
        assert!(stop::evaluate("any_judge_error", &ctx).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crashed_judge_does_not_halt_the_run() {
        broken_judge_does_not_halt("brk-crash", "crash");
    }

    #[test]
    fn a_timed_out_judge_does_not_halt_the_run() {
        broken_judge_does_not_halt("brk-timeout", "timeout");
    }

    #[test]
    fn a_judge_emitting_garbage_does_not_halt_the_run() {
        broken_judge_does_not_halt("brk-garbage", "garbage");
    }

    #[test]
    fn a_broken_judge_raises_any_judge_error_from_the_real_cycle() {
        // The `judge_errors` set the cycle hands to the evaluator must carry REAL data — the ids of
        // the judges that RAN and errored. Asserted through the halt expression, which is the only
        // thing that consumes it.
        let dir = tmpdir("brk-anyerr");
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![moded_invariant("build_ok", &dir), never_met("feature")],
            stop_when: "all_goals".into(),
            // the abort_if this fix makes usable: the EXPLICIT, front-door policy for a broken judge.
            halt_when: Some("any_regressed(invariants) OR any_judge_error".into()),
        })
        .unwrap();
        let rs = RunState::default();
        let first = eng.evaluate_cycle(&dir, &dir, &rs, ruler());
        assert!(!first.halt, "no judge errored in cycle 1");

        std::fs::write(dir.join("mode"), "crash").unwrap();
        let res = eng.evaluate_cycle(&dir, &dir, &rs, ruler());
        assert!(res.halt, "a broken judge must halt through `any_judge_error` — the front door");
        assert!(res.halt_reason.unwrap().contains("any_judge_error"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_judge_on_a_never_met_goal_stays_pending() {
        // The other half: an error must not invent progress. A goal that never got past Pending is
        // Pending, not Regressed — and `any_regressed(invariants)` stays false, so no halt.
        let dir = tmpdir("brk-pending");
        std::fs::write(dir.join("mode"), "crash").unwrap();
        let mut eng = Engine::new(GoalsConfig {
            goals: vec![moded_invariant("build_ok", &dir), never_met("feature")],
            stop_when: "all_goals".into(),
            halt_when: Some("any_regressed(invariants)".into()),
        })
        .unwrap();
        let rs = RunState::default();
        let res = eng.evaluate_cycle(&dir, &dir, &rs, ruler());
        assert!(eng.goals[0].last_verdict.as_ref().unwrap().error.is_some());
        assert_eq!(eng.goals[0].state, Lifecycle::Pending, "never met → stays Pending, never Regressed");
        assert!(!eng.goals[0].ever_met);
        assert!(!res.halt);
        assert!(!res.stop);
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
