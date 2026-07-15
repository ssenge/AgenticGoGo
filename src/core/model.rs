//! Core data model: goals, verdicts, lifecycle states.
//!
//! A [`Goal`] is declarative (loaded from `goals.yaml`). A [`Verdict`] is what a
//! judge returns each cycle. The goal's [`Lifecycle`] is derived from its verdict
//! history, so we can detect `Regressed` — a goal that was met and is now unmet.

use serde::{Deserialize, Serialize};

/// The three goal kinds (requirement #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalType {
    /// met = yes/no
    Binary,
    /// value 0..100, met when value >= target
    Percentage,
    /// value of max (e.g. 18 of 28), met when value >= target
    Cardinal,
}

/// Derived lifecycle state of a goal across cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// never evaluated, or evaluated with zero progress
    Pending,
    /// evaluated, partial progress, not met
    InProgress,
    /// currently meets target
    Met,
    /// was Met in a prior cycle, now unmet — first-class signal
    Regressed,
}

impl Lifecycle {
    /// A short glyph for the scoreboard.
    pub fn glyph(self) -> &'static str {
        match self {
            Lifecycle::Pending => "·",
            Lifecycle::InProgress => "◑",
            Lifecycle::Met => "✔",
            Lifecycle::Regressed => "⚠",
        }
    }
}

/// Uniform judge output — the judge contract. Every judge (script or LLM) prints
/// exactly this JSON to stdout, regardless of the goal type it evaluates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub met: bool,
    /// numeric measure the judge emitted: a count, a percent, or 0/1. `None` = the judge emitted NO
    /// number (a binary goal says only `met`; a broken judge emits nothing usable). `Some(0.0)` is a
    /// REAL, measured zero and is DISTINCT from absent — `coverage.value == 0` must tell the two
    /// apart (`core::stop`), and `verdicts.jsonl` records the difference faithfully. Was a
    /// `#[serde(default)] f64`, where absent and a real 0 collapsed to the same 0.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// denominator for cardinal/percentage goals. Absent (`None`) for the same reasons as `value`;
    /// was `#[serde(default = "one")]`, where absent was indistinguishable from a real `1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// presentational only (draws the progress bar) — deliberately NOT readable in a stop condition.
    #[serde(default = "one")]
    pub target: f64,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// set when the judge itself failed to run (vs. a clean "not met")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn one() -> f64 {
    1.0
}

impl Verdict {
    /// Construct the verdict for a judge that failed to execute.
    pub fn failed(error: impl Into<String>) -> Self {
        let error = error.into();
        Verdict {
            met: false,
            // a broken judge emitted no number — not a measured 0.
            value: None,
            max: None,
            target: 1.0,
            rationale: format!("judge failed: {error}"),
            evidence: vec![],
            error: Some(error),
        }
    }
}

/// How often a goal's judge should run. Lets a goal whose status CANNOT change once
/// achieved (a written paper, a completed study) skip its (possibly expensive, e.g. LLM)
/// judge on later cycles — instead of re-judging every cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecheckPolicy {
    /// Re-judge every cycle (the default; required for invariants — their status can regress).
    Always,
    /// Re-judge until the goal is first `Met`, then LATCH it: skip the judge forever after,
    /// treating it as met. For terminal deliverables that can't un-happen.
    OnceMet,
    /// Re-judge only when a declared input (see `recheck_inputs`) changes — by content hash.
    /// Cheaper than `always`, but catches the worker editing the artifact again (unlike `once_met`).
    OnChange,
}

fn default_recheck() -> RecheckPolicy {
    RecheckPolicy::Always
}

/// A goal as declared in `goals.yaml`, plus runtime lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    #[serde(rename = "type")]
    pub goal_type: GoalType,
    /// raw judge spec (kind + params) — resolved by the judge runner
    pub judge: JudgeSpec,
    #[serde(default = "one")]
    pub target: f64,
    #[serde(default = "one")]
    pub weight: f64,
    /// must STAY met; a regression can halt the loop
    #[serde(default)]
    pub invariant: bool,
    #[serde(default)]
    pub description: String,
    /// when to re-run this goal's judge (default `always`). See [`RecheckPolicy`].
    #[serde(default = "default_recheck")]
    pub recheck: RecheckPolicy,
    /// for `recheck: on_change`: file globs/paths whose content gates re-judging. Relative to cwd.
    #[serde(default)]
    pub recheck_inputs: Vec<String>,

    // ---- runtime state (not deserialized from goals.yaml) ----
    #[serde(skip, default = "default_lifecycle")]
    pub state: Lifecycle,
    #[serde(skip)]
    pub last_verdict: Option<Verdict>,
    /// has this goal EVER been met during this run? (drives regression detection)
    #[serde(skip)]
    pub ever_met: bool,
    /// `once_met`: set true once first met → judge skipped thereafter. `on_change`: holds the
    /// last-judged input signature (hash) so we re-judge only when it changes.
    #[serde(skip)]
    pub latched: bool,
    #[serde(skip)]
    pub recheck_sig: Option<u64>,
}

