//! Workflow building blocks on the driver API (A-26 to A-31, A-36): a process of named steps, each a script, an
//! agent's work (discussed first by other agents, if asked) judged by checks and a critic and repaired, a
//! condition, or a human's approval; with a step history, re-entry at a step with a note, and the task force when
//! a step fails. A library over `agg.step`, `agg.judge`, `agg.gate` and `agg.hil_bool`: no second execution
//! model.
//!
//! A step's key is the first word of its name (`"2.5"` of `"2.5 Resolve bindings"`): what `--from` and
//! `--until` name, and what the history goes by.

mod agent;
mod approval;
mod brief;
mod discussion;
mod flow;
mod git;
mod history;
mod review;
mod roles;
mod scratch;
mod task;
mod task_force;

pub use brief::{task, with_lead, with_section};
pub use flow::{Flow, Run, When};
pub use git::{before, changes_since, commit, take_back, unchanged_since};
pub use history::{History, DEFAULT as HISTORY};
pub use roles::{Role, Roles};
pub use task::{Outcome, Task};
pub use task_force::run;

/// A step's key: the first word of its name.
pub fn key(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}
