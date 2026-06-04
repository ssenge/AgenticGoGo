//! Dashboard state — the serializable snapshot the loop writes and the TUI reads.
//!
//! Two-stream discipline (the hard lesson from a prior harness): the line-oriented log
//! on stdout stays the source of truth (greppable, tailable). The TUI is a *view*
//! rendered from this compact state file, never the only output. The loop writes
//! `.agg/state.json` atomically after each meaningful change; `agg dashboard`
//! polls it and repaints in place.

use crate::engine::Engine;
use crate::model::{GoalType, Lifecycle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single goal's view for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalView {
    pub id: String,
    pub goal_type: String,   // "binary"/"percentage"/"cardinal"
    pub state: String,       // lifecycle: "pending"/"in_progress"/"met"/"regressed"
    pub invariant: bool,
    pub value: f64,
    pub max: f64,
    pub target: f64,
    pub delta: f64,          // change in value since last cycle (for ▲+N)
    pub rationale: String,
    pub judge_kind: String,  // "script" | "llm:<model>"
}

/// The full dashboard snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DashboardState {
    pub project: String,
    pub stop_when: String,
    pub up_secs: u64,
    pub session: u32,
    pub phase: String,       // "running" | "judging" | "backoff" | "done" | ...
    pub idle_secs: u64,
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    pub goals_met: usize,
    pub goals_total: usize,
    pub goals: Vec<GoalView>,
    pub now: String,         // current activity line (last 🔧/💬)
    pub think: String,       // last 💬 thought
    pub summary_cumulative: String,
    pub summary_windowed: String,
    /// monotonically increasing; lets the dashboard detect updates
    pub seq: u64,
    /// terminal flag — dashboard shows the final banner and can exit
    pub finished: bool,
    pub finish_reason: String,
}

impl DashboardState {
    /// Path to the state file under a project dir.
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".agg").join("state.json")
    }

    /// Snapshot the current goal set from the engine into goal views.
    pub fn goals_from_engine(eng: &Engine, prev: &[GoalView]) -> Vec<GoalView> {
        eng.goals
            .iter()
            .map(|g| {
                let value = g.last_verdict.as_ref().map(|v| v.value).unwrap_or(0.0);
                let prev_value = prev.iter().find(|p| p.id == g.id).map(|p| p.value).unwrap_or(value);
                let judge_kind = match &g.judge {
                    crate::model::JudgeSpec::Script { .. } => "script".to_string(),
                    crate::model::JudgeSpec::Llm { model, .. } => format!("llm:{model}"),
                };
                GoalView {
                    id: g.id.clone(),
                    goal_type: type_str(g.goal_type),
                    state: state_str(g.state),
                    invariant: g.invariant,
                    value,
                    max: g.last_verdict.as_ref().map(|v| v.max).unwrap_or(1.0),
                    target: g.target,
                    delta: value - prev_value,
                    rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
                    judge_kind,
                }
            })
            .collect()
    }

    /// Write atomically (write tmp, rename) so the dashboard never reads a torn file.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let dest = Self::path(dir);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("json.tmp");
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &dest)
    }

    /// Read the latest state, or None if missing/unparseable.
    pub fn read(dir: &Path) -> Option<DashboardState> {
        let text = std::fs::read_to_string(Self::path(dir)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

fn type_str(t: GoalType) -> String {
    match t {
        GoalType::Binary => "binary",
        GoalType::Percentage => "percentage",
        GoalType::Cardinal => "cardinal",
    }
    .to_string()
}

fn state_str(s: Lifecycle) -> String {
    match s {
        Lifecycle::Pending => "pending",
        Lifecycle::InProgress => "in_progress",
        Lifecycle::Met => "met",
        Lifecycle::Regressed => "regressed",
    }
    .to_string()
}
