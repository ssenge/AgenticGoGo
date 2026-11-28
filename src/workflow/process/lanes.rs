//! A parallel `each` (`parallel: { max, fail_fast, seed }`): its items run at once as lanes, each in a linked worktree of
//! its own on a thread of its own with the run's engine instance for its steps, and the finished lanes are joined into the
//! run's branch in item order.
//!
//! The run waits here while its lanes work: about every second it forwards the operator's commands to them, mirrors their
//! asks, and keeps their views. A lane that fails keeps its worktree for the diagnosis and the resume; the lanes after it
//! are not joined yet, so that the join stays in item order.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::anyhow;

use super::exec::Item;
use super::format::{Parallel, Process, Step};
use super::lane_files as files;
use super::number::Numbered;
use super::proc::{Init, Proc};
use super::settings::Settings;
use super::{run, standing};
use crate::driver::{Fatal, Opts, Relentless};
use crate::lanes::forward::Forwarder;
use crate::lanes::join::{self, Joined};
use crate::lanes::record::{forget, note, recorded};
use crate::lanes::schedule::{self, Plan, Reading};
use crate::lanes::services::{Limits, LaneServices, LaneState, LaneView, Shared};
use crate::lanes::worktree::{self, Lane};
use crate::plugin::RunOutcome;
use crate::workflow::{Roles, Run};

/// What a lane ends with.
pub(super) struct Ended {
    /// the lane's steps all ended met
    pub(super) result: Result<(), String>,
    /// a stop ended it (resumable), not a failure
    pub(super) stopped: bool,
    pub(super) reentry: Option<crate::workflow::task_force::Reentry>,
    pub(super) spent: bool,
    pub(super) redirects: Vec<files::Redirect>,
    pub(super) state_seed: Vec<(std::path::PathBuf, u64)>,
}

/// What every lane's thread reads of the run.
pub(super) struct Env<'a> {
    pub(super) step: &'a Step,
    pub(super) spec: &'a Process,
    pub(super) list: &'a [Numbered],
    pub(super) home: &'a Path,
    pub(super) project: &'a Path,
    pub(super) settings: &'a Settings,
    pub(super) vars: &'a BTreeMap<String, String>,
    pub(super) roles: Option<&'a Roles>,
    pub(super) run: &'a Run,
    pub(super) resume: bool,
    pub(super) phase: Option<String>,
    pub(super) base: &'a str,
    pub(super) seed: &'a [String],
    pub(super) target: &'a super::format::Target,
}

/// Runs the items of `step` (an `each` with `parallel`) as lanes, then joins them in item order.
pub fn each(p: &Proc, step: &Step, items: Vec<Item>, par: &Parallel) -> Result<(), Fatal> {
    let keys: Vec<String> = items.iter().map(|i| i.key.clone()).collect();
    if let Some(clash) = crate::lanes::collisions(&keys).first() {
        return Err(anyhow!("step `{}`: item keys that become one name in a lane's folder and branch: {clash}", step.id).into());
    }
    let project = p.project.clone();
    if worktree::toplevel(&project).is_none() {
        return Err(anyhow!("step `{}`: a parallel `each` needs the project in a git repository: a lane is a worktree of it", step.id).into());
    }
    // the run boots here (its guard, its bus): the operator's stop and pause can reach it while the lanes work
    p.check_limits()?;
    let resume = p.resumes();
    let joined = if resume { recorded(&project, &step.id) } else { forget(&project, &step.id) };
    if !resume && !crate::git::is_clean(&project) {
        return Err(anyhow!("step `{}`: the project has uncommitted changes, which the lanes' worktrees would not hold: commit them first", step.id).into());
    }
    // the ignore rules are committed here, as the first session would: otherwise every lane branch adds the file and the
    // join meets the run's own untracked copy
    crate::git::ensure_rlt_gitignored(&project);
    crate::workflow::commit(&project, &[".gitignore"], "Ignore rlt's runtime folders").map_err(Fatal::Other)?;
    let base = crate::git::git(&project, &["rev-parse", "HEAD"]).1;
    let name = project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let lanes: Vec<Lane> = keys.iter().map(|k| Lane::of(&project, &name, &step.id, k)).collect();
    let todo: Vec<usize> = (0..keys.len()).filter(|i| !joined.contains(&keys[*i])).collect();
    if !resume {
        // lanes a run before this one left are not this run's: each starts fresh from the run's HEAD
        lanes.iter().filter(|l| l.exists()).for_each(|l| worktree::remove(&project, l));
    }
    let env = Env { step, spec: &p.shared.spec, list: &p.list, home: &p.home, project: &project, settings: &p.settings,
                    vars: &p.shared.vars, roles: p.roles.as_ref(), run: &p.run, resume, phase: p.phase_now(), base: &base,
                    seed: &par.seed, target: &par.target };
    let services = Shared::new(Limits::default());
    let plan = Plan { keys: todo.iter().map(|i| keys[*i].clone()).collect(), max: par.max, fail_fast: par.fail_fast };
    let forwarder = Forwarder::new(&project, &lanes, &todo, &services);
    p.info(&format!("{}: {} item(s) as lanes, {} at once", step.id, todo.len(), par.max));
    let results = schedule::run(
        &plan,
        &services,
        &|running| forwarder.tick(running.iter().map(|r| todo[*r]).collect::<Vec<_>>().as_slice()),
        &|n, _key| lane_body(&env, &lanes[todo[n]], &items[todo[n]], &services),
        &Reading { failed: &|e: &Ended| e.result.is_err(),
                   panicked: &|why| Ended { result: Err(format!("panic: {why}")), stopped: false, reentry: None, spent: false,
                                            redirects: vec![], state_seed: vec![] } },
    );
    forwarder.tick(&[]);
    finish(p, step, (&lanes, &todo), results, &services, &forwarder)
}

