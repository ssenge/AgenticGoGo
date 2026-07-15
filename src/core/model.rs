//! Core data model: judges, verdicts, lifecycle states.
//!
//! A [`Judge`] is a goal made executable (§7.1 — "a judge IS a goal"): resolved by NAME from disk
//! (§5.1), it carries its run-set membership and the [`Lifecycle`] derived from its verdict
//! history, so we can detect `Regressed` — a judge that was met and is now unmet. A [`Verdict`] is
//! what a judge returns each step.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Derived lifecycle state of a judge across steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// never evaluated, or evaluated with zero progress
    Pending,
    /// evaluated, partial progress, not met
    InProgress,
    /// currently meets target
    Met,
    /// was Met in a prior step, now unmet — first-class signal
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
/// exactly this JSON to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub met: bool,
    /// numeric measure the judge emitted: a count, a percent, or 0/1. `None` = the judge emitted NO
    /// number (a binary goal says only `met`; a broken judge emits nothing usable). `Some(0.0)` is a
    /// REAL, measured zero and is DISTINCT from absent — `coverage.value == 0` must tell the two
    /// apart (`core::stop`), and `verdicts.jsonl` records the difference faithfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// denominator for cardinal/percentage goals. Absent (`None`) for the same reasons as `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// presentational only (draws the progress bar) — deliberately NOT readable in a condition.
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

/// How a resolved judge runs. The kind is decided by the FILE EXTENSION at resolution time
/// (§5.1): `.sh` ⇒ Script, `.md` ⇒ Llm. There is no `kind:` key and no registry — the old
/// serde-tagged `JudgeSpec` enum is gone.
#[derive(Debug, Clone)]
pub enum JudgeKind {
    /// a script whose stdout is the verdict JSON. `path` is the resolved file.
    Script { path: PathBuf },
    /// an LLM rubric ⇒ a tools-off call on the RULER. `path` is the resolved rubric file; `inputs`
    /// come from ITS OWN yaml frontmatter (§5.1), read at resolution time — no registry.
    Llm { path: PathBuf, inputs: Vec<String> },
}

impl JudgeKind {
    /// A short display tag for the scoreboard/dashboard.
    pub fn tag(&self) -> &'static str {
        match self {
            JudgeKind::Script { .. } => "script",
            JudgeKind::Llm { .. } => "llm",
        }
    }
}

/// A judge — a goal, made executable. Resolved by NAME from disk (§5.1); it carries its DoD-set
/// membership and its runtime lifecycle.
#[derive(Debug, Clone)]
pub struct Judge {
    /// the judge's NAME — the filename stem it was resolved from, and the id conditions reference.
    pub name: String,
    /// how it runs — decided by the resolved file's extension (§5.1).
    pub kind: JudgeKind,
    /// named in `sequence.invariants` — must STAY met.
    pub invariant: bool,
    /// in the DoD-set (`done_if` ∪ `invariants`) — the set the AGGREGATES range over (§5.3). A
    /// judge named ONLY in an `if` condition (e.g. `stalled`) is in the run-set but NOT the DoD-set,
    /// so `all_goals` can't be blocked on "we got stuck".
    pub in_dod: bool,

    // ---- runtime state ----
    pub state: Lifecycle,
    pub last_verdict: Option<Verdict>,
    /// has this judge EVER been met during this run? (drives regression detection)
    pub ever_met: bool,
}

impl Judge {
    /// Fold a fresh verdict into the judge's lifecycle state. Returns the new state.
    ///
    /// # A BROKEN JUDGE IS NOT A FAILING JUDGE
    /// A verdict carrying `error` (spawn failure, timeout, garbage stdout — every
    /// [`Verdict::failed`] path) is the judge saying *"I could not grade this"*, NOT *"this is not
    /// met"*. It therefore **never changes the lifecycle**: never a regression, never satisfies
    /// `done_if`, never marks a judge met. We record the verdict (its `rationale` carries the error
    /// text) and leave `state`/`ever_met` exactly as they were. `any_judge_error` (see `core::stop`)
    /// is the front-door signal, and `abort_if: … OR any_judge_error` the explicit policy.
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

    /// The presentational goal-type, INFERRED from the verdict (§7.1): a judge that emitted no
    /// number is `binary`; one with a `max` reads as `cardinal`; else `percentage`.
    pub fn type_str(&self) -> &'static str {
        match self.last_verdict.as_ref() {
            Some(v) if v.value.is_none() => "binary",
            Some(v) if v.max.is_some() => "cardinal",
            Some(_) => "percentage",
            None => "binary",
        }
    }

    /// Compact one-line scoreboard representation.
    pub fn scoreboard_line(&self) -> String {
        let measure = match &self.last_verdict {
            None => "—".to_string(),
            Some(v) if v.value.is_none() => if v.met { "yes".into() } else { "no".into() },
            Some(v) => format!("{:.0}/{:.0}", v.value.unwrap_or(0.0), v.max.unwrap_or(v.target)),
        };
        format!(
            "{} {:<18} {:<10} {}",
            self.state.glyph(),
            self.name,
            self.kind.tag(),
            measure
        )
    }
}