fn default_lifecycle() -> Lifecycle {
    Lifecycle::Pending
}

/// The judge specification embedded in a goal. Tagged by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JudgeSpec {
    /// Run a command; its stdout (verdict JSON) is the verdict.
    Script {
        cmd: String,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    /// A cheap, READ-ONLY one-shot model call that scores artifacts against a rubric prompt. Runs
    /// on the RULER — the backend that judges, which is not necessarily the one that works. See
    /// `core::config::AggConfig::ruler_backend` and `backend::AgentBackend::one_shot`.
    Llm {
        /// `None` = the ruler's own cheap-model default, resolved at USE time (`core::judge`).
        /// It was a backend-specific serde default, which is what made goals.yaml — not just
        /// agg.yaml — unparseable without a latched backend. See the `backend` module docs.
        #[serde(default)]
        model: Option<String>,
        rubric: String,
        #[serde(default)]
        inputs: Vec<String>,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
}

fn default_timeout() -> u64 {
    300
}

impl Goal {
    /// Fold a fresh verdict into the goal's lifecycle state. Returns the new state.
    ///
    /// # A BROKEN JUDGE IS NOT A FAILING JUDGE
    /// A verdict carrying `error` (spawn failure, timeout, garbage stdout — every
    /// [`Verdict::failed`] path) is the judge saying *"I could not grade this"*, NOT *"this is not
    /// met"*. It therefore **never changes the lifecycle**: it is never a regression, never satisfies
    /// `stop_when`, and never marks a goal met. We record the verdict — its `rationale` carries the
    /// error text, so the scoreboard and the memory fold show it — and leave `state`/`ever_met`
    /// exactly as they were. `any_judge_error` (see `core::stop`) is the front-door signal, and
    /// `abort_if: … OR any_judge_error` the explicit policy.
    ///
    /// This FIXES a shipped bug. Before it, `met: false` from a `Verdict::failed` fell through to
    /// the `ever_met` branch below, so a crashed judge on a previously-met goal became
    /// `Regressed` → `any_regressed(invariants)` → the default `halt_when` → **the run halted**,
    /// blaming a regression that never happened. (`latched`/`recheck_sig` are guarded separately, in
    /// `engine::update_recheck_state`, which returns early on the same condition.)
    pub fn apply(&mut self, verdict: Verdict) -> Lifecycle {
        if verdict.error.is_some() {
            self.last_verdict = Some(verdict);
            return self.state;
        }
        let met = verdict.met;
        let value = verdict.value;
        self.last_verdict = Some(verdict);
        self.state = if met {
            self.ever_met = true;
            Lifecycle::Met
        } else if self.ever_met {
            // was met before, now not -> regression
            Lifecycle::Regressed
        } else if value.is_some_and(|v| v > 0.0) {
            // a measured value above 0 = partial progress. `None` (a binary/numberless judge) is not
            // progress — it stays Pending.
            Lifecycle::InProgress
        } else {
            Lifecycle::Pending
        };
        self.state
    }

    pub fn met(&self) -> bool {
        self.state == Lifecycle::Met
    }

    pub fn regressed(&self) -> bool {
        self.state == Lifecycle::Regressed
    }

    /// Compact one-line scoreboard representation.
    pub fn scoreboard_line(&self) -> String {
        let measure = match (&self.last_verdict, self.goal_type) {
            (None, _) => "—".to_string(),
            (Some(v), GoalType::Binary) => if v.met { "yes".into() } else { "no".into() },
            (Some(v), GoalType::Percentage) => format!("{:.0}/{:.0}%", v.value.unwrap_or(0.0), self.target),
            (Some(v), GoalType::Cardinal) => format!("{:.0}/{:.0}", v.value.unwrap_or(0.0), v.max.unwrap_or(1.0)),
        };
        format!(
            "{} {:<18} {:<10} {}",
            self.state.glyph(),
            self.id,
            format!("{:?}", self.goal_type).to_lowercase(),
            measure
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cardinal(id: &str, target: f64) -> Goal {
        Goal {
            id: id.into(),
            goal_type: GoalType::Cardinal,
            judge: JudgeSpec::Script { cmd: "true".into(), timeout: 10 },
            target,
            weight: 1.0,
            invariant: false,
            description: String::new(),
            recheck: RecheckPolicy::Always,
            recheck_inputs: vec![],
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
            latched: false,
            recheck_sig: None,
        }
    }

    fn verdict(met: bool, value: f64, max: f64) -> Verdict {
        Verdict { met, value: Some(value), max: Some(max), target: max, rationale: String::new(), evidence: vec![], error: None }
    }

    #[test]
    fn absent_value_is_none_and_a_real_zero_stays_some() {
        // The whole point of `Option<f64>`: `{"met":true}` (a binary judge, no number) MUST be
        // distinguishable from `{"met":true,"value":0}` (a real measured zero). Before this both
        // deserialized to 0.0, which made "the judge emitted no number" unrepresentable.
        let numberless: Verdict = serde_json::from_str(r#"{"met":true}"#).unwrap();
        assert_eq!(numberless.value, None);
        assert_eq!(numberless.max, None);
        let zero: Verdict = serde_json::from_str(r#"{"met":true,"value":0}"#).unwrap();
        assert_eq!(zero.value, Some(0.0));
        // ...and an absent value stays ABSENT on the wire (skip_serializing_if), so verdicts.jsonl
        // records "no number" faithfully instead of inventing a 0.
        let out = serde_json::to_string(&numberless).unwrap();
        assert!(!out.contains("value"), "absent value must not serialize: {out}");
        assert!(!out.contains("max"), "absent max must not serialize: {out}");
        assert!(serde_json::to_string(&zero).unwrap().contains("\"value\":0"));
    }

    #[test]
    fn pending_to_in_progress_to_met() {
        let mut g = cardinal("g", 28.0);
        assert_eq!(g.apply(verdict(false, 0.0, 28.0)), Lifecycle::Pending);
        assert_eq!(g.apply(verdict(false, 18.0, 28.0)), Lifecycle::InProgress);
        assert_eq!(g.apply(verdict(true, 28.0, 28.0)), Lifecycle::Met);
        assert!(g.met());
    }

    #[test]
    fn regression_is_detected() {
        let mut g = cardinal("g", 28.0);
        g.apply(verdict(true, 28.0, 28.0));
        assert_eq!(g.state, Lifecycle::Met);
        // a later cycle says not-met -> regressed, not just in_progress
        assert_eq!(g.apply(verdict(false, 27.0, 28.0)), Lifecycle::Regressed);
        assert!(g.regressed());
    }

    #[test]
    fn a_broken_judge_is_not_a_regression() {
        // THE BUG: `Verdict::failed` has `met: false`, so a crashed/timed-out/garbage judge on a
        // previously-MET goal used to land in `Regressed` — which `any_regressed(invariants)` in the
        // shipped default `halt_when` turns into a HALT. The run died blaming a regression that
        // never happened. An error says "I could not grade this", not "this is not met".
        let mut g = cardinal("g", 28.0);
        g.apply(verdict(true, 28.0, 28.0));
        assert_eq!(g.state, Lifecycle::Met);

        assert_eq!(g.apply(Verdict::failed("judge timed out after 300s")), Lifecycle::Met);
        assert!(g.met(), "a met goal STAYS met when its judge blows up");
        assert!(!g.regressed());
        // the verdict IS recorded — the error text has to be visible somewhere.
        let v = g.last_verdict.as_ref().unwrap();
        assert!(v.error.is_some());
        assert!(v.rationale.contains("timed out"));
        // and the real state is still there underneath: a genuine not-met still regresses.
        assert_eq!(g.apply(verdict(false, 27.0, 28.0)), Lifecycle::Regressed);
    }

    #[test]
    fn a_broken_judge_never_moves_a_goal_that_was_never_met() {
        // The other half of the rule: an error must not INVENT progress either. A goal that never
        // got past Pending stays Pending — it does not become InProgress, Met, or Regressed.
        let mut g = cardinal("g", 28.0);
        assert_eq!(g.apply(Verdict::failed("no such file: ./judges/build.sh")), Lifecycle::Pending);
        assert!(!g.ever_met);
        // ...and it does not freeze the goal: the next honest verdict still lands.
        assert_eq!(g.apply(verdict(false, 18.0, 28.0)), Lifecycle::InProgress);
        assert_eq!(g.apply(Verdict::failed("judge exploded")), Lifecycle::InProgress);
        assert_eq!(g.apply(verdict(true, 28.0, 28.0)), Lifecycle::Met);
    }
}