/// Joins the lanes in item order until the first that isn't done, records them, and ends the `each` as the lanes did.
fn finish(p: &Proc, step: &Step, (lanes, todo): (&[Lane], &[usize]), results: Vec<Option<Ended>>, services: &Arc<Shared>, forward: &Forwarder)
          -> Result<(), Fatal> {
    let (mut clean, mut failure, mut reentry, mut spent) = (true, None::<String>, None::<crate::workflow::task_force::Reentry>, false);
    for (n, result) in results.into_iter().enumerate() {
        let lane = &lanes[todo[n]];
        let Some(ended) = result else {
            clean = false;
            p.record(&format!("{}:{}", step.id, lane.key), "lane", false, "not started: the run was stopped or a lane failed first");
            continue;
        };
        spent |= ended.spent;
        if let Some(r) = ended.reentry.clone() {
            // the earliest step in process order any lane asked for; a later request names the lane that made it
            reentry = Some(reentry.map_or(r.clone(), |held| earlier(p, held, r)));
        }
        match (&ended.result, clean) {
            (Ok(()), true) => match join_one(p, step, lane, &ended) {
                Ok(()) => {}
                Err(why) => {
                    clean = false;
                    failure.get_or_insert(why);
                }
            },
            (Ok(()), false) => p.record(&format!("{}:{}", step.id, lane.key), "lane", true, "done; its join waits for the lane before it"),
            (Err(why), _) => {
                clean = false;
                let state = if ended.stopped { "stopped" } else { "failed" };
                p.record(&format!("{}:{}", step.id, lane.key), "lane", false, &format!("{state}: {why}"));
                if !ended.stopped {
                    failure.get_or_insert(format!("lane `{}` failed: {why}", lane.key));
                }
            }
        }
    }
    forward.finished();
    p.flush()?;
    if let Some(r) = reentry {
        crate::workflow::task_force::ask_reentry_from_lanes(r);
    }
    if spent {
        crate::workflow::task_force::limit_spent_from_lanes();
    }
    if let Some(why) = services.stop_reason() {
        if services.ceiling_hit().is_some() {
            return p.halt(format!("{}: {why}", step.id));
        }
        // the operator's stop reaches the run's own bus again, where the run ends the way a stop ends it
        if forward.requeue_stop(&why) {
            p.check_limits()?;
        }
    }
    match failure {
        Some(why) => p.halt(format!("{}: {why}", step.id)),
        None => Ok(()),
    }
}

/// Of two re-entry requests, the one whose step comes first in the process.
fn earlier(p: &Proc, a: crate::workflow::task_force::Reentry, b: crate::workflow::task_force::Reentry) -> crate::workflow::task_force::Reentry {
    let at = |step: &str| p.list.iter().position(|s| s.id == step || s.number.as_deref() == Some(step)).unwrap_or(usize::MAX);
    if at(&b.0) < at(&a.0) { b } else { a }
}

