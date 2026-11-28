//! `Relentless::fan_out`: a driver's keys run at once as lanes, each in a worktree of its own with an engine instance of its
//! own, and are joined in key order, as the items of a parallel `each` of a process file are.
//!
//! ```ignore
//! let reports = rlt.fan_out(&["billing", "orders", "search"], FanOut::new().max(3), |lane, key| {
//!     lane.step(&Step::new(format!("port {key}")).prompt(format!("Port the {key} module.")))?;
//!     Ok(())
//! })?;
//! ```

use std::sync::Arc;

use super::{Fatal, Relentless};
use crate::lanes::forward::Forwarder;
use crate::lanes::join::{self, Joined};
use crate::lanes::record;
use crate::lanes::schedule::{self, Plan, Reading};
use crate::lanes::services::{LaneServices, Limits, Shared};
use crate::lanes::worktree::{self, Lane};

/// How a fan-out runs its lanes.
#[derive(Debug, Clone)]
pub struct FanOut {
    max: usize,
    fail_fast: bool,
    seed: Vec<String>,
}

impl FanOut {
    /// Four lanes at once, the others waiting; a failed lane does not stop the rest.
    pub fn new() -> FanOut {
        FanOut { max: 4, fail_fast: false, seed: Vec::new() }
    }

    /// How many lanes run at once (at least 1).
    pub fn max(mut self, max: usize) -> FanOut {
        self.max = max.max(1);
        self
    }

    /// A failed lane stops the running ones and starts no more.
    pub fn fail_fast(mut self, on: bool) -> FanOut {
        self.fail_fast = on;
        self
    }

    /// Ignored or untracked paths of the project copied into each lane's worktree (dependencies, build caches).
    pub fn seed(mut self, paths: &[&str]) -> FanOut {
        self.seed = paths.iter().map(|p| p.to_string()).collect();
        self
    }
}

impl Default for FanOut {
    fn default() -> FanOut {
        FanOut::new()
    }
}

/// How one lane ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneEnd {
    /// its body returned `Ok` and its work was joined
    Joined,
    /// its body returned `Ok`, and its work waits: a lane before it did not end done
    Done,
    /// its body failed, or panicked
    Failed(String),
    /// the run was stopped under it
    Stopped,
    /// never started: a stop or `fail_fast` came first
    NotStarted,
    /// its work conflicts with the lanes joined before it, in these files; its branch is for the human to merge
    Conflict(Vec<String>),
}

/// One lane's end, and its tip when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneReport {
    pub key: String,
    pub end: LaneEnd,
    pub tip: Option<String>,
}

impl LaneReport {
    /// Whether the lane's work is in the run's branch.
    pub fn joined(&self) -> bool {
        self.end == LaneEnd::Joined
    }
}

