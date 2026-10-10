//! The RUN REPORT — `agg/private/report.json`, the one machine-readable answer to "how did it end".
//!
//! Before this existed, an embedding program had to piece the ending together from the exit code,
//! `state.json`, `verdicts.jsonl` and `project.json` — all documented as INTERNAL, so any refactor of
//! agg could break every wrapper. The report is the stable interface instead: versioned, published
//! as a JSON Schema (`docs/schemas/report.v2.json`), written atomically, and written on EVERY ending
//! agg can observe — done, aborted, stopped, an error, and a panic inside agg itself.
//!
//! ```text
//!   mod.rs    the format: `Report` and its parts, `Outcome`, the precedence rule
//!   build.rs  what the loop records as it runs (`Tally`), and `LoopState` → `Report`
//!   file.rs   the run handle: stale-report removal, `run_id`, the atomic write
//!   headless.rs  `--report-json`: logs to `agg/private/run.log`, the report alone on stdout
//! ```
//!
//! # Extending it
//!
//! ⚠ ANY change to the serialized shape — a field added, renamed or retyped — bumps
//! [`SCHEMA_VERSION`] and ships a new `docs/schemas/report.v<N>.json`. The schema is strict
//! (`additionalProperties: false`) precisely so the test that validates a real report against it
//! fails the moment a field lands without the version moving.

mod build;
mod file;
pub mod headless;
mod live;
pub mod status;

pub use build::{from_state, record_baseline, record_gate, Tally};
pub use crate::state::Waiting;
pub use file::{amend_to_error, begin, begin_as, clear, last_written_on_this_thread, read_text, write, write_fallback, RunHandle};
pub use live::{detectors, ratchets, Detectors, RatchetView};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The message a panic carried, for the `error` field. Panics carry `&str` or `String` in practice;
/// anything else is named as such rather than dropped.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with a non-string payload".into())
}

/// A TEST HOOK: fail on purpose at a named point, so the suite can prove an internal fault mid-run
/// still ends in an `error` report. `AGG_TEST_FAULT=<point>-error` returns an `Err`,
/// `<point>-panic` panics. Compiled into debug builds only — a release binary has no such switch.
#[cfg(debug_assertions)]
pub fn fault_point(point: &str) -> anyhow::Result<()> {
    match std::env::var("AGG_TEST_FAULT") {
        Ok(f) if f == format!("{point}-error") => anyhow::bail!("injected fault at {point} (AGG_TEST_FAULT)"),
        Ok(f) if f == format!("{point}-panic") => panic!("injected panic at {point} (AGG_TEST_FAULT)"),
        _ => Ok(()),
    }
}

/// The report format's version. Bump it on any change to the serialized shape (see module doc).
pub const SCHEMA_VERSION: u32 = 2;

/// How the run ended, as a consumer should act on it. Authoritative over the exit code.
///
/// Four values, not one per exit code: `--max-sessions` (exit 4) is an `aborted` run whose
/// `stop_reason` names the cap — to a consumer it is "gave up before done", like any ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// `done_if` held (exit 0).
    Done,
    /// `abort_if` fired, or a session cap was reached (exit 3 / 4).
    Aborted,
    /// an operator ended it: `agg stop`, SIGTERM, SIGINT (exit 5).
    Stopped,
    /// a fault inside agg or its environment — not a verdict about the work (exit 1).
    Error,
}

impl Outcome {
    /// The outcome of a run that ended with `res`. An `Err` is always `error`, whatever the loop was
    /// about to decide: precedence rule 1.
    pub fn of(res: &anyhow::Result<crate::plugin::RunOutcome>) -> Outcome {
        use crate::plugin::RunOutcome as R;
        match res {
            Ok(R::GoalsMet) => Outcome::Done,
            Ok(R::Halt) | Ok(R::MaxSessions) => Outcome::Aborted,
            Ok(R::Stopped) => Outcome::Stopped,
            Err(_) => Outcome::Error,
        }
    }
}

