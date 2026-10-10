//! What the loop records for the report as it runs, and the `LoopState` → [`Report`] assembly.

use std::collections::BTreeMap;
use std::path::Path;

use super::{JudgeEntry, OpenAsk, Outcome, Paths, Report, RunHandle, RunResult, Sessions, Time, VerdictView};
use crate::context::LoopState;
use crate::core::model::Verdict;
use crate::features::state::AGGState;
use crate::plugin::RunOutcome;

/// The report's per-run counters, kept in the run's `ext` store like any feature's state.
///
/// They are counted AS THE GATE DECIDES rather than reconstructed afterwards: the gate is the only
/// place that knows how many sessions a kept span carried, and `verdicts.jsonl` (which records it)
/// is internal and worker-readable.
#[derive(Default)]
pub struct Tally {
    pub merged: u32,
    pub rolled_back: u32,
    /// each judge's verdict from the baseline pass, before any session ran.
    pub baseline: BTreeMap<String, VerdictView>,
}

/// Record one gate decision about the span ending in `branch`: `kept` merged it onto base, else it
/// was discarded. A span carries every session staged on it, and they all share its fate.
pub fn record_gate(ctx: &mut LoopState, kept: bool, branch: Option<&str>) {
    let git = &ctx.ext.get::<AGGState>().git;
    // the span's other branches, plus the one gated — which IS in the list on the driver path (the
    // tip) and is not on the YAML path (a judged session cut off the tip).
    let n = git.span_branches.iter().filter(|b| Some(b.as_str()) != branch).count() as u32 + 1;
    let t = ctx.ext.get::<Tally>();
    if kept {
        t.merged += n;
    } else {
        t.rolled_back += n;
    }
}

/// Record the baseline pass's verdicts — the "before" column of the report's judge table.
pub fn record_baseline(ctx: &mut LoopState, fresh: &[(String, Verdict)]) {
    let t = ctx.ext.get::<Tally>();
    for (name, v) in fresh {
        t.baseline.insert(name.clone(), v.into());
    }
}

/// Build the report for a run that ended with `res`, from the live loop state.
pub fn from_state(st: &mut LoopState, run: &RunHandle, res: &anyhow::Result<RunOutcome>) -> Report {
    // refreshes `dash.judges` from the engine on the YAML path; the driver path's are already there.
    st.publish();
    let now = crate::util::now_epoch();
    let (iso_base, span_tip, staged) = {
        let git = &st.ext.get::<AGGState>().git;
        (git.iso_base.clone(), git.span_tip.clone(), git.span_branches.len() as u32)
    };
    let tally = std::mem::take(st.ext.get::<Tally>());
    let usage = std::mem::take(st.ext.get::<crate::usage::Ledger>());
    let base_branch = Some(iso_base).filter(|b| !b.is_empty());
    let final_commit = base_branch.as_deref().and_then(|b| crate::git::rev_parse(&st.dir, b));
    Report {
        sessions: Sessions { total: st.session, merged: tally.merged, rolled_back: tally.rolled_back, staged_unmerged: staged },
        time: Time {
            wall_time_s: st.clock.wall_secs(now),
            work_time_s: st.clock.work_secs(now),
            human_wait_time_s: st.clock.human_wait_secs,
        },
        judges: judges(&st.dash.judges, tally.baseline, &st.dash.ratchet),
        result: RunResult { base_branch, final_commit, staged_branch: span_tip },
        open_asks: open_asks(&st.dir, now),
        detectors: st.dash.detectors.clone(),
        usage: usage.summary(),
        session_log: usage.sessions().to_vec(),
        waiting: st.dash.waiting.clone(),
        ..skeleton(&st.dir, &st.cfg.project, run.run_id(), run.started_at(), res, &st.dash.finish_reason)
    }
}

/// The report for a run that never built its loop state — every count zero, every git fact null.
pub(super) fn fallback(dir: &Path, project: &str, run_id: &str, started_at: u64, error: &str) -> Report {
    skeleton(dir, project, run_id, started_at, &Err(anyhow::anyhow!("{error}")), "")
}

/// The fields every report has, whatever state was available to build it from.
fn skeleton(
    dir: &Path,
    project: &str,
    run_id: &str,
    started_at: u64,
    res: &anyhow::Result<RunOutcome>,
    finish_reason: &str,
) -> Report {
    let error = res.as_ref().err().map(|e| format!("{e:#}"));
    let stop_reason = match &error {
        Some(e) => format!("error: {e}"),
        None if !finish_reason.is_empty() => finish_reason.to_string(),
        None => format!("{:?}", Outcome::of(res)).to_lowercase(),
    };
    Report {
        schema_version: super::SCHEMA_VERSION,
        agg_version: env!("CARGO_PKG_VERSION").to_string(),
        run_id: run_id.to_string(),
        project: project.to_string(),
        outcome: Outcome::of(res),
        exit_code: res.as_ref().map(|o| o.exit_code()).unwrap_or(1),
        stop_reason,
        error,
        started_at,
        finished_at: crate::util::now_epoch(),
        sessions: Sessions::default(),
        time: Time::default(),
        judges: BTreeMap::new(),
        result: RunResult::default(),
        open_asks: Vec::new(),
        paths: paths(dir),
        detectors: super::Detectors::default(),
        usage: crate::usage::Summary::default(),
        session_log: Vec::new(),
        waiting: None,
    }
}

