//! The task force (A-30, A-35): when a step fails, an agent looks for the cause. Where it can, it repairs
//! the cause in place, and the step is judged again; met, the run goes on. Otherwise it proposes to go back to
//! the earliest step whose result is wrong, with a note, and after a human's yes the run ends and [`run`]
//! starts it again from there (A-29); or it leaves the failure to a human.

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::OnceLock;

use super::brief::with_section;
use super::flow::{Flow, Run};
use crate::driver::{Fatal, Step};

/// Where the next run re-enters, its note, the project, and whether the note is the humans' decision: set
/// by the task force or the driver, read by [`run`] after the run.
static REENTRY: OnceLock<(String, String, PathBuf, bool)> = OnceLock::new();

/// How a failed step is judged again after a repair in place: `None` when it is met now, else why not.
pub type Again<'a> = &'a dyn Fn() -> Result<Option<String>, Fatal>;

/// The task force's answer: `outcome` (`fixed`, `reenter` or `escalate`), the step to re-enter, the cause, the note.
struct Answer {
    outcome: String,
    at: String,
    cause: String,
    note: String,
}

impl Flow {
    /// On a failed step, `step` (an agent) looks for the failure's origin, up to `max` re-entries per run.
    pub fn on_failure(mut self, step: Step, max: u32) -> Flow {
        self.task_force = Some((step, max));
        self
    }

    /// The task force's proposals that `rule` (the failed step, the step to re-enter) accepts go ahead without a
    /// human's yes, still within the run's re-entries: for a failure the driver knows is the re-entered step's
    /// own to repair (a check of a step's script, sent back to the step that writes it).
    pub fn trust(mut self, rule: impl Fn(&str, &str) -> bool + 'static) -> Flow {
        self.trusted = Some(Box::new(rule));
        self
    }

    /// Ends this run and starts it again at step `at` (one this run has seen) with `note`, as after a proposal
    /// a human said yes to; it counts as a re-entry. For a driver that knows itself where the run must go
    /// back: a decision taken at an approval that an earlier step's result doesn't carry yet. The note is a
    /// decision (`--decided`): step `at` refreshes against it, and stays unchanged where it doesn't concern it.
    pub fn reenter(&self, at: &str, note: &str) -> Result<(), Fatal> {
        if !self.seen().iter().any(|s| s == at) {
            return Err(Fatal::Other(anyhow::anyhow!("re-entering at {at}: no step of this run")));
        }
        self.record(at, "reentry", true, note);
        let _ = REENTRY.set((at.to_string(), note.to_string(), self.project().to_path_buf(), true));
        self.halt(format!("re-entering at {at}: {note}"))
    }
}

/// Step `name` failed for `why`: the task force repairs it in place (when `again` can judge the step again), proposes
/// a re-entry a human decides, or leaves it to a human; without the task force, or past its limit, the run ends.
pub(crate) fn fail(flow: &Flow, name: &str, why: &str, again: Option<Again>) -> Result<(), Fatal> {
    let reason = format!("{name}: {why}");
    let spent = flow.run.reentries + flow.fixes.get();
    let Some((force, max)) = flow.task_force.as_ref() else {
        return flow.halt(reason);
    };
    if spent >= *max {
        return flow.halt(format!("{reason} (the task force's {max} re-entries and repairs of this run are spent)"));
    }
    let file = flow.scratch("task-force.yaml")?;
    let step = with_section(&flow.absolute(force), &brief(flow, name, why, &file, again.is_some()))?;
    flow.flush()?;
    let head = super::git::git_out(flow.project(), &["rev-parse", "HEAD"]).unwrap_or_default();
    flow.step(&step)?;
    flow.gate()?;
    let a = answer(&file);
    match (a.outcome.as_str(), again) {
        ("fixed", Some(again)) => fixed(flow, name, &a, (again, head.trim())),
        ("reenter", _) => reentered(flow, name, &reason, a),
        _ => {
            flow.record(name, "task-force", false, &format!("no re-entry: {}", a.cause));
            flow.halt(format!("{reason} (task force: {})", a.cause))
        }
    }
}

