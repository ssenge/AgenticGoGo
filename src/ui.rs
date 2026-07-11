//! **ui** — the read-only views onto a run. None of these drive the loop.
//!
//! Every surface here is a *viewer*: it reads the snapshot the loop publishes to `.agg/state.json`
//! and renders it. `dashboard` is the live TUI, `status` the one-shot text render (also the
//! `--once` headless snapshot), `serve` the JSON API the standalone web UI polls, and `localtime`
//! the zone conversion they all share so timestamps match the user's wall clock.
//!
//! The one exception to "read-only" is `serve`'s `POST /api/send`, which does not touch the loop
//! either — it queues a command onto the same bus `agg send` writes to.

pub mod dashboard;
pub mod localtime;
pub mod serve;
pub mod status;
