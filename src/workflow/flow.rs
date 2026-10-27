//! A workflow run (A-27, A-29): agg, plus where the run starts and ends, the steps it reached, its history,
//! the checks around every step, and a scratch folder outside the project for the files a step's brief names,
//! removed when the run ends.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use super::git::{before, commit, git};
use super::history::{History, DEFAULT};
use super::key;
use crate::core::model::Judge;
use crate::driver::{Agg, Fatal, Step};
use crate::plugin::RunOutcome;

/// Where a run starts and ends: `--from <step> [--note <text> [--decided]] [--reset] [--until <step>]
/// [--reentries <n>]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    /// the key of the step the run starts at; the steps before it are skipped.
    pub from: Option<String>,
    /// what a later step found wrong with the `from` step's result: it leads that step's brief.
    pub note: Option<String>,
    /// take back what the `from` step and every later step changed, instead of refreshing it.
    pub reset: bool,
    /// the key of the step after which the run ends, done.
    pub until: Option<String>,
    /// how often the task force already sent this run back.
    pub reentries: u32,
    /// the note is what humans decided at an approval, not a fault a later step found: the `from` step
    /// refreshes its result against it, and a result it doesn't concern stays unchanged.
    pub decided: bool,
}

impl Run {
    /// The run's flags among `args` (the program's arguments); other arguments are the driver's.
    pub fn from_args(args: &[String]) -> anyhow::Result<Run> {
        let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
        let run = Run { from: value("--from"), note: value("--note"), reset: args.iter().any(|a| a == "--reset"),
                        until: value("--until"),
                        reentries: value("--reentries").and_then(|n| n.parse().ok()).unwrap_or(0),
                        decided: args.iter().any(|a| a == "--decided") };
        if run.note.is_some() && run.from.is_none() {
            anyhow::bail!("--note needs --from");
        }
        if run.decided && run.note.is_none() {
            anyhow::bail!("--decided needs --note");
        }
        Ok(run)
    }

    /// The flags that take a value, so a driver can tell its own positional arguments from them.
    pub const VALUED: [&'static str; 4] = ["--from", "--note", "--until", "--reentries"];
}

/// When a step's checks run: before its work, or after it, before its own judges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Before,
    After,
}

type Hooks = Box<dyn Fn(&Flow, &str, When) -> Vec<Judge>>;
type OnStart = Box<dyn Fn(&Flow, &str) -> Result<(), String>>;

/// A rule over the task force's proposals: (the failed step, the step to re-enter) → go ahead without a human's yes.
pub(crate) type Trust = dyn Fn(&str, &str) -> bool;

/// A workflow run on agg: what every block of [`super`] works through.
pub struct Flow {
    agg: Agg,
    project: PathBuf,
    /// where the run starts and ends.
    pub run: Run,
    started: Cell<bool>,
    scratch: PathBuf,
    seen: RefCell<Vec<String>>,
    history: History,
    pub(crate) own: Vec<String>,
    hooks: Option<Hooks>,
    on_start: Option<OnStart>,
    pub(crate) task_force: Option<(Step, u32)>,
    /// how often the task force repaired a failure in place in this run; with the re-entries, within its limit.
    pub(crate) fixes: Cell<u32>,
    /// which of the task force's proposals go ahead without a human's yes: (the failed step, the step to re-enter).
    pub(crate) trusted: Option<Box<Trust>>,
}

impl Drop for Flow {
    /// Removes the run's scratch folder, however the run ended: what a step left there was for this run.
    fn drop(&mut self) {
        super::scratch::remove(&self.scratch);
    }
}

impl Deref for Flow {
    type Target = Agg;
    fn deref(&self) -> &Agg {
        &self.agg
    }
}

impl Flow {
    /// A run of `agg` in its project, starting and ending as `run` says.
    pub fn new(agg: Agg, run: Run) -> Flow {
        let project = agg.dir().to_path_buf();
        let scratch = super::scratch::folder(&std::env::temp_dir());
        super::scratch::sweep(&std::env::temp_dir());
        Flow { started: Cell::new(run.from.is_none()), history: History::new(&project, DEFAULT), project, run,
               scratch, seen: RefCell::new(vec![]), own: vec![], hooks: None, on_start: None, task_force: None,
               fixes: Cell::new(0), trusted: None, agg }
    }

    /// The history in `path` (relative to the project) instead of `agg/private/steps.jsonl`.
    pub fn history(mut self, path: impl AsRef<Path>) -> Flow {
        self.history = History::new(&self.project, path);
        self
    }