/// The task force's brief: the failure, the run's steps, and the answer it writes to `file`.
fn brief(flow: &Flow, name: &str, why: &str, file: &std::path::Path, in_place: bool) -> String {
    let fixed = if in_place {
        "`fixed`: you repaired the cause in place, and the failed step is judged again"
    } else {
        "`fixed` is no answer here: this failure can't be judged again in place"
    };
    let keep = flow.constraints.as_ref().and_then(|f| f(flow, name)).map(|c| format!(
        "## What step {name} must keep to\n\nA repair in place is judged by all of the step's rules, not only the one \
         that failed: keep within these. Where the cause can't be repaired within them, answer `reenter` or \
         `escalate` instead.\n\n{c}\n\n")).unwrap_or_default();
    format!("## The failure\n\nStep {name} failed:\n\n{why}\n\n{keep}## The steps of this run, in order\n\n{}\n\nThis \
             run was re-entered {} times already.\n\n## Your answer\n\nWrite it to {} as YAML with four keys: \
             `outcome`, one of `fixed`, `reenter` or `escalate` ({fixed}; `reenter`: an earlier step's result is \
             wrong, and the run goes back to it; `escalate`: only a human can decide it); `reenter`, for that \
             outcome the step (as listed above) whose result must change; `cause`, in one or two sentences what \
             went wrong where; `note`, what you changed, or what that step's worker must fix.",
            flow.seen().join(", "), flow.run.reentries, file.display())
}

/// The task force repaired the cause in place: the step judged again goes on when met; else what is left is a new
/// failure, which the task force gets again while its limit lasts (a repair often uncovers the next point).
fn fixed(flow: &Flow, name: &str, a: &Answer, (again, head): (Again, &str)) -> Result<(), Fatal> {
    flow.fixes.set(flow.fixes.get() + 1);
    keep_spent(flow.project(), flow.run.reentries, flow.fixes.get());
    let changed = super::git::git_out(flow.project(), &["diff", "--name-only", head, "HEAD"]).unwrap_or_default();
    let changed = changed.split_whitespace().collect::<Vec<_>>().join(", ");
    let what = format!("fixed in place ({changed}): {}: {}", a.cause, a.note);
    match again()? {
        None => {
            flow.record(name, "task-force", true, &what);
            flow.flush()
        }
        Some(still) => {
            flow.record(name, "task-force", false, &format!("{what}; judged again: {still}"));
            fail(flow, name, &format!("{still} (after the task force's repair in place)"), Some(again))
        }
    }
}

/// The task force proposes to go back to `a.at`: after a human's yes (or a rule the driver trusts) the run ends for it.
fn reentered(flow: &Flow, name: &str, reason: &str, a: Answer) -> Result<(), Fatal> {
    if a.at.is_empty() || a.at == "none" || a.note.is_empty() || !flow.seen().contains(&a.at) {
        flow.record(name, "task-force", false, &format!("no re-entry: {}", a.cause));
        return flow.halt(format!("{reason} (task force: {})", a.cause));
    }
    let (at, cause, note) = (a.at, a.cause, a.note);
    // an approval takes no note: what is decided there is the humans' own, never an agent's proposal
    if flow.steps().is_approval(&at) {
        flow.record(name, "task-force", false, &format!("humans decide at {at}: {cause}"));
        return flow.halt(format!("{reason} (task force: humans decide at {at}: {cause})"));
    }
    let trusted = flow.trusted.as_ref().is_some_and(|rule| rule(name, &at));
    if !trusted && !flow.hil_bool(&format!("{reason}\n\nTask force: {cause}\nRe-enter at {at}, with the note: {note}\nGo?"))? {
        return flow.halt(reason.to_string());
    }
    let how = if trusted { " (a proposal the driver trusts, no ask)" } else { "" };
    flow.record(name, "task-force", true, &format!("re-enter at {at}{how}: {note}"));
    keep_spent(flow.project(), flow.run.reentries + 1, flow.fixes.get());
    let _ = REENTRY.set((at.clone(), note, flow.project().to_path_buf(), false));
    flow.halt(format!("{reason}; re-entering at {at}"))
}

