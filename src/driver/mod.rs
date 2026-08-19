//! The Rust driver API — a second entry point beside `agg.yaml`, for flow YAML cannot express.
//!
//! YAML + `agg run` stays the default and the 80% case. This path exists for the runs whose shape
//! is `for` / `if` / `break` / labeled `continue` — i.e. the host language's control flow, used
//! directly instead of re-encoded as data. A driver is an ordinary Rust binary that depends on this
//! crate and calls the facade.
//!
//! A driver starts `use agg::prelude::*;` and returns `Result<(), Fatal>` from `main`.
//!
//! # What lives here
//!
//! This module is being built in the order of BUILD.md §4. Today it holds the **public value
//! types** — the things a driver names, matches on and stores — [`Step`], the step builder with its
//! template merge rules, and [`Agg`], the facade itself.
//!
//! Run end is `Drop for Agg`: `main` returning IS the end of the run, so the ledger, `state.json`
//! and the ungated-span warning are written there rather than by a post-loop block no driver would
//! call. ⛔ agg never auto-merges an ungated span and never auto-rolls it back — it names the branch
//! and the `git merge` that lands it, and leaves the call to the operator.
//!
//! `Judge` and `Verdict` are NOT here: they are the shipped [`crate::core::model`] types, extended
//! in place with the driver's constructors ([`Judge::rubric`](crate::core::model::Judge::rubric),
//! `script`, `native`) rather than twinned — one definition of a judge, whichever path built it.
//!
//! # The rule that shapes every type in here
//!
//! ⛔ **A stray `agg.yaml` in a driver project is IGNORED** — not merged, not a fallback. The two
//! paths share FILES (`agg/judges/`, `agg/AGG.md`, `agg/state/`), never configuration. Types that
//! look like config ([`Limits`](crate::core::config::Limits), [`Isolation`](crate::isolation::Isolation))
//! are REUSED from the shipped structs rather than redefined, so there is one definition of each
//! knob — but on this path they are populated in Rust and only in Rust.

mod facade;
mod step;
mod types;

pub use facade::{Agg, PosFrame};
pub use step::Step;
pub use types::{
    run, Agent, Effort, Fatal, GateFailure, GateOutcome, Landing, OnRegression, Opts, StepOutcome,
};