/// Every judge the run consulted: its last verdict (the published scoreboard, which both entry
/// points fill) joined with its baseline and its ratchet. A judge that never produced a verdict has
/// `last: null`.
pub(super) fn judges(
    board: &[crate::state::JudgeView],
    mut baseline: BTreeMap<String, VerdictView>,
    ratchet: &BTreeMap<String, super::RatchetView>,
) -> BTreeMap<String, JudgeEntry> {
    let mut out = BTreeMap::new();
    for j in board {
        let entry = JudgeEntry { last: last_verdict(j), baseline: baseline.remove(&j.name), ratchet: ratchet.get(&j.name).cloned() };
        out.insert(j.name.clone(), entry);
    }
    for (name, b) in baseline {
        let entry = JudgeEntry { last: None, baseline: Some(b), ratchet: ratchet.get(&name).cloned() };
        out.insert(name, entry);
    }
    out
}

/// A scoreboard row as a verdict — `None` while the judge has not run.
pub(super) fn last_verdict(j: &crate::state::JudgeView) -> Option<VerdictView> {
    j.judged.then(|| VerdictView {
        met: j.met,
        value: j.value,
        max: j.max,
        target: j.target,
        rationale: j.rationale.clone(),
        error: j.error.clone(),
    })
}

/// An open ask as both the report and the live status state it.
pub(super) fn open_ask(v: &crate::state::AskView) -> OpenAsk {
    OpenAsk { id: v.id.clone(), kind: v.case.clone(), question: v.question.clone(), options: v.options.clone(), age_s: v.age_secs }
}

fn open_asks(dir: &Path, now: u64) -> Vec<OpenAsk> {
    crate::core::asks::open(dir).iter().map(|a| open_ask(&crate::state::AskView::of(a, now))).collect()
}

/// Absolute paths, so a consumer can open them from any cwd.
pub(super) fn paths(dir: &Path) -> Paths {
    let abs = |p: std::path::PathBuf| {
        let p = if p.is_absolute() { p } else { std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p) };
        p.to_string_lossy().into_owned()
    };
    Paths {
        report: abs(crate::paths::report_json(dir)),
        ledger: abs(crate::paths::verdicts_jsonl(dir)),
        log: abs(crate::paths::run_log(dir)),
        evidence_dir: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fallback is an `error` report with exit 1, the message in both `error` and `stop_reason`,
    /// and zeroes/nulls rather than invented facts.
    #[test]
    fn a_fallback_reports_the_error_and_invents_nothing() {
        let r = fallback(Path::new("/proj"), "p", "run-1", 7, "no agg.yaml here");
        assert_eq!(r.outcome, Outcome::Error);
        assert_eq!(r.exit_code, 1);
        assert_eq!(r.error.as_deref(), Some("no agg.yaml here"));
        assert_eq!(r.stop_reason, "error: no agg.yaml here");
        assert_eq!(r.sessions, Sessions::default());
        assert_eq!(r.result, RunResult::default());
        assert_eq!(r.paths.report, "/proj/agg/private/report.json");
        assert_eq!(r.schema_version, super::super::SCHEMA_VERSION);
    }

    /// A clean ending carries its exit code, and the loop's own finish reason when it has one.
    #[test]
    fn a_clean_ending_states_its_reason_and_code() {
        let r = skeleton(Path::new("/p"), "p", "r", 0, &Ok(RunOutcome::MaxSessions), "reached max_sessions=2");
        assert_eq!((r.outcome, r.exit_code, r.error), (Outcome::Aborted, 4, None));
        assert_eq!(r.stop_reason, "reached max_sessions=2");
        let r = skeleton(Path::new("/p"), "p", "r", 0, &Ok(RunOutcome::Stopped), "");
        assert_eq!(r.stop_reason, "stopped", "an empty finish reason falls back to the outcome name");
    }

    /// A judge only the baseline saw still appears, with `last: null`.
    #[test]
    fn judges_join_the_scoreboard_with_the_baseline() {
        use crate::state::JudgeView;
        let mut b = BTreeMap::new();
        b.insert("a".to_string(), VerdictView { met: false, ..Default::default() });
        b.insert("gone".to_string(), VerdictView::default());
        let dash = vec![
            JudgeView { name: "a".into(), state: "met".into(), met: true, judged: true, ..Default::default() },
            JudgeView { name: "p".into(), state: "pending".into(), ..Default::default() },
            JudgeView { name: "z".into(), state: "pending".into(), judged: true, value: Some(0.0), ..Default::default() },
        ];
        let mut ratchet = BTreeMap::new();
        ratchet.insert(
            "a".to_string(),
            super::super::RatchetView { reference: Some(3.0), direction: crate::core::ratchet::Direction::Up, tolerance: 0.0, tolerance_pct: None },
        );
        let out = judges(&dash, b, &ratchet);
        assert_eq!(out["a"].ratchet.as_ref().and_then(|r| r.reference), Some(3.0), "a ratcheted judge carries its reference");
        assert!(out["p"].ratchet.is_none(), "an unratcheted one carries none");
        assert!(out["a"].last.as_ref().is_some_and(|v| v.met));
        assert!(out["a"].baseline.as_ref().is_some_and(|v| !v.met));
        assert!(out["p"].last.is_none(), "a judge that has not run has no verdict yet");
        assert!(out["z"].last.as_ref().is_some_and(|v| !v.met), "an unmet judge at 0 HAS run — its verdict is reported");
        assert!(out["gone"].last.is_none() && out["gone"].baseline.is_some());
    }
}