impl Relentless {
    /// Runs `body` for each of `keys` as a lane: at most `opts.max` at once, each in a linked worktree of the project on
    /// a branch of its own, with an instance of the engine that is this one's (its settings, its ceilings: the run's
    /// total spend counts, a rate limit one lane meets holds back the others). When the lanes have ended, the ones that
    /// ended done are merged into the run's branch in key order, up to the first that did not; a conflict is aborted and
    /// reported, never half-resolved. Lanes the run joined before (`--resume`) are not run again. A lane's failure is in
    /// its report, not an `Err`.
    ///
    /// `body` runs on the lane's thread and gets the lane's instance: use it as the driver uses `self`.
    pub fn fan_out<F>(&self, keys: &[&str], opts: FanOut, body: F) -> Result<Vec<LaneReport>, Fatal>
    where
        F: Fn(&Relentless, &str) -> anyhow::Result<()> + Sync,
    {
        self.check_limits()?;
        let project = self.dir().to_path_buf();
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        if let Some(clash) = crate::lanes::collisions(&keys).first() {
            return Err(Fatal::from_message(format!("fan_out: keys that become one name in a lane's folder and branch: {clash}")));
        }
        if worktree::toplevel(&project).is_none() {
            return Err(Fatal::from_message("fan_out needs the project in a git repository: a lane is a worktree of it".into()));
        }
        let resume = self.resumes();
        let joined = if resume { record::recorded(&project, "fan") } else { record::forget(&project, "fan") };
        if !resume && !crate::git::is_clean(&project) {
            return Err(Fatal::from_message("fan_out: the project has uncommitted changes, which the lanes' worktrees would not hold: commit them first".into()));
        }
        crate::git::ensure_rlt_gitignored(&project);
        crate::workflow::commit(&project, &[".gitignore"], "Ignore rlt's runtime folders").map_err(Fatal::Other)?;
        let base = crate::git::git(&project, &["rev-parse", "HEAD"]).1;
        let name = self.project_name_of();
        let lanes: Vec<Lane> = keys.iter().map(|k| Lane::of(&project, &name, "fan", k)).collect();
        if !resume {
            lanes.iter().filter(|l| l.exists()).for_each(|l| worktree::remove(&project, l));
        }
        let todo: Vec<usize> = (0..keys.len()).filter(|i| !joined.contains(&keys[*i])).collect();
        let (limits, template) = (self.cfg_limits(), self.lane_template());
        let services = Shared::new(Limits { tokens: limits.tokens, cost: limits.cost });
        let forwarder = Forwarder::new(&project, &lanes, &todo, &services);
        let plan = Plan { keys: todo.iter().map(|i| keys[*i].clone()).collect(), max: opts.max, fail_fast: opts.fail_fast };
        let run_lane = |n: usize, key: &str| -> Result<(), (String, bool)> {
            let lane = &lanes[todo[n]];
            let existed = lane.exists();
            worktree::add(&project, lane, &base).map_err(|e| (e, false))?;
            if !existed {
                worktree::seed(&project, &lane.dir, &opts.seed);
            }
            let svc: Arc<dyn LaneServices> = services.clone();
            let instance = template.instance(&lane.dir, &lane.project, key, svc, resume && existed).map_err(|e| (e.to_string(), false))?;
            let outcome = body(&instance, key);
            instance.finish(if outcome.is_ok() { crate::plugin::RunOutcome::GoalsMet } else { crate::plugin::RunOutcome::Halt });
            outcome.map_err(|e| {
                let stopped = e.downcast_ref::<Fatal>().is_some_and(|f| matches!(f, Fatal::Ended(crate::plugin::RunOutcome::Stopped)));
                (format!("{e:#}"), stopped)
            })
        };
        let results = schedule::run(
            &plan,
            &services,
            &|running| forwarder.tick(running.iter().map(|r| todo[*r]).collect::<Vec<_>>().as_slice()),
            &run_lane,
            &Reading { failed: &|r: &Result<(), (String, bool)>| r.is_err(), panicked: &|why| Err((format!("panic: {why}"), false)) },
        );
        forwarder.tick(&[]);
        forwarder.finished();
        let mut reports = vec![];
        let mut clean = true;
        let mut results = results.into_iter();
        for (i, key) in keys.iter().enumerate() {
            let lane = &lanes[i];
            if joined.contains(key) {
                reports.push(LaneReport { key: key.clone(), end: LaneEnd::Joined, tip: None });
                continue;
            }
            let (end, tip) = match results.next().flatten() {
                None => {
                    clean = false;
                    (LaneEnd::NotStarted, None)
                }
                Some(Err((why, stopped))) => {
                    clean = false;
                    (if stopped { LaneEnd::Stopped } else { LaneEnd::Failed(why) }, worktree::tip(&project, lane))
                }
                Some(Ok(())) if !clean => (LaneEnd::Done, worktree::tip(&project, lane)),
                Some(Ok(())) => match join::join(&project, lane) {
                    Joined::Merged(tip) | Joined::Already(tip) => {
                        record::note(&project, "fan", key, &tip);
                        worktree::remove(&project, lane);
                        (LaneEnd::Joined, Some(tip))
                    }
                    Joined::Conflict(files) => {
                        clean = false;
                        (LaneEnd::Conflict(files), worktree::tip(&project, lane))
                    }
                    Joined::Failed(why) => {
                        clean = false;
                        (LaneEnd::Failed(why), worktree::tip(&project, lane))
                    }
                },
            };
            reports.push(LaneReport { key: key.clone(), end, tip });
        }
        if services.stop_reason().is_some() && forwarder.requeue_stop(&services.stop_reason().unwrap_or_default()) {
            self.check_limits()?;
        }
        Ok(reports)
    }
}
