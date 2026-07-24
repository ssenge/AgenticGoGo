//! AgenticGoGo (`agg`) — a Ralph-loop harness for Claude Code workers.
//!
//! This library crate exposes the harness internals so they can be driven from integration
//! tests (`tests/`) and, in principle, embedded. The `agg` binary (`main.rs`) is a thin CLI
//! over these modules.
//!
//! # Layout
//!
//! The dependency graph was always a clean acyclic DAG; the flat 30-file layout just hid it.
//! Four groups now make the layering visible, and each one's module doc says what it is for:
//!
//! ```text
//!   core/     what the harness IS          model · stop · engine · judge · memory · config
//!   backend/  what agent we DRIVE          backend · stream · worker
//!   os/       processes, signals, reaping  proc · reap · spawns · signals · detach
//!   ui/       read-only views on a run     dashboard · status · serve · localtime
//! ```
//!
//! Everything else sits at the top level because it spans the groups (`loop_` orchestrates all
//! four) or is a leaf they all lean on (`util`, `paths`, `state`, `bus`, `git`).
//!
//! The pure cores — `core::model`, `core::engine`, `core::stop`, `core::config`, `util`, `paths`,
//! and `git`'s decision logic — are the most valuable to test directly.
//!
//! Everything stays `pub`: the integration tests drive these internals by design. A narrower
//! facade was considered and rejected as YAGNI.

pub mod backend;
pub mod core;
pub mod os;
pub mod ui;

pub mod assembly;
pub mod bus;
pub mod capability;
pub mod context;
pub mod doctor;
pub mod features;
pub mod git;
pub mod hooks;
pub mod init;
pub mod isolation;
pub mod loop_;
pub mod paths;
pub mod plugin;
pub mod project;
pub mod registry;
pub mod skills;
pub mod state;
pub mod summary;
pub mod util;
