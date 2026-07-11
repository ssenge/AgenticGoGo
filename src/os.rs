//! **os** — the operating-system layer: processes, process groups, signals, detaching.
//!
//! Every syscall the harness makes lives under here, behind `#[cfg]` where platforms differ.
//! `proc` owns the primitives (kill / liveness / the timeout runner); `reap` sweeps stragglers a
//! session leaves behind; `spawns` tracks the long tasks that are *meant* to outlive a session so
//! the reaper spares them; `signals` turns a Ctrl-C into a graceful unwind instead of an orphaned
//! worker; `detach` puts a run in the background.
//!
//! This is the layer that makes an autonomous loop safe to leave alone: nothing it launches can
//! leak, and nothing it kills was something you wanted.

pub mod detach;
pub mod proc;
pub mod reap;
pub mod signals;
pub mod spawns;
