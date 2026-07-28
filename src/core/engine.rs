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

/// The longest `{{reason}}` we will emit. A rationale is worker-influenced (a `blocked` detector
/// echoes a line the worker wrote), and it lands in an operator's terminal, a push notification and
/// a command line — all of which have opinions about length. `core::memory` already caps and
/// control-strips every worker string it ingests; this is the same discipline on the notify path.
const REASON_MAX_CHARS: usize = 400;

/// One safe, single-LINE reason: control characters removed, whitespace collapsed, length capped.
/// Falls back to `fallback` (the expression text) if sanitizing leaves nothing — an empty
/// notification looks delivered and says nothing, which is worse than a terse one.
///
/// Quoting for the shell happens later and separately (`features::notify::shq`); this is about the
/// string being READABLE and honest, not about it being safe to execute.
fn sanitize_reason(text: &str, fallback: &str) -> String {
    // `split_whitespace` already collapses \n and \t; the filter catches the rest (ESC, BEL, and
    // the C1 range — an untrusted string that can repaint or retitle the operator's terminal).
    let cleaned: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let cleaned = if cleaned.is_empty() { fallback.split_whitespace().collect::<Vec<_>>().join(" ") } else { cleaned };
    match cleaned.char_indices().nth(REASON_MAX_CHARS) {
        // truncate on a CHAR boundary — `char_indices` gives one by construction, so this cannot
        // panic on a multi-byte rationale.
        Some((byte, _)) => format!("{}…", &cleaned[..byte]),
        None => cleaned,
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
    /// the NON-TERMINAL signal (STUCK_NOTIFY §8.2): `Some(reason)` = `notify_if` is true THIS step.
    /// It never sets `stop`/`halt` — a stuck loop keeps running; a human is a side-channel, not a
    /// gate. `reason` is the one-line `{{reason}}` string (see [`Engine::notify_reason`]).
    pub notify: Option<String>,
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
    /// the non-terminal notify guard (STUCK_NOTIFY §3). Same grammar, no effect on stop/halt.
    pub notify_if: Option<String>,
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
    pub fn new(
        judges: Vec<Judge>,
        done_if: String,
        abort_if: Option<String>,
        notify_if: Option<String>,
    ) -> Result<Self> {
        stop::validate(&done_if, &judges)?;
        if let Some(a) = &abort_if {
            stop::validate(a, &judges)?;
        }
        if let Some(n) = &notify_if {
            stop::validate(n, &judges)?;
        }
        Ok(Engine { judges, done_if, abort_if, notify_if })
    }

    /// The one-line `{{reason}}` for a `notify_if` that just fired (STUCK_NOTIFY §5 / §12.3): the
    /// rationale of the judge NAMED IN THE EXPRESSION with the highest `value`, so a compound
    /// `stuck.value >= 85 OR blocked` reports whichever detector is actually shouting. Falls back to
    /// the expression text when no named judge has a usable rationale (e.g. the baseline pass, where
    /// only run-scalars like `over_iterations` can be true).
    pub fn notify_reason(&self, expr: &str) -> String {
        let named = stop::judge_names(expr).unwrap_or_default();
        // MET FIRST, then value. Ranking on raw `value` alone is meaningless across judges: a
        // rubric scores 0–100 and a script scores 0–1, so a quiet `stuck` at 10/100 outranks a
        // FIRING `blocked` at 1/1 and the human is paged with the rationale of the detector that is
        // not complaining. `met` is the judge's own verdict, not proof it made the expression true
        // (`stuck.value >= 50` can fire while the rubric's own `met` threshold of 85 is unmet), so
        // it is a heuristic — but it is the right one: among the judges an expression names, the
        // ones reporting trouble are the ones with something to say. The all-judges pass is the
        // fallback for exactly that threshold-mismatch case.
        //
        // `reduce` with a strict `>`, not `max_by`: `Iterator::max_by` returns the LAST of equal
        // maxima, and §12.3 pins ties to the FIRST in run-set order (the order the expression names
        // them). Both are deterministic; matching the spec is free and one less thing to debug.
        let pick = |met_only: bool| {
            self.judges
                .iter()
                .filter(|g| named.iter().any(|n| n == &g.name))
                .filter(|g| !met_only || g.met())
                .filter_map(|g| g.last_verdict.as_ref().map(|v| (v.value.unwrap_or(0.0), v.rationale.trim())))
                .filter(|(_, r)| !r.is_empty())
                .reduce(|best, cur| if cur.0 > best.0 { cur } else { best })
        };
        let text = match pick(true).or_else(|| pick(false)) {
            Some((_, rationale)) => rationale,
            None => expr,
        };
        sanitize_reason(text, expr)
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
        isolation: crate::isolation::Isolation,
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
            let verdicts = self.run_judges(cwd, ruler, judge_model, timeout, session, step, isolation);
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
    #[allow(clippy::too_many_arguments)]
    fn run_judges(
        &self,
        cwd: &Path,
        ruler: &dyn AgentBackend,
        judge_model: &str,
        timeout: u64,
        session: Option<u32>,
        step: &str,
        isolation: crate::isolation::Isolation,
    ) -> Vec<(Verdict, crate::backend::Spend)> {
        self.judges
            .iter()
            .map(|g| judge::run(&g.kind, &g.name, cwd, ruler, judge_model, timeout, session, step, isolation))
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
        // Evaluated on the SAME judge snapshot as done/abort so a cycle cannot report an
        // inconsistent trio. NON-TERMINAL by construction: `notify` is a separate field, and no
        // branch below folds it into `stop`/`halt`.
        let notify = match &self.notify_if {
            Some(expr) if eval_or_log(expr, &ctx, "notify_if") => Some(self.notify_reason(expr)),
            _ => None,
        };
        CycleResult {
            stop,
            halt,
            halt_reason,
            notify,
            deltas,
            fresh_verdicts: Vec::new(),
            judge_tokens: 0,
            judge_cost: None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Judge, JudgeKind, Lifecycle, Verdict};

    /// A judge that has never been graded — the state `Engine::new` validates against, and the
    /// state every detector is in on the baseline pass.
    fn unjudged(name: &str, in_dod: bool) -> Judge {
        Judge {
            name: name.into(),
            kind: JudgeKind::Script { path: "true".into() },
            invariant: false,
            in_dod,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    /// A graded DoD-set goal. Passing `met: false` is what keeps the non-terminality assertions
    /// honest: a vacuously-satisfied `done_if` would set `stop` for reasons that have nothing to do
    /// with notify, and the test would pass while proving nothing.
    fn goal(name: &str, met: bool) -> Judge {
        let mut j = unjudged(name, true);
        j.apply(Verdict {
            met,
            value: Some(if met { 1.0 } else { 0.0 }),
            max: Some(1.0),
            target: 1.0,
            rationale: String::new(),
            evidence: vec![],
            error: None,
        });
        j
    }

    /// A stuck-detector: run-set only (`in_dod: false` — machinery, never a goal, §12.1), carrying
    /// the `value` the expression compares and the `rationale` that becomes `{{reason}}`.
    fn detector(name: &str, value: f64, rationale: &str) -> Judge {
        let mut j = unjudged(name, false);
        j.apply(Verdict {
            met: value > 0.0,
            value: Some(value),
            max: Some(100.0),
            target: 100.0,
            rationale: rationale.into(),
            evidence: vec![],
            error: None,
        });
        j
    }

    /// The engine under test: an unmet DoD plus a guard that cannot trip on a default `RunState`,
    /// so `stop`/`halt` are false unless the thing under test moves them.
    fn engine(judges: Vec<Judge>, notify_if: Option<&str>) -> Engine {
        Engine::new(judges, "all_goals".into(), Some("over_iterations".into()), notify_if.map(String::from))
            .expect("the test engine's expressions must validate")
    }

    /// One cycle's verdict without running a single judge (no subprocess, no ruler) — the same
    /// entry point the rollback gate uses to re-derive a cycle from kept judge state.
    fn cycle(eng: &Engine) -> CycleResult {
        eng.conditions_only(&RunState::default())
    }

    /// THE feature, in one assertion (§2.1/§8.2): a fired `notify_if` is PURE SIGNAL. If this
    /// regresses, agg has silently become the thing it exists to avoid — a loop that halts to ask a
    /// human. The reason travels on its own field; `stop`/`halt` do not move.
    #[test]
    fn a_fired_notify_if_is_non_terminal_the_loop_keeps_running() {
        let eng = engine(
            vec![goal("feature", false), detector("stuck", 92.0, "no judge moved in 5 sessions")],
            Some("stuck.value >= 85"),
        );
        let res = cycle(&eng);
        assert_eq!(res.notify.as_deref(), Some("no judge moved in 5 sessions"), "the detector's rationale is the reason");
        assert!(!res.stop, "notify is not success — it must never satisfy the Definition of Done");
        assert!(!res.halt, "notify is not abort — the loop KEEPS RUNNING while a human is pinged");
        assert!(res.halt_reason.is_none(), "nothing halted, so there is no halt reason to report");

        // the same detector below its threshold pings nobody.
        let quiet = engine(
            vec![goal("feature", false), detector("stuck", 40.0, "still moving")],
            Some("stuck.value >= 85"),
        );
        assert!(cycle(&quiet).notify.is_none(), "notify_if false ⇒ no signal at all");
    }

    /// §5/§12.3: under a compound guard, `{{reason}}` is the rationale of whichever NAMED detector
    /// is shouting loudest — highest `value`. Not the first one listed, and not the loudest judge
    /// in the run-set, or a `stuck.value >= 85 OR blocked` ladder would report the wrong stage.
    #[test]
    fn the_reason_comes_from_the_highest_value_judge_named_in_the_expression() {
        let expr = "stuck.value >= 85 OR blocked";

        // `stuck` (92) outranks `blocked` (1) even though `blocked` comes first in the run-set…
        let eng = engine(
            vec![
                goal("feature", false),
                detector("blocked", 1.0, "waiting on a prod credential"),
                detector("stuck", 92.0, "values flat for 5 sessions"),
                // …and a LOUDER judge the expression does not name must not supply the reason.
                detector("coverage", 100.0, "coverage is fine"),
            ],
            Some(expr),
        );
        assert_eq!(cycle(&eng).notify.as_deref(), Some("values flat for 5 sessions"));

        // the ranking is by VALUE, not position: flip the numbers, keep the order, get the other one.
        let flipped = engine(
            vec![
                goal("feature", false),
                detector("blocked", 99.0, "waiting on a prod credential"),
                detector("stuck", 90.0, "values flat for 5 sessions"),
            ],
            Some(expr),
        );
        assert_eq!(cycle(&flipped).notify.as_deref(), Some("waiting on a prod credential"));
    }

    /// §12.3 step 4 — the reason is never empty, so a detector with nothing to say falls back to
    /// the expression text: "why did I get paged" must always be answerable. Two shapes reach it —
    /// a judge that ran and said nothing, and a judge that has not run yet (the baseline pass).
    #[test]
    fn a_judge_with_no_usable_rationale_falls_back_to_the_expression_text() {
        let silent = engine(
            vec![goal("feature", false), detector("stuck", 92.0, "   ")],
            Some("stuck.value >= 85"),
        );
        assert_eq!(cycle(&silent).notify.as_deref(), Some("stuck.value >= 85"), "whitespace is not a rationale");

        // never graded: `NOT stuck` still fires (an unjudged judge is not met) and there is no
        // verdict to read a rationale from.
        let fresh = engine(vec![goal("feature", false), unjudged("stuck", false)], Some("NOT stuck"));
        assert_eq!(cycle(&fresh).notify.as_deref(), Some("NOT stuck"));
    }

    /// §12.3: the reason lands in LINE-ORIENTED sinks (ntfy, a `>> log` tail, a push notification)
    /// and a rationale is free-form LLM prose that routinely wraps. Every interior break is
    /// collapsed before it can truncate the message or corrupt the sink.
    #[test]
    fn the_reason_is_always_a_single_line() {
        let rationale = "goal values flat since session 3.\nThe diff churns over the same\r\n  two files.\n";
        let eng = engine(vec![goal("feature", false), detector("stuck", 90.0, rationale)], Some("stuck.value >= 85"));
        let reason = cycle(&eng).notify.expect("notify_if fired");
        assert!(!reason.contains('\n'), "no newline may survive: {reason:?}");
        assert!(!reason.contains('\r'), "not a CR either — CRLF prose is the common case: {reason:?}");
        assert_eq!(reason, "goal values flat since session 3. The diff churns over the same two files.");
    }

    /// §8.1/§12.1: a `notify_if` naming a judge outside the run-set is a STARTUP error, not a
    /// surprise three sessions into an overnight run — the exact treatment `abort_if` already gets.
    /// (In production `assemble` puts `notify_if`'s judges INTO the run-set; this is the guard for
    /// when a name still fails to resolve.)
    #[test]
    fn engine_new_refuses_a_notify_if_naming_a_judge_outside_the_run_set() {
        let run_set = || vec![goal("feature", false)];

        // `.map(|_| ())` only so the failure path is printable — `Engine` is not `Debug`.
        let err = format!(
            "{:#}",
            Engine::new(run_set(), "all_goals".into(), None, Some("stuck.value >= 85".into()))
                .map(|_| ())
                .expect_err("an unresolvable notify_if must not build an Engine")
        );
        assert!(err.contains("unknown judge `stuck`"), "the error must name the judge that is missing: {err}");
        assert!(err.contains("stuck.value >= 85"), "…and quote the offending expression back: {err}");

        // identical treatment for the terminal twin — this is a mirror, not a new policy.
        assert!(Engine::new(run_set(), "all_goals".into(), Some("stuck".into()), None).is_err());

        // …and the very same expression builds clean once the detector IS in the run-set.
        let mut with_detector = run_set();
        with_detector.push(unjudged("stuck", false));
        assert!(Engine::new(with_detector, "all_goals".into(), None, Some("stuck.value >= 85".into())).is_ok());
    }

    /// §12.7 row 4 / §12.8: with no `notify_if`, `CycleResult.notify` is `None` on EVERY cycle —
    /// including the one that succeeds and the one that halts. The field means exactly one thing,
    /// "notify_if fired this step"; success is not a cry for help ("ping me when the run ends" is
    /// spelled `hooks.on_stop`), and the terminal halt ping (§8.5) is keyed off `halt` by the
    /// delivery handler, not smuggled into this field.
    #[test]
    fn without_notify_if_no_cycle_ever_notifies() {
        let running = engine(vec![goal("feature", false), detector("stuck", 99.0, "hopelessly stuck")], None);
        let res = cycle(&running);
        assert!(res.notify.is_none(), "a detector pinned at 99 is silent when no notify_if reads it");
        assert!(!res.stop && !res.halt);

        let succeeded = engine(vec![goal("feature", true)], None);
        let res = cycle(&succeeded);
        assert!(res.stop, "the DoD is met");
        assert!(res.notify.is_none(), "reaching the DoD is not a notification");

        let over = RunState { sessions_done: 3, max_sessions: Some(3), ..RunState::default() };
        let res = engine(vec![goal("feature", false)], None).conditions_only(&over);
        assert!(res.halt, "the abort guard tripped");
        assert!(res.notify.is_none(), "an abort_if halt does not set `notify` — that is the handler's job");
    }

    /// §12.3's tie-break, which `Iterator::max_by` gets backwards (it returns the LAST of equal
    /// maxima). Two detectors, same `value`, both with a rationale: the FIRST in run-set order wins.
    /// Run-set order is the order the expression names them, so this is the tie-break a reader of
    /// `blocked OR escalated` would predict.
    #[test]
    fn a_tie_on_value_goes_to_the_first_judge_in_run_set_order() {
        let eng = engine(
            vec![
                goal("feature", false),
                detector("blocked", 1.0, "first — a human must unblock this"),
                detector("escalated", 1.0, "second — should lose the tie"),
            ],
            Some("blocked OR escalated"),
        );
        assert_eq!(cycle(&eng).notify.as_deref(), Some("first — a human must unblock this"));

        // …and it really is ORDER, not the name: flip the run-set and the other one wins.
        let flipped = engine(
            vec![
                goal("feature", false),
                detector("escalated", 1.0, "second — should lose the tie"),
                detector("blocked", 1.0, "first — a human must unblock this"),
            ],
            Some("blocked OR escalated"),
        );
        assert_eq!(cycle(&flipped).notify.as_deref(), Some("second — should lose the tie"));
    }

    /// §12.10b — the halt ping enriches the bare `abort_if` expression with the winning judge's
    /// rationale, so "stop + notify" delivers the blocker rather than a config line. A ceiling-only
    /// expression names no judge and must come back UNCHANGED (that is §8.5's contract, and the e2e
    /// asserts the exact `over_iterations` line).
    #[test]
    fn the_halt_reason_carries_the_blocker_but_a_ceiling_stays_bare() {
        let eng = engine(
            vec![goal("feature", false), detector("blocked", 1.0, "BLOCKED: need the prod deploy key")],
            None,
        );
        assert_eq!(
            eng.notify_reason("blocked OR over_iterations"),
            "BLOCKED: need the prod deploy key",
            "a named judge with a rationale supplies the detail the handler appends"
        );
        assert_eq!(
            eng.notify_reason("over_budget OR wall_hours >= 8"),
            "over_budget OR wall_hours >= 8",
            "a ceiling-only expression names no judge — echo it back so the handler leaves it bare"
        );
    }
}
