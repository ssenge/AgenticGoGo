//! The LIVE STATUS document — `agg status --json`, schema `docs/schemas/status.v1.json` (A-17).
//!
//! The report answers "how did it end"; this answers "where is it now", for a program watching a run
//! from another process. Its field names are the report's wherever the two say the same thing
//! (`run_id`, `stop_reason`, `sessions`, `time`, `judges.*.last`, `open_asks`, `detectors`, `usage`,
//! `waiting`, `paths`), so one parser reads both.
//!
//! Cheap by construction: it reads the snapshot the loop publishes (`state.json`), the open asks
//! (`asks.jsonl`, refreshed by [`DashboardState::read`]) and whether the run's process is alive
//! (`run.pid`). It never runs a judge and never takes a lock the loop holds.
//!
//! ⚠ Like the report, ANY change to the serialized shape bumps [`SCHEMA_VERSION`] and ships a new
//! `docs/schemas/status.v<N>.json`; the schema is strict so the test catches a field that lands
//! without the version moving.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Detectors, OpenAsk, Paths, RatchetView, Sessions, Time, VerdictView, Waiting};
use crate::state::DashboardState;

/// The status document's version. Bump it on any change to the serialized shape.
pub const SCHEMA_VERSION: u32 = 1;

/// One run's live status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub schema_version: u32,
    pub agg_version: String,
    /// the report's `run_id` for this run; `null` for a snapshot written before run ids existed.
    pub run_id: Option<String>,
    pub project: String,
    /// a loop process is alive for this project right now.
    pub running: bool,
    /// the run published its ending; `stop_reason` says why. A run that is neither `running` nor
    /// `finished` was killed (SIGKILL, OOM, power loss).
    pub finished: bool,
    /// why the run ended, once it has; `null` while it runs.
    pub stop_reason: Option<String>,
    /// where the loop is: `starting` · `inject` · `run` · `verify` · `gate` · `backoff` · `staging` · `done`.
    pub phase: String,
    /// the step being run; `null` before the first and on a driver between steps.
    pub current_step: Option<CurrentStep>,
    /// the session number within this run.
    pub session: u32,
    /// sessions across every run of this project.
    pub lifetime_session: u32,
    pub time: Time,
    pub sessions: Sessions,
    /// every judge the run consulted, by name.
    pub judges: BTreeMap<String, LiveJudge>,
    pub detectors: Detectors,
    pub open_asks: Vec<OpenAsk>,
    /// running totals; the per-session breakdown is in the report.
    pub usage: crate::usage::Summary,
    pub waiting: Option<Waiting>,
    pub paths: Paths,
}

/// The step being run, and who runs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurrentStep {
    pub name: String,
    pub agent: String,
    /// the model agg asked for; `null` = the agent's own default. What actually ran is in the
    /// report's `session_log`.
    pub model: Option<String>,
}

/// One judge, as it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveJudge {
    /// its latest verdict; `null` until it has run.
    pub last: Option<VerdictView>,
    /// counted toward `done_if` (the DoD-set), as opposed to a control judge like `stalled`.
    pub in_dod: bool,
    /// `null` for a judge that is not ratcheted.
    pub ratchet: Option<RatchetView>,
}

/// The status of the run in `dir`, or an error if no run has ever published a snapshot there.
pub fn document(dir: &Path) -> anyhow::Result<Status> {
    let Some(s) = DashboardState::read(dir) else {
        anyhow::bail!(
            "no run snapshot yet (agg/private/state.json not found) — run `agg run` first \
             (it publishes state as it goes)."
        );
    };
    Ok(build(&s, run_is_alive(dir), crate::util::now_epoch(), super::build::paths(dir)))
}

/// Is a loop process alive for `dir`? Read-only: unlike the run's own double-run guard, a status
/// read never deletes a stale pid file.
fn run_is_alive(dir: &Path) -> bool {
    std::fs::read_to_string(crate::paths::run_pid(dir))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .is_some_and(crate::os::proc::pid_alive)
}