    /// Files a `--reset` never takes back: the humans' own, the run's records.
    pub fn own<I: IntoIterator<Item = S>, S: Into<String>>(mut self, paths: I) -> Flow {
        self.own.extend(paths.into_iter().map(Into::into));
        self
    }

    /// The checks around every step: `f(flow, step, when)`; each verdict is recorded (kind `hook`), and an
    /// unmet one fails the step (before: without its work; after: before its own judges read the result).
    pub fn hooks(mut self, f: impl Fn(&Flow, &str, When) -> Vec<Judge> + 'static) -> Flow {
        self.hooks = Some(Box::new(f));
        self
    }

    /// What the driver does when a step starts (e.g. checks between phases); an error fails the step.
    pub fn on_start(mut self, f: impl Fn(&Flow, &str) -> Result<(), String> + 'static) -> Flow {
        self.on_start = Some(Box::new(f));
        self
    }

    /// The project folder.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The run's history.
    pub fn steps(&self) -> &History {
        &self.history
    }

    /// The keys of the steps this run reached, in order.
    pub fn seen(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }

    /// Records one event of step `name` (kind `done`, `tool`, `skipped`, …), written at the next boundary.
    pub fn record(&self, name: &str, kind: &str, met: bool, rationale: &str) {
        self.history.defer(name, kind, met, rationale);
    }

    /// Writes the history's pending entries and commits it.
    pub fn flush(&self) -> Result<(), Fatal> {
        self.history.flush()?;
        Ok(self.history.commit()?)
    }

    /// Whether step `name` runs: the run reached its `from` step and not past its `until` step. A step that
    /// runs marks where the base branch stands (`refs/agg/before/<key>`), so a later `--reset` can go back.
    pub fn runs(&self, name: &str) -> Result<bool, Fatal> {
        if let Some(until) = &self.run.until {
            if self.seen.borrow().contains(until) && key(name) != until {
                self.flush()?;
                self.agg.finish_with(RunOutcome::GoalsMet, format!("done up to {until}, as asked (--until)"));
                return Err(Fatal::Ended(RunOutcome::GoalsMet));
            }
        }
        self.seen.borrow_mut().push(key(name).to_string());
        if self.run.from.as_deref() == Some(key(name)) {
            self.started.set(true);
        }
        if !self.started.get() {
            return Ok(false);
        }
        // a pending entry would count as the next session's change, and a refresh would never be unchanged
        self.flush()?;
        self.history.at(name);
        git(&self.project, &["update-ref", &before(key(name)), "HEAD"])?;
        if let Some(Err(why)) = self.on_start.as_ref().map(|f| f(self, name)) {
            self.recover(name, &why, &|| Ok(self.on_start.as_ref().and_then(|f| f(self, name).err())))?;
        }
        Ok(true)
    }

    /// Runs the hooks of step `name` (`when`), recording each verdict; the unmet ones' rationales joined.
    pub fn check(&self, name: &str, when: When) -> Result<(), String> {
        let which = if when == When::Before { "before" } else { "after" };
        let judges = self.hook_judges(name, when);
        let failed: Vec<String> = judges.iter().filter_map(|j| {
            let verdict = self.agg.judge(j);
            self.record(name, "hook", verdict.met(), &format!("{which} {}: {}", j.name, verdict.rationale()));
            (!verdict.met()).then(|| format!("{}: {}", j.name, verdict.rationale()))
        }).collect();
        if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
    }

    /// The hooks of step `name` (`when`), as the driver gives them.
    pub(crate) fn hook_judges(&self, name: &str, when: When) -> Vec<Judge> {
        self.hooks.as_ref().map(|f| f(self, name, when)).unwrap_or_default()
    }

    /// The step with its prompt file as agg resolves it (relative: under the project's `agg/`), so the blocks
    /// can read and extend its task.
    pub(crate) fn absolute(&self, step: &Step) -> Step {
        match &step.prompt_file {
            Some(f) if Path::new(f).is_relative() => {
                step.clone().prompt_file(self.project.join("agg").join(f).display().to_string())
            }
            _ => step.clone(),
        }
    }

    /// The rubric judge with its file as agg resolves it (relative: under the project).
    pub(crate) fn absolute_judge(&self, judge: &Judge) -> Judge {
        let mut judge = judge.clone();
        if let crate::core::model::JudgeKind::Llm { path, .. } = &mut judge.kind {
            if path.is_relative() {
                *path = self.project.join(&*path);
            }
        }
        judge
    }