/// Merges one done lane into the run's branch and takes back what it kept apart; the worktree and the branch go.
fn join_one(p: &Proc, step: &Step, lane: &Lane, ended: &Ended) -> Result<(), String> {
    let project = &p.project;
    let label = format!("{}:{}", step.id, lane.key);
    match join::join(project, lane) {
        Joined::Merged(tip) | Joined::Already(tip) => {
            files::take_back(project, &lane.dir, &lane.key, &ended.redirects);
            files::take_back_state(project, &lane.dir, &lane.key, &ended.state_seed);
            note(project, &step.id, &lane.key, &tip);
            p.record(&label, "lane", true, &format!("joined at {}", &tip[..tip.len().min(10)]));
            worktree::remove(project, lane);
            Ok(())
        }
        Joined::Conflict(found) => {
            let why = format!("lane `{}` conflicts with the lanes joined before it in {}: merge `{}` by hand (`git merge {}`), then `--resume`",
                              lane.key, found.join(", "), lane.branch, lane.branch);
            p.record(&label, "lane", false, &why);
            Err(why)
        }
        Joined::Failed(err) => {
            let why = format!("lane `{}`: joining it failed: {err}", lane.key);
            p.record(&label, "lane", false, &why);
            Err(why)
        }
    }
}

/// One lane, on its own thread: the worktree, the engine instance and the item's steps.
fn lane_body(env: &Env, lane: &Lane, item: &Item, services: &Arc<Shared>) -> Ended {
    services.view(LaneView { state: LaneState::Running, started: Some(crate::util::now_epoch()), ..LaneView::queued(&lane.key) });
    if env.target.kind != super::format::TargetKind::Local {
        let ended = super::lane_remote::run(env, lane, item, services, env.target);
        let state = match (&ended.result, ended.stopped) {
            (Ok(()), _) => LaneState::Done,
            (Err(_), true) => LaneState::Stopped,
            (Err(_), false) => LaneState::Failed,
        };
        services.view(LaneView { state, ended: Some(crate::util::now_epoch()), ..LaneView::queued(&lane.key) });
        return ended;
    }
    let existed = lane.exists();
    let setup = worktree::add(env.project, lane, env.base).map_err(Fatal::from_message);
    let mut ended = Ended { result: Ok(()), stopped: false, reentry: None, spent: false, redirects: vec![], state_seed: vec![] };
    let outcome = setup.and_then(|()| {
        if !existed {
            worktree::seed(env.project, &lane.dir, env.seed);
            files::seed_state(env.project, &lane.dir);
        }
        ended.state_seed = files::state_lengths_kept(env.project, &lane.dir);
        let (spec, redirects) = files::redirected(env.spec, env.project, &lane.dir);
        ended.redirects = redirects;
        steps(env, lane, item, &spec, (services, existed))
    });
    let (reentry, spent) = crate::workflow::task_force::take_requests();
    (ended.reentry, ended.spent) = (reentry, spent);
    if let Err(e) = outcome {
        ended.stopped = matches!(e, Fatal::Ended(RunOutcome::Stopped | RunOutcome::Disk));
        ended.result = Err(e.to_string());
    }
    let state = match (&ended.result, ended.stopped) {
        (Ok(()), _) => LaneState::Done,
        (Err(_), true) => LaneState::Stopped,
        (Err(_), false) => LaneState::Failed,
    };
    services.view(LaneView { state, ended: Some(crate::util::now_epoch()), ..LaneView::queued(&lane.key) });
    ended
}

/// The lane's engine instance, built here on the lane's thread, and the item's child steps.
pub(super) fn steps(env: &Env, lane: &Lane, item: &Item, spec: &Process, (services, existed): (&Arc<Shared>, bool)) -> Result<(), Fatal> {
    let svc: Arc<dyn LaneServices> = services.clone();
    let mut rlt = Relentless::open_with(&lane.dir, Opts { resume: env.resume && existed })?
        .project_name(&lane.project)
        .lanes(svc, &lane.key)
        .memory(spec.rlt.memory)
        .vars(env.vars.clone())
        .user_settings(spec.rlt.user_settings)
        .user_env(spec.rlt.user_env.clone())
        .on_regression(if spec.rlt.on_regression == "rollback" { crate::driver::OnRegression::Rollback } else { crate::driver::OnRegression::Annotate })
        .verifier_timeout(spec.rlt.verifier_timeout)
        .min_free_disk(spec.rlt.min_free_disk.unwrap_or_default());
    rlt = rlt.system_prompt(standing::text(spec.run.system_prompt.as_ref(), standing::SYSTEM, env.home)?);
    let force = run::task_force(spec, env.home, &lane.dir, env.vars, env.roles)?;
    let init = Init { spec: spec.clone(), list: env.list.to_vec(), home: env.home.to_path_buf(), settings: env.settings.clone(),
                      vars: env.vars.clone(), roles: env.roles.cloned(), force };
    let p = Proc::new_lane(rlt, env.run.clone(), init, env.phase.clone());
    for child in &env.step.steps {
        run::exec(&p, child, Some(item.clone()))?;
    }
    p.finish_lane()?;
    p.finish_with(RunOutcome::GoalsMet, "the lane is done");
    Ok(())
}
