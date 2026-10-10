//! `waiting` — why the loop is not working right now, for whom, and until when (A-18.1).
//!
//! Published in `state.json` (so `agg status --json` shows it live) and carried into the run report
//! when a run ENDS while waiting — a stop during a rate-limit wait is then distinguishable from a
//! stop mid-session.

use serde::{Deserialize, Serialize};

/// The one reason agg waits today: an agent CLI said its quota window is used up.
pub const RATE_LIMIT: &str = "rate_limit";

/// One wait in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    /// why the loop waits: `rate_limit`.
    pub reason: String,
    /// the agent whose limit it is (`claude` / `codex` / `copilot` / a custom backend's name).
    pub agent: String,
    /// epoch seconds at which the wait ends on its own. `agg send wake` can end it sooner.
    pub until: u64,
}
