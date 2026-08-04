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
//! types** — the things a driver names, matches on and stores. The facade (`Agg`) and the builders
//! (`Step`, `Judge`) land in later commits.
//!
//! # The rule that shapes every type in here
//!
//! ⛔ **A stray `agg.yaml` in a driver project is IGNORED** — not merged, not a fallback. The two
//! paths share FILES (`agg/judges/`, `agg/AGG.md`, `agg/state/`), never configuration. Types that
//! look like config ([`Limits`](crate::core::config::Limits), [`Isolation`](crate::isolation::Isolation))
//! are REUSED from the shipped structs rather than redefined, so there is one definition of each
//! knob — but on this path they are populated in Rust and only in Rust.

mod types;

pub use types::{
    Agent, Effort, Fatal, GateFailure, GateOutcome, Landing, OnRegression, Opts, StepOutcome,
};
