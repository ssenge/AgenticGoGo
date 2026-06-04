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
    /// numeric measure: a count, a percent, or 0/1
    #[serde(default)]
    pub value: f64,
    /// denominator for cardinal/percentage goals
    #[serde(default = "one")]
    pub max: f64,
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
            value: 0.0,
            max: 1.0,
            target: 1.0,
            rationale: format!("judge failed: {error}"),
            evidence: vec![],
            error: Some(error),
        }
    }
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

    // ---- runtime state (not deserialized from goals.yaml) ----
    #[serde(skip, default = "default_lifecycle")]
    pub state: Lifecycle,
    #[serde(skip)]
    pub last_verdict: Option<Verdict>,
    /// has this goal EVER been met during this run? (drives regression detection)
    #[serde(skip)]
    pub ever_met: bool,
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
    /// A cheap `claude -p --bare` call with a rubric prompt (Phase 2).
    Llm {
        #[serde(default = "default_model")]
        model: String,
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
fn default_model() -> String {
    "haiku".to_string()
}

impl Goal {
    /// Fold a fresh verdict into the goal's lifecycle state. Returns the new state.
    pub fn apply(&mut self, verdict: Verdict) -> Lifecycle {
        let met = verdict.met;
        let value = verdict.value;
        self.last_verdict = Some(verdict);
        self.state = if met {
            self.ever_met = true;
            Lifecycle::Met
        } else if self.ever_met {
            // was met before, now not -> regression
            Lifecycle::Regressed
        } else if value > 0.0 {
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
            (Some(v), GoalType::Percentage) => format!("{:.0}/{:.0}%", v.value, self.target),
            (Some(v), GoalType::Cardinal) => format!("{:.0}/{:.0}", v.value, v.max),
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
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    fn verdict(met: bool, value: f64, max: f64) -> Verdict {
        Verdict { met, value, max, target: max, rationale: String::new(), evidence: vec![], error: None }
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
}