/// Where the task force's re-entries and repairs are kept between the driver's processes: a re-entry starts a new
/// one, and so does an operator's restart `--from` a step; the limit holds for the run, not for one process.
pub const SPENT: &str = "agg/private/task-force.json";

/// The re-entries and repairs a run starting as `run` has spent already: none for a run from the start (which
/// begins the count anew), else what the processes before it kept, and at least the `--reentries` it was given.
pub(crate) fn spent_before(project: &std::path::Path, run: &Run) -> (u32, u32) {
    if run.from.is_none() {
        keep_spent(project, run.reentries, 0);
        return (run.reentries, 0);
    }
    let kept: serde_json::Value = std::fs::read_to_string(project.join(SPENT)).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let count = |k: &str| kept.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
    (count("reentries").max(run.reentries), count("fixes"))
}

/// Keeps the run's count of re-entries and repairs for the next process of it.
fn keep_spent(project: &std::path::Path, reentries: u32, fixes: u32) {
    let file = project.join(SPENT);
    let _ = file.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(&file, serde_json::json!({ "reentries": reentries, "fixes": fixes }).to_string());
}

/// The task force's answer in `file`; a key that is missing is empty, and without `outcome` a step to re-enter means
/// `reenter`, none `escalate` (the answer before A-35).
fn answer(file: &std::path::Path) -> Answer {
    let parsed = std::fs::read_to_string(file).map_err(|e| e.to_string())
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).map_err(|e| e.to_string()));
    let field = |k: &str| parsed.as_ref().ok().and_then(|a| a.get(k)).and_then(|v| v.as_str())
        .unwrap_or_default().trim().to_string();
    let cause = match &parsed {
        Err(e) => format!("no answer in {}: {e}", file.display()),
        Ok(_) => field("cause"),
    };
    let at = field("reenter");
    let outcome = match field("outcome") {
        o if !o.is_empty() => o,
        _ if !at.is_empty() && at != "none" => "reenter".into(),
        _ => "escalate".into(),
    };
    Answer { outcome, at, cause, note: field("note") }
}

/// Runs a workflow driver (`agg::driver::run`), an error that ends it recorded in the history against the step
/// running then; when the task force sent it back, starts it again at the step it chose, with the note and one more
/// re-entry, keeping the call log the new run would replace.
pub fn run(drive: impl FnOnce() -> Result<(), Fatal>) -> ExitCode {
    let code = crate::driver::run(|| drive().inspect_err(|e| {
        if !matches!(e, Fatal::Ended(_)) {
            super::history::ended_by(&e.to_string());
        }
    }));
    let Some((at, note, project, decided)) = REENTRY.get() else {
        return code;
    };
    let calls = project.join("agg/private/calls.jsonl");
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let _ = std::fs::copy(&calls, calls.with_file_name(format!("calls.{secs}.jsonl")));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let again = Command::new(std::env::current_exe().unwrap_or_else(|_| "agg-driver".into()))
        .args(rerun(&args, at, note, *decided)).status();
    match again {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(_) => code,
    }
}

