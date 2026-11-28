//! `rlt lane --spec <file>`: one lane of a parallel `each`, run by a process of its own in the lane's clone, wherever a target
//! started it. The spec says which process file (a copy of the process's home travels with the lane), which `each`, which item and
//! how the run stood; the lane is what a thread of the run would have done, and its end is `rlt/private/lane-result.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use super::exec::Item;
use super::format::{Process, Step};
use super::lane_files as files;
use super::lanes::{self, Env};
use super::number::number;
use super::{run, template};
use crate::driver::Fatal;
use crate::lanes::services::{Limits, Shared};
use crate::lanes::worktree::Lane;
use crate::plugin::RunOutcome;
use crate::workflow::Run;

/// Where a lane's spec is, relative to its project folder.
pub const SPEC: &str = "rlt/private/lane-spec.json";
/// Where a lane leaves its end.
pub const RESULT: &str = "rlt/private/lane-result.json";

/// The run's flags, as a lane takes them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunFlags {
    pub from: Option<String>,
    pub note: Option<String>,
    pub reset: bool,
    pub until: Option<String>,
    pub reentries: u32,
    pub decided: bool,
}

impl RunFlags {
    pub fn of(run: &Run) -> RunFlags {
        RunFlags { from: run.from.clone(), note: run.note.clone(), reset: run.reset, until: run.until.clone(), reentries: run.reentries, decided: run.decided }
    }

    fn run(&self) -> Run {
        Run { from: self.from.clone(), note: self.note.clone(), reset: self.reset, until: self.until.clone(), reentries: self.reentries, decided: self.decided }
    }
}

/// What a lane needs to know of its run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneSpec {
    /// the process file's name in `home`
    pub process: String,
    /// a copy of the process's home, relative to the lane's folder
    pub home: String,
    pub each: String,
    pub key: String,
    /// the item's variables (`item`, `item.yaml`, `item.<field>`)
    pub vars: BTreeMap<String, String>,
    pub last: bool,
    /// whether the lane has an item (a `kind: parallel` step's lane has one only inside an `each`)
    #[serde(default = "yes")]
    pub has_item: bool,
    /// the id of the one child of the step this lane runs; all of them when none
    #[serde(default)]
    pub only: Option<String>,
    pub resume: bool,
    pub run: RunFlags,
    /// the phase the run was in at the `each`
    pub phase: Option<String>,
    /// the project name the lane's engine instance runs as
    pub project: String,
}

fn yes() -> bool {
    true
}

/// How a lane ended, as the run reads it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaneResult {
    pub ok: bool,
    pub error: Option<String>,
    /// a stop ended it (resumable)
    pub stopped: bool,
    /// the task force's request to re-enter: the step, its note, the project, whether the note is a decision
    pub reentry: Option<(String, String, PathBuf, bool)>,
    pub spent: bool,
}

/// Finds the `each` step `id` among the phases' steps.
fn find<'a>(steps: &'a [Step], id: &str) -> Option<&'a Step> {
    steps.iter().find_map(|s| if s.id == id { Some(s) } else { find(&s.steps, id) })
}

/// Runs the lane `spec` names, in the current folder; its result is written to [`RESULT`]. Exit 0 when the lane ended done.
pub fn command(spec_file: &Path) -> ExitCode {
    let result = match lane(spec_file) {
        Ok(r) => r,
        Err(e) => LaneResult { error: Some(format!("{e:#}")), ..LaneResult::default() },
    };
    let dir = std::env::current_dir().unwrap_or_default();
    let file = dir.join(RESULT);
    let _ = file.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(&file, serde_json::to_string_pretty(&result).unwrap_or_default());
    if let Some(why) = &result.error {
        eprintln!("lane: {why}");
    }
    if result.ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn lane(spec_file: &Path) -> anyhow::Result<LaneResult> {
    let spec: LaneSpec = serde_json::from_str(&std::fs::read_to_string(spec_file).map_err(|e| anyhow!("{}: {e}", spec_file.display()))?)?;
    let dir = std::env::current_dir()?.canonicalize()?;
    let home = dir.join(&spec.home);
    let process = Process::parse(&std::fs::read_to_string(home.join(&spec.process)).map_err(|e| anyhow!("{}: {e}", spec.process))?)?;
    let list = number(&process);
    for (name, command) in &process.rlt.agents {
        crate::backend::register_command(name, command.clone())?;
    }
    let vars = template::variables(&process, &list, &home, &dir.display().to_string()).map_err(|e| anyhow!("{e}"))?;
    let settings = run::settings_of(&process, &list, &home, &dir, &vars).map_err(|e| anyhow!("{e}"))?;
    let roles = run::roles_of(&process, &dir).map_err(|e| anyhow!("{e}"))?;
    let step = process.phases.iter().find_map(|p| find(&p.steps, &spec.each)).ok_or_else(|| anyhow!("no step `{}` in the process", spec.each))?;
    let run_flags = spec.run.run();
    let local = super::format::Target::default();
    let env = Env { step, spec: &process, list: &list, home: &home, project: &dir, settings: &settings, vars: &vars, roles: roles.as_ref(),
                    run: &run_flags, resume: spec.resume, phase: spec.phase.clone(), base: "", seed: &[], target: &local };
    let lane = Lane { key: spec.key.clone(), each: spec.each.clone(), worktree: dir.clone(), dir: dir.clone(), branch: String::new(), project: spec.project.clone() };
    let item = spec.has_item.then(|| Item { key: spec.key.clone(), vars: spec.vars.clone(), last: spec.last });
    let job = lanes::Job { key: spec.key.clone(), item, only: spec.only.clone() };
    let (lane_spec, _) = files::redirected(&process, &dir, &dir);
    let services = Shared::new(Limits::default());
    let outcome = lanes::steps(&env, &lane, &job, &lane_spec, (&services, spec.resume));
    let (reentry, spent) = crate::workflow::task_force::take_requests();
    Ok(match outcome {
        Ok(()) => LaneResult { ok: true, reentry, spent, ..LaneResult::default() },
        Err(e) => LaneResult { ok: false, stopped: matches!(e, Fatal::Ended(RunOutcome::Stopped | RunOutcome::Disk)), error: Some(e.to_string()), reentry, spent },
    })
}
