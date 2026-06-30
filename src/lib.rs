//! AgenticGoGo (`agg`) — a Ralph-loop harness for Claude Code workers.
//!
//! This library crate exposes the harness internals so they can be driven from integration
//! tests (`tests/`) and, in principle, embedded. The `agg` binary (`main.rs`) is a thin CLI
//! over these modules. The pure cores — `model`, `engine`, `stop`, `config`, `util`, `paths`,
//! `git`'s decision logic — are the most valuable to test directly.

pub mod bus;
pub mod config;
pub mod dashboard;
pub mod detach;
pub mod doctor;
pub mod engine;
pub mod git;
pub mod hooks;
pub mod init;
pub mod judge;
pub mod localtime;
pub mod loop_;
pub mod memory;
pub mod model;
pub mod paths;
pub mod proc;
pub mod project;
pub mod reap;
pub mod spawns;
pub mod state;
pub mod status;
pub mod stop;
pub mod stream;
pub mod summary;
pub mod util;
pub mod worker;