/// THE PRECEDENCE RULE for one session end: `error` > `stopped` > `done` > `aborted`.
///
/// `error` never reaches here — it is an `Err` that unwinds past the decision. Of the rest, a stop
/// wins because the operator's kill switch must never be reported as anything else, and `done` beats
/// `aborted` because work that met its Definition of Done in the same session a ceiling tripped is
/// done: the ceiling exists to stop wasted effort, and none is left to waste.
pub fn decide(stopped: bool, done: bool, aborted: bool) -> Option<Outcome> {
    if stopped {
        Some(Outcome::Stopped)
    } else if done {
        Some(Outcome::Done)
    } else if aborted {
        Some(Outcome::Aborted)
    } else {
        None
    }
}

/// The whole report. Field order is the serialized order, which is the order a human reads it in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub agg_version: String,
    /// fresh per run; also written to `agg/private/run.log`, so a reader can never mistake the
    /// previous run's report for this one's.
    pub run_id: String,
    pub project: String,
    pub outcome: Outcome,
    /// the process exit code `agg run` returns with this report.
    pub exit_code: u8,
    /// human-readable: which `abort_if` term fired, the cap, the stop reason, the error.
    pub stop_reason: String,
    /// the error chain (or panic message) when `outcome` is `error`, else `null`.
    pub error: Option<String>,
    /// epoch seconds.
    pub started_at: u64,
    pub finished_at: u64,
    pub sessions: Sessions,
    pub time: Time,
    /// every judge the run consulted, by name.
    pub judges: BTreeMap<String, JudgeEntry>,
    pub result: RunResult,
    pub open_asks: Vec<OpenAsk>,
    pub paths: Paths,
    /// the stop detectors as the run ended.
    pub detectors: Detectors,
    /// the run's usage totals: all of it, the worker part, the ruler part.
    pub usage: crate::usage::Summary,
    /// every session that made an agent call — session `0` is the baseline pass — with what each
    /// role spent on each model.
    pub session_log: Vec<crate::usage::SessionUsage>,
    /// the wait the run was in when it ended (A-18.1, a rate limit), else `null`.
    pub waiting: Option<Waiting>,
}

/// Worker sessions THIS run launched, by what became of their work.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub total: u32,
    /// sessions whose work a gate merged onto the base branch (a kept span counts every session in it).
    pub merged: u32,
    /// sessions whose work a gate discarded (regression rollback, a veto, a conflict).
    pub rolled_back: u32,
    /// sessions a gate did not merge because nothing improved — not a regression (A-15).
    pub no_improvement: u32,
    /// sessions staged on a span that no gate ever decided — the work is on `result.staged_branch`.
    pub staged_unmerged: u32,
}

/// The run clock, in seconds. Same definitions as the `wall_time` / `work_time` condition terms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Time {
    pub wall_time_s: f64,
    pub work_time_s: f64,
    pub human_wait_time_s: f64,
}

/// One judge: what it said last, what it said before any work (the baseline pass), and — for a
/// ratcheted judge — its ratchet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JudgeEntry {
    pub last: Option<VerdictView>,
    /// `null` on the driver path, which has no baseline pass (its judges are lazy).
    pub baseline: Option<VerdictView>,
    /// `null` for a judge that is not ratcheted.
    pub ratchet: Option<RatchetView>,
}

/// A verdict as the report states it: the judge contract, with `evidence` naming agg's KEPT copies
/// rather than the paths the judge declared (those pointed into a scratch that does not last).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerdictView {
    pub met: bool,
    /// `null` when the judge emitted no number (a binary judge, or a broken one) — never a fake 0.
    pub value: Option<f64>,
    pub max: Option<f64>,
    pub target: f64,
    pub rationale: String,
    /// set when the judge could not grade at all, as distinct from "not met".
    pub error: Option<String>,
    /// absolute paths of the evidence files agg kept for this verdict, under `paths.evidence_dir`.
    /// Omitted when none were kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