/// The arguments of the re-entered run: the driver's own, `--until` kept, the step and note replaced, and
/// `--decided` when the note is the humans' decision. Only the task force's re-entries count against its limit:
/// a decision's keeps the count, or approvals with decisions would use up the repairs of the steps after them.
fn rerun(args: &[String], at: &str, note: &str, decided: bool) -> Vec<String> {
    let reentries = Run::from_args(args).map(|r| r.reentries).unwrap_or(0) + u32::from(!decided);
    let mut kept = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" | "--note" | "--reentries" => { it.next(); }
            "--reset" | "--decided" => {}
            _ => kept.push(a.clone()),
        }
    }
    kept.extend(["--from", at, "--note", note, "--reentries"].map(String::from));
    kept.push(reentries.to_string());
    if decided {
        kept.push("--decided".into());
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A re-entered run keeps the driver's arguments and `--until`, and replaces step, note and count; a
    /// decision's re-entry says so and keeps the count, and the next one, the task force's, doesn't.
    #[test]
    fn a_reentered_run_replaces_the_step_and_counts_one_more() {
        let args: Vec<String> = ["proj", "--from", "4.1", "--note", "old", "--reset", "--until", "5.3", "--reentries", "1"]
            .map(String::from).to_vec();
        assert_eq!(rerun(&args, "2.5", "fix y", false),
                   ["proj", "--until", "5.3", "--from", "2.5", "--note", "fix y", "--reentries", "2"].map(String::from));
        let decided = rerun(&args, "4.2", "CP3 decided x", true);
        assert_eq!(decided.last().map(String::as_str), Some("--decided"));
        assert_eq!(Run::from_args(&decided).unwrap().reentries, 1);
        let again = rerun(&decided, "2.5", "fix y", false);
        assert!(!again.contains(&"--decided".to_string()));
        assert_eq!(Run::from_args(&again).unwrap().reentries, 2);
    }

    /// A driver's own error is recorded against the step running; an ended run's end is not an error.
    #[test]
    fn a_drivers_error_is_recorded_and_an_end_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let history = super::super::History::new(dir.path(), "steps.jsonl");
        history.at("3.2 Capture");
        let _ = run(|| Err(Fatal::Ended(crate::plugin::RunOutcome::Halt)));
        assert!(history.entries().is_empty());
        let _ = run(|| Err(Fatal::Other(anyhow::anyhow!("writing x: no folder"))));
        assert_eq!(history.entries()[0]["rationale"], "writing x: no folder");
    }

    /// A restart `--from` a step keeps the count of the processes before it; a run from the start begins anew.
    #[test]
    fn the_count_of_reentries_and_repairs_outlives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let from = |n: u32| Run { from: Some("6.3".into()), reentries: n, ..Run::default() };
        assert_eq!(spent_before(dir.path(), &from(0)), (0, 0));
        keep_spent(dir.path(), 2, 1);
        assert_eq!(spent_before(dir.path(), &from(0)), (2, 1), "an operator's restart without --reentries");
        assert_eq!(spent_before(dir.path(), &from(3)), (3, 1), "a re-entry's own count when it is higher");
        assert_eq!(spent_before(dir.path(), &Run::default()), (0, 0));
        assert_eq!(spent_before(dir.path(), &from(0)), (0, 0), "the run from the start began the count anew");
    }

    /// The answer's keys, or why there is none; without `outcome`, a step means `reenter` and none `escalate`.
    #[test]
    fn the_answer_is_read_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("task-force.yaml");
        let read = |text: &str| {
            std::fs::write(&file, text).unwrap();
            let a = answer(&file);
            (a.outcome, a.at, a.cause, a.note)
        };
        assert!(answer(&file).cause.starts_with("no answer in"));
        assert_eq!(answer(&file).outcome, "escalate");
        assert_eq!(read("reenter: '2.5'\ncause: bound the wrong JDK\nnote: bind Corretto 25\n"),
                   ("reenter".into(), "2.5".into(), "bound the wrong JDK".into(), "bind Corretto 25".into()));
        assert_eq!(read("reenter: none\ncause: the network\n").0, "escalate");
        assert_eq!(read("outcome: fixed\ncause: no parser\nnote: installed it\n").0, "fixed");
    }
}