/// The document for snapshot `s`. Pure, so every field is testable without a run.
pub fn build(s: &DashboardState, running: bool, now: u64, paths: Paths) -> Status {
    let current_step = (!s.step.is_empty()).then(|| CurrentStep {
        name: s.step.clone(),
        agent: s.step_agent.clone(),
        model: Some(s.step_model.clone()).filter(|m| !m.is_empty()),
    });
    let judges = s
        .judge_views()
        .iter()
        .map(|j| {
            let live = LiveJudge { last: super::build::last_verdict(j), in_dod: j.in_dod, ratchet: s.ratchet.get(&j.name).cloned() };
            (j.name.clone(), live)
        })
        .collect();
    Status {
        schema_version: SCHEMA_VERSION,
        agg_version: env!("CARGO_PKG_VERSION").to_string(),
        run_id: Some(s.run_id.clone()).filter(|r| !r.is_empty()),
        project: s.project.clone(),
        running,
        finished: s.finished,
        stop_reason: s.finished.then(|| s.finish_reason.clone()),
        phase: s.phase.to_string(),
        current_step,
        session: s.session,
        lifetime_session: s.lifetime_session,
        time: live_time(s, running, now),
        sessions: s.sessions.clone(),
        judges,
        detectors: s.detectors.clone(),
        open_asks: s.asks.iter().map(super::build::open_ask).collect(),
        usage: s.usage.clone(),
        waiting: s.waiting.clone(),
        paths,
    }
}

/// The run clock: ticking against `now` while the run is alive (the snapshot is only republished at
/// stage changes, so a long session would otherwise read as frozen), else as last published.
fn live_time(s: &DashboardState, running: bool, now: u64) -> Time {
    if !running || s.clock_started_at == 0 {
        return s.time.clone();
    }
    let wall = now.saturating_sub(s.clock_started_at) as f64;
    let human = s.time.human_wait_time_s;
    Time { wall_time_s: wall, work_time_s: (wall - human).max(0.0), human_wait_time_s: human }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AskView, JudgeView};

    fn snapshot() -> DashboardState {
        let mut s = DashboardState {
            project: "p".into(),
            run_id: "run-1".into(),
            step: "work".into(),
            step_agent: "claude".into(),
            session: 3,
            clock_started_at: 1_000,
            time: Time { wall_time_s: 50.0, work_time_s: 40.0, human_wait_time_s: 10.0 },
            judges: vec![
                JudgeView { name: "cc".into(), state: "met".into(), met: true, judged: true, value: Some(7.0), in_dod: true, ..Default::default() },
                JudgeView { name: "stalled".into(), state: "pending".into(), ..Default::default() },
            ],
            asks: vec![AskView { id: "a1".into(), case: "choose".into(), options: Some(vec!["x".into(), "y".into()]), age_secs: 9, ..Default::default() }],
            ..Default::default()
        };
        s.ratchet.insert(
            "cc".into(),
            RatchetView { reference: Some(8.0), direction: crate::core::ratchet::Direction::Down, tolerance: 0.0, tolerance_pct: None },
        );
        s
    }

    /// A live run: the step with an unpinned model, judges keyed by name with their ratchet, asks
    /// with options, the clock ticking against `now`, and no stop reason yet.
    #[test]
    fn a_running_snapshot_reads_as_a_live_status() {
        let st = build(&snapshot(), true, 1_100, Paths::default());
        assert_eq!((st.schema_version, st.run_id.as_deref(), st.running, st.stop_reason), (1, Some("run-1"), true, None));
        assert_eq!(st.current_step, Some(CurrentStep { name: "work".into(), agent: "claude".into(), model: None }));
        assert_eq!(st.judges["cc"].ratchet.as_ref().and_then(|r| r.reference), Some(8.0));
        assert!(st.judges["cc"].last.as_ref().is_some_and(|v| v.met && v.value == Some(7.0)));
        assert!(st.judges["stalled"].last.is_none(), "a judge that has not run has no verdict");
        assert_eq!(st.open_asks[0].options.as_deref(), Some(&["x".to_string(), "y".to_string()][..]));
        assert_eq!((st.time.wall_time_s, st.time.work_time_s), (100.0, 90.0), "the clock ticks while it runs");
    }

    /// A finished (or killed) run reads as published: the frozen clock, the stop reason, no step id
    /// invented for a snapshot that predates run ids.
    #[test]
    fn a_stopped_snapshot_reads_as_published() {
        let mut s = snapshot();
        s.finished = true;
        s.finish_reason = "3/3 judges met".into();
        s.run_id.clear();
        s.step.clear();
        let st = build(&s, false, 1_100, Paths::default());
        assert_eq!((st.running, st.finished, st.stop_reason.as_deref()), (false, true, Some("3/3 judges met")));
        assert_eq!((st.run_id, st.current_step), (None, None));
        assert_eq!(st.time.wall_time_s, 50.0, "not running: the last published clock");
    }

    /// The wire format: `waiting` is always present, `null` when the loop is not waiting.
    #[test]
    fn waiting_is_an_explicit_null() {
        let v = serde_json::to_value(build(&snapshot(), false, 0, Paths::default())).unwrap();
        assert!(v.get("waiting").is_some_and(|w| w.is_null()), "{v}");
    }
}