impl From<&crate::core::model::Verdict> for VerdictView {
    fn from(v: &crate::core::model::Verdict) -> Self {
        VerdictView {
            met: v.met,
            value: v.value,
            max: v.max,
            target: v.target,
            rationale: v.rationale.clone(),
            error: v.error.clone(),
            evidence: Vec::new(),
        }
    }
}

/// Where the work ended up in git.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// `null` when the run failed before it resolved one.
    pub base_branch: Option<String>,
    /// the tip of `base_branch` after the last gate.
    pub final_commit: Option<String>,
    /// the tip of a span no gate decided, if any: `git merge <it>` lands that work.
    pub staged_branch: Option<String>,
}

/// A question a human still owes the run an answer to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenAsk {
    pub id: String,
    /// `bool` | `choose` | `input`.
    pub kind: String,
    pub question: String,
    /// the allowed answers of a `bool`/`choose` ask; `null` for `input`.
    pub options: Option<Vec<String>>,
    pub age_s: u64,
}

/// Absolute paths to the run's other artefacts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Paths {
    pub report: String,
    /// the per-gate verdict ledger. Its ROW FORMAT is internal; the path is given for auditing.
    pub ledger: String,
    pub log: String,
    /// this run's kept judge evidence (`agg/private/evidence/<run_id>`); `null` when none was kept.
    pub evidence_dir: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::RunOutcome;

    /// The rule, as one table: every combination of the three conditions a session end can hold.
    #[test]
    fn precedence_is_stopped_then_done_then_aborted() {
        let o = |s, d, a| decide(s, d, a);
        assert_eq!(o(true, true, true), Some(Outcome::Stopped), "a stop beats everything");
        assert_eq!(o(true, false, false), Some(Outcome::Stopped));
        assert_eq!(o(false, true, true), Some(Outcome::Done), "done beats a ceiling in the same session");
        assert_eq!(o(false, true, false), Some(Outcome::Done));
        assert_eq!(o(false, false, true), Some(Outcome::Aborted));
        assert_eq!(o(false, false, false), None, "nothing holds: the loop goes round again");
    }

    /// An `Err` is `error` whatever else held, and each `RunOutcome` lands on the outcome a consumer
    /// acts on — max-sessions included, which has no outcome of its own.
    #[test]
    fn every_ending_maps_to_one_outcome() {
        assert_eq!(Outcome::of(&Ok(RunOutcome::GoalsMet)), Outcome::Done);
        assert_eq!(Outcome::of(&Ok(RunOutcome::Halt)), Outcome::Aborted);
        assert_eq!(Outcome::of(&Ok(RunOutcome::MaxSessions)), Outcome::Aborted);
        assert_eq!(Outcome::of(&Ok(RunOutcome::Stopped)), Outcome::Stopped);
        assert_eq!(Outcome::of(&Err(anyhow::anyhow!("boom"))), Outcome::Error);
    }

    /// The wire spelling is the contract: snake_case outcome names, and `null` (not an omitted key)
    /// for an absent number, so a consumer's schema check sees every field every time.
    #[test]
    fn a_panic_message_survives_both_payload_types() {
        let p = std::panic::catch_unwind(|| panic!("plain")).unwrap_err();
        assert_eq!(panic_message(&*p), "plain");
        let p = std::panic::catch_unwind(|| panic!("{}", String::from("formatted"))).unwrap_err();
        assert_eq!(panic_message(&*p), "formatted");
        let p = std::panic::catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
        assert!(panic_message(&*p).contains("non-string"));
    }

    #[test]
    fn the_wire_format_spells_outcomes_and_absent_numbers_explicitly() {
        assert_eq!(serde_json::to_string(&Outcome::Aborted).unwrap(), "\"aborted\"");
        let v = serde_json::to_value(VerdictView::default()).unwrap();
        assert!(v.get("value").is_some_and(|x| x.is_null()), "absent value must serialize as null: {v}");
    }
}