    /// The step failed: the task force looks for its origin when the run has one (A-30), else the run ends.
    pub fn fail(&self, name: &str, why: &str) -> Result<(), Fatal> {
        super::task_force::fail(self, name, why, None)
    }

    /// The step failed, and `again` judges it again (`None`: met now, else why not): the task force may repair the
    /// cause in place (A-35), and when the step is met then, this returns and the step goes on. Otherwise as
    /// [`Flow::fail`].
    pub fn recover(&self, name: &str, why: &str, again: super::task_force::Again) -> Result<(), Fatal> {
        super::task_force::fail(self, name, why, Some(again))
    }

    /// Ends the run with `reason`.
    pub fn halt(&self, reason: impl Into<String>) -> Result<(), Fatal> {
        let _ = self.flush();
        self.agg.finish_with(RunOutcome::Halt, reason);
        Err(Fatal::Ended(RunOutcome::Halt))
    }

    /// A fresh path for `file` in the run's own folder outside the project: files a brief names.
    pub fn scratch(&self, file: &str) -> Result<PathBuf, Fatal> {
        std::fs::create_dir_all(&self.scratch).map_err(|e| anyhow!("creating {}: {e}", self.scratch.display()))?;
        let path = self.scratch.join(file);
        let _ = std::fs::remove_file(&path);
        Ok(path)
    }

    /// An executable script `file` in the scratch folder that runs `command` with its arguments: how a
    /// script judge gets a parameter.
    pub fn wrapper(&self, file: &str, command: &str) -> Result<String, Fatal> {
        let path = self.scratch(file)?;
        std::fs::write(&path, format!("#!/bin/sh\nexec {command} \"$@\"\n"))
            .map_err(|e| anyhow!("writing {}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| anyhow!("making {} executable: {e}", path.display()))?;
        }
        Ok(path.display().to_string())
    }

    /// A script step (A-27): `script` does the work and gives the verdict. Met, what it wrote is committed and
    /// the step recorded (kind `tool`); unmet, the step fails, and after the task force's repair in place the script
    /// runs again.
    pub fn script_step(&self, name: &str, script: &Judge) -> Result<(), Fatal> {
        if !self.runs(name)? {
            return Ok(());
        }
        if let Err(why) = self.check(name, When::Before) {
            self.recover(name, &why, &|| Ok(self.check(name, When::Before).err()))?;
        }
        let said = RefCell::new(String::new());
        let attempt = || {
            let verdict = self.agg.judge(script);
            said.replace(verdict.rationale().to_string());
            if verdict.met() { self.check(name, When::After).err() } else { Some(verdict.rationale().into()) }
        };
        if let Some(why) = attempt() {
            self.record(name, "tool", false, &why);
            self.recover(name, &why, &|| Ok(attempt()))?;
        }
        commit(&self.project, &["."], name)?;
        self.record(name, "tool", true, &said.borrow());
        self.flush()
    }

    /// Whether step `name` applies (A-27): `when` (a script) is met. One that doesn't is recorded as skipped
    /// with the reason and costs no session; a condition that errs fails the step instead of skipping it. The
    /// condition is asked as one ([`Agg::condition`]): its flip between runs is no regression of the work.
    pub fn applies(&self, name: &str, when: &Judge) -> Result<bool, Fatal> {
        if !self.runs(name)? {
            return Ok(false);
        }
        let verdict = self.agg.condition(when);
        if let Some(error) = verdict.error() {
            self.fail(name, &format!("whether it applies can't be told: {error}"))?;
            return Ok(false);
        }
        if !verdict.met() {
            self.agg.info(&format!("{name}: skipped, it doesn't apply: {}", verdict.rationale()));
            self.record(name, "skipped", true, &format!("it doesn't apply: {}", verdict.rationale()));
        }
        Ok(verdict.met())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run's flags come from the arguments; a note needs a step to go to.
    #[test]
    fn the_run_reads_its_flags_and_leaves_the_rest() {
        let args: Vec<String> = ["proj", "--from", "2.5", "--note", "fix x", "--until", "2.13", "--reentries", "2"]
            .map(String::from).to_vec();
        let run = Run::from_args(&args).unwrap();
        assert_eq!((run.from.as_deref(), run.note.as_deref(), run.until.as_deref(), run.reentries, run.reset),
                   (Some("2.5"), Some("fix x"), Some("2.13"), 2, false));
        assert!(Run::from_args(&["--note".into(), "x".into()]).is_err());
        assert_eq!(Run::from_args(&["proj".into()]).unwrap(), Run::default());
    }
}
