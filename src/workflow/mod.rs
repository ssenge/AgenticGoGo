//! Workflow building blocks on the driver API (A-26 to A-31): a process of named steps, each a script, an
//! agent's work judged by checks and a critic and repaired, a condition, or a human's approval; with a step
//! history, re-entry at a step with a note, and triage when a step fails. A library over `agg.step`,
//! `agg.judge`, `agg.gate` and `agg.hil_bool`: no second execution model.
//!
//! A step's key is the first word of its name (`"2.5"` of `"2.5 Resolve bindings"`): what `--from` and
//! `--until` name, and what the history and the session store go by.

mod brief;
mod git;
mod history;
mod task;

pub use brief::{task, with_lead, with_section};
pub use git::{before, changes_since, commit, take_back, unchanged_since};
pub use history::{History, DEFAULT as HISTORY};
pub use task::{Outcome, Task};

/// A step's key: the first word of its name.
pub fn key(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}
