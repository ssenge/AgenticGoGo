//! **core** — what the harness *is*, independent of any agent, OS, or display.
//!
//! The pure decision layer: the goal/verdict data model, the condition language the stop/halt
//! rules are written in, the engine that folds verdicts into a cycle result, the judges that
//! produce those verdicts, the durable memory, and the config that parameterizes all of it.
//!
//! These are the modules worth testing directly (and the integration tests do). Nothing here
//! spawns an agent, touches a process group, or paints a terminal — those are [`crate::backend`],
//! [`crate::os`] and [`crate::ui`].

pub mod config;
pub mod engine;
pub mod judge;
pub mod memory;
pub mod model;
pub mod stop;
