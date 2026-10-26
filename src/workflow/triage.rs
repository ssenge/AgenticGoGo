//! Triage on failure (A-30): when a step fails, an agent finds the earliest step whose result is wrong and
//! proposes to go back there with a note; after a human's yes the run ends and [`run`] starts it again from
//! there (A-29).

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::OnceLock;

use super::brief::with_section;
use super::flow::{Flow, Run};
use crate::driver::{Fatal, Step};

/// Where the next run re-enters, its note, and the project: set by triage, read by [`run`] after the run.
static REENTRY: OnceLock<(String, String, PathBuf)> = OnceLock::new();

impl Flow {
    /// On a failed step, `step` (an agent) looks for the failure's origin, up to `max` re-entries per run.
    pub fn on_failure(mut self, step: Step, max: u32) -> Flow {
        self.triage = Some((step, max));
        self
    }

    /// Triage's proposals that `rule` (the failed step, the step to re-enter) accepts go ahead without a
    /// human's yes, still within the run's re-entries: for a failure the driver knows is the re-entered step's
    /// own to repair (a check of a step's script, sent back to the step that writes it).
    pub fn trust(mut self, rule: impl Fn(&str, &str) -> bool + 'static) -> Flow {
        self.trusted = Some(Box::new(rule));
        self
    }

    /// Ends this run and starts it again at step `at` (one this run has seen) with `note`, as after a triage
    /// a human said yes to; it counts as a re-entry. For a driver that knows itself where the run must go
    /// back: a decision taken at an approval that an earlier step's result doesn't carry yet.
    pub fn reenter(&self, at: &str, note: &str) -> Result<(), Fatal> {
        if !self.seen().iter().any(|s| s == at) {
            return Err(Fatal::Other(anyhow::anyhow!("re-entering at {at}: no step of this run")));
        }
        self.record(at, "reentry", true, note);
        let _ = REENTRY.set((at.to_string(), note.to_string(), self.project().to_path_buf()));
        self.halt(format!("re-entering at {at}: {note}"))
    }
}

/// Step `name` failed for `why`: triage proposes a re-entry and a human decides; without triage, after the
/// last re-entry, or without a usable proposal, the run ends.
pub(crate) fn fail(flow: &Flow, name: &str, why: &str) -> Result<(), Fatal> {
    let reason = format!("{name}: {why}");
    let Some((triage, _)) = flow.triage.as_ref().filter(|(_, max)| flow.run.reentries < *max) else {
        return flow.halt(reason);
    };
    let file = flow.scratch("triage.yaml")?;
    let step = with_section(&flow.absolute(triage), &format!(
        "## The failure\n\nStep {name} failed:\n\n{why}\n\n## The steps of this run, in order\n\n{}\n\nThis run \
         was re-entered {} times already.\n\n## Your answer\n\nWrite it to {} as YAML with three keys: `reenter`, \
         the step (as listed above) whose result must change, or `none`; `cause`, in one or two sentences what \
         went wrong where; `note`, what that step's worker must fix.",
        flow.seen().join(", "), flow.run.reentries, file.display()))?;
    flow.flush()?;
    flow.step(&step)?;
    flow.gate()?;
    let (at, cause, note) = answer(&file);
    if at.is_empty() || at == "none" || note.is_empty() || !flow.seen().contains(&at) {
        flow.record(name, "triage", false, &format!("no re-entry: {cause}"));
        return flow.halt(format!("{reason} (triage: {cause})"));
    }
    // an approval takes no note: what is decided there is the humans' own, never an agent's proposal
    if flow.steps().is_approval(&at) {
        flow.record(name, "triage", false, &format!("humans decide at {at}: {cause}"));
        return flow.halt(format!("{reason} (triage: humans decide at {at}: {cause})"));
    }
    let trusted = flow.trusted.as_ref().is_some_and(|rule| rule(name, &at));
    if !trusted && !flow.hil_bool(&format!("{reason}\n\nTriage: {cause}\nRe-enter at {at}, with the note: {note}\nGo?"))? {
        return flow.halt(reason);
    }
    let how = if trusted { " (a proposal the driver trusts, no ask)" } else { "" };
    flow.record(name, "triage", true, &format!("re-enter at {at}{how}: {note}"));
    let _ = REENTRY.set((at.clone(), note, flow.project().to_path_buf()));
    flow.halt(format!("{reason}; re-entering at {at}"))
}

/// The triage's `reenter`, `cause` and `note`; empty where the file or a key is missing.
fn answer(file: &std::path::Path) -> (String, String, String) {
    let parsed = std::fs::read_to_string(file).map_err(|e| e.to_string())
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).map_err(|e| e.to_string()));
    let field = |k: &str| parsed.as_ref().ok().and_then(|a| a.get(k)).and_then(|v| v.as_str())
        .unwrap_or_default().trim().to_string();
    let cause = match &parsed {
        Err(e) => format!("no answer in {}: {e}", file.display()),
        Ok(_) => field("cause"),
    };
    (field("reenter"), cause, field("note"))
}

/// Runs a workflow driver (`agg::driver::run`); when triage sent it back, starts it again at the step it
/// chose, with the note and one more re-entry, keeping the call log the new run would replace.
pub fn run(drive: impl FnOnce() -> Result<(), Fatal>) -> ExitCode {
    let code = crate::driver::run(drive);
    let Some((at, note, project)) = REENTRY.get() else {
        return code;
    };
    let calls = project.join("agg/private/calls.jsonl");
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let _ = std::fs::copy(&calls, calls.with_file_name(format!("calls.{secs}.jsonl")));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let again = Command::new(std::env::current_exe().unwrap_or_else(|_| "agg-driver".into()))
        .args(reentered(&args, at, note)).status();
    match again {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(_) => code,
    }
}

/// The arguments of the re-entered run: the driver's own, `--until` kept, the step and note replaced.
fn reentered(args: &[String], at: &str, note: &str) -> Vec<String> {
    let reentries = Run::from_args(args).map(|r| r.reentries).unwrap_or(0) + 1;
    let mut kept = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" | "--note" | "--reentries" => { it.next(); }
            "--reset" => {}
            _ => kept.push(a.clone()),
        }
    }
    kept.extend(["--from", at, "--note", note, "--reentries"].map(String::from));
    kept.push(reentries.to_string());
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A re-entered run keeps the driver's arguments and `--until`, and replaces step, note and count.
    #[test]
    fn a_reentered_run_replaces_the_step_and_counts_one_more() {
        let args: Vec<String> = ["proj", "--from", "4.1", "--note", "old", "--reset", "--until", "5.3", "--reentries", "1"]
            .map(String::from).to_vec();
        assert_eq!(reentered(&args, "2.5", "fix y"),
                   ["proj", "--until", "5.3", "--from", "2.5", "--note", "fix y", "--reentries", "2"].map(String::from));
    }

    /// The answer's keys, or why there is none.
    #[test]
    fn the_answer_is_read_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("triage.yaml");
        assert!(answer(&file).1.starts_with("no answer in"));
        std::fs::write(&file, "reenter: '2.5'\ncause: bound the wrong JDK\nnote: bind Corretto 25\n").unwrap();
        assert_eq!(answer(&file), ("2.5".into(), "bound the wrong JDK".into(), "bind Corretto 25".into()));
    }
}
