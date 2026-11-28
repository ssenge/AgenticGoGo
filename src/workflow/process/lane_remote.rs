//! A lane that runs elsewhere than a thread of this process (`parallel.target`): the run prepares the lane's clone (the project
//! at the base commit, the seed, the workers' memory, a copy of the process's home, the lane's spec, the variables it passes),
//! the target's commands start it (`rlt lane --spec`), poll it and bring the clone back, and its commits are fetched into the
//! run's repository for the join.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::format::{Target, TargetKind};
use super::lane_files as files;
use super::lane_main::{LaneResult, LaneSpec, RunFlags, RESULT, SPEC};
use super::lanes::{Ended, Env, Job};
use crate::lanes::services::Shared;
use crate::lanes::target::{self, Commands, Ssh};
use crate::lanes::worktree::{self, Lane};

/// The lane's clone, its spec and its variables, ready to ship; what the run takes back at the join.
fn prepare(env: &Env, lane: &Lane, job: &Job, target: &Target) -> Result<Prepared, String> {
    let existed = lane.exists();
    worktree::add_clone(env.project, lane, env.base)?;
    let private = crate::paths::private_dir(&lane.dir);
    let (_, redirects) = files::redirected(env.spec, env.project, &lane.dir);
    if !existed {
        worktree::seed(env.project, &lane.dir, env.seed);
        files::seed_state(env.project, &lane.dir);
        worktree::copy_dir(env.home, &private.join("lane-home"));
    }
    let process = process_file_name(env.project).ok_or("the project noted no process file (rlt/private/process.json)")?;
    let spec = LaneSpec { process, home: "rlt/private/lane-home".into(), each: env.step.id.clone(), key: job.key.clone(),
                          vars: job.item.as_ref().map(|i| i.vars.clone()).unwrap_or_default(), last: job.item.as_ref().is_some_and(|i| i.last),
                          has_item: job.item.is_some(), only: job.only.clone(), resume: env.resume && existed, run: RunFlags::of(env.run), phase: env.phase.clone(),
                          project: lane.project.clone() };
    std::fs::create_dir_all(&private).map_err(|e| e.to_string())?;
    std::fs::write(lane.dir.join(SPEC), serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if target.kind == TargetKind::Ssh {
        write_env_file(&private.join("lane.env"), &target.pass_env)?;
    }
    Ok(Prepared { redirects, state_seed: files::state_lengths_kept(env.project, &lane.dir) })
}

struct Prepared {
    redirects: Vec<files::Redirect>,
    state_seed: Vec<(std::path::PathBuf, u64)>,
}

/// The name of the process file the project's run noted (`rlt/private/process.json`).
fn process_file_name(project: &Path) -> Option<String> {
    let note: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::paths::private_dir(project).join("process.json")).ok()?).ok()?;
    Path::new(note["file"].as_str()?).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The passed variables as `export NAME='value'` lines in a file only its owner can read.
fn write_env_file(file: &Path, names: &[String]) -> Result<(), String> {
    let text: String = names.iter().filter_map(|n| Some(format!("export {n}={}\n", target::quote(&std::env::var(n).ok()?)))).collect();
    std::fs::write(file, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::set_permissions(file, std::os::unix::fs::PermissionsExt::from_mode(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

/// The commands of `target` for `lane`.
fn commands(target: &Target, lane: &Lane) -> Result<Commands, String> {
    let need = |v: &Option<String>, key: &str| v.clone().ok_or_else(|| format!("target.{key} is missing"));
    Ok(match target.kind {
        TargetKind::Ssh => Ssh { host: need(&target.host, "host")?, user: target.user.clone(), dir: need(&target.dir, "dir")?,
                                 rlt: target.rlt.clone().unwrap_or_else(|| "rlt".into()), options: target.ssh_options.clone() }.commands(lane),
        TargetKind::Container => {
            let image = target.image.clone().unwrap_or_else(|| crate::images::tag("claude"));
            if let Some(engine) = crate::isolation::container_engine() {
                if let Some(hint) = crate::images::missing_hint(engine, &image) {
                    return Err(hint);
                }
            }
            target::container_commands(&image, target.network.as_deref(), &target.pass_env, lane)
        }
        TargetKind::Command => Commands { launch: need(&target.launch, "launch")?, poll: need(&target.poll, "poll")?, kill: target.kill.clone(),
                                          fetch: need(&target.fetch, "fetch")? },
        TargetKind::Local => return Err("a local lane has no target commands".into()),
    })
}

/// Runs the lane on `target`: the clone prepared, the commands supervised, its commits fetched; how it ended.
pub(super) fn run(env: &Env, lane: &Lane, job: &Job, services: &Arc<Shared>, target: &Target) -> Ended {
    let failed = |why: String| Ended { result: Err(why), stopped: false, reentry: None, spent: false, redirects: vec![], state_seed: vec![] };
    let prepared = match prepare(env, lane, job, target) {
        Ok(p) => p,
        Err(e) => return failed(e),
    };
    let cmds = match commands(target, lane) {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let every = Duration::from_millis(std::env::var("RLT_LANE_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000));
    let ended = match target::supervise(&cmds, lane, env.project, services, every) {
        Ok(e) => e,
        Err(e) => return failed(e),
    };
    let _ = std::fs::remove_file(crate::paths::private_dir(&lane.dir).join("lane.env"));
    let result: LaneResult = std::fs::read_to_string(lane.dir.join(RESULT)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| {
        LaneResult { error: Some(if ended == target::Ended::Killed { "stopped".into() } else { "the lane left no result".into() }), ..LaneResult::default() }
    });
    let stopped = result.stopped || ended == target::Ended::Killed;
    if result.ok {
        if let Err(e) = worktree::fetch_back(env.project, lane) {
            return failed(e);
        }
    }
    Ended { result: if result.ok { Ok(()) } else { Err(result.error.unwrap_or_else(|| "the lane failed".into())) }, stopped,
            reentry: result.reentry, spent: result.spent, redirects: prepared.redirects, state_seed: prepared.state_seed }
}
