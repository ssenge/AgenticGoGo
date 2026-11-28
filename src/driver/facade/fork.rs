//! Forks: parallel work the Rust way. [`Relentless::fork`] makes a new instance of the engine in a worktree of its own, on a
//! branch of its own cut from what this one has kept; the driver runs it on any thread with any Rust parallelism
//! (`std::thread::scope`, rayon, tokio's blocking pool); [`Relentless::merge`] brings its work back with a git merge, and a
//! fork that is dropped instead is discarded, its work kept on its branch.
//!
//! ```ignore
//! let mut rlt = Relentless::open(".")?;
//! let (guide, slides) = (rlt.fork()?, rlt.fork()?);
//! let (a, b) = rayon::join(|| guide.step(&write_guide), || slides.step(&write_slides));
//! a?; b?;
//! rlt.merge(guide)?;
//! rlt.merge(slides)?;
//! ```
//!
//! What is the engine's and not Rust's: two agents in one folder overwrite each other's files and git branch, so every
//! parallel piece of work gets a copy of the project of its own, and gets its result back through git. A fork shares the run's
//! ceilings, its rate-limit gate and its stop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::{Fatal, Relentless};
use crate::core::calls::CallRecord;
use crate::git::git;
use crate::lanes::join::{self, Joined};
use crate::lanes::services::{LaneServices, LaneState, LaneView, Limits, Shared};
use crate::lanes::worktree::{self, Lane};

/// Where a fork came from and what becomes of it.
pub(super) struct ForkOf {
    /// the root run's project folder: forks' worktrees and saved ledgers live under its `rlt/private/`
    pub(super) root: PathBuf,
    /// the root run's project name, which names every fork's branch
    pub(super) root_project: String,
    /// `4`: the number of the parent's ledger the fork call has; `4-2` is the second fork of the fork `4`
    pub(super) name: String,
    pub(super) lane: Lane,
    /// the commit the worktree is cut from: what the parent had kept when it forked
    base: String,
    /// the folder a merge goes into: the root's project, or the worktree of the fork it forked from
    pub(super) parent_dir: PathBuf,
    seed: Vec<String>,
    /// the worktree is made once, at the first call
    made: OnceLock<Result<(), String>>,
    pub(super) merged: AtomicBool,
    settled: AtomicBool,
}

/// Where the forks of a run keep what a resume needs of the ones that ended.
fn forks_dir(root: &Path) -> PathBuf {
    crate::paths::private_dir(root).join("forks")
}

impl ForkOf {
    /// The fork's private folder as it was when it ended.
    fn saved(&self) -> PathBuf {
        forks_dir(&self.root).join(crate::lanes::safe(&self.name)).join("private")
    }

    /// Makes the fork's worktree, once: a new one from the base; the one a stopped run left, as it is; or, for a fork that
    /// ended in a run this one resumes, its branch checked out again with its ledger put back so that it replays.
    pub(super) fn make(&self) -> &Result<(), String> {
        self.made.get_or_init(|| {
            let existed = self.lane.exists();
            if !existed && self.lane.worktree.exists() {
                // what a run left in the place of a worktree that is gone
                let _ = std::fs::remove_dir_all(&self.lane.worktree);
            }
            let saved = self.saved();
            if !existed && saved.is_dir() {
                worktree::reopen(&self.parent_dir, &self.lane)?;
                worktree::copy_dir(&saved, &crate::paths::private_dir(&self.lane.dir));
            } else {
                worktree::add(&self.parent_dir, &self.lane, &self.base)?;
                if !existed {
                    worktree::seed(&self.parent_dir, &self.lane.dir, &self.seed);
                }
            }
            Ok(())
        })
    }

    /// Whether the worktree was made.
    pub(super) fn made(&self) -> bool {
        matches!(self.made.get(), Some(Ok(())))
    }

    /// The fork's first call: its worktree is made, and its view says it runs.
    pub(super) fn begin(&self, services: Option<&Arc<dyn LaneServices>>) -> Result<(), String> {
        self.make().clone()?;
        if let Some(svc) = services {
            svc.view(LaneView { state: LaneState::Running, started: Some(crate::util::now_epoch()), ..LaneView::queued(&self.name) });
        }
        Ok(())
    }

    /// The end of a fork, merged or not: its private folder (ledger, report) is kept under the root's, its worktree goes and
    /// its branch stays.
    pub(super) fn settle(&self, owner: &Relentless) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.made() && self.lane.exists() {
            let saved = self.saved();
            let _ = std::fs::remove_dir_all(&saved);
            worktree::copy_dir(&crate::paths::private_dir(&self.lane.dir), &saved);
            // the run's pid is the process's own: a resume's guard would meet it
            let _ = std::fs::remove_file(saved.join("run.pid"));
            worktree::remove_worktree(&self.parent_dir, &self.lane);
        }
        if let Some((services, _)) = &owner.lane {
            let state = if self.merged.load(Ordering::SeqCst) { LaneState::Done } else { LaneState::Discarded };
            services.view(LaneView { state, ended: Some(crate::util::now_epoch()), ..LaneView::queued(&self.name) });
        }
    }
}

/// What an earlier run left of forks: their worktrees, their branches and their saved folders. A fresh run starts over.
fn clear_stale(root: &Path, project: &str) {
    let lanes = crate::paths::private_dir(root).join("lanes");
    for entry in std::fs::read_dir(&lanes).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("fork-") {
            let _ = git(root, &["worktree", "remove", "--force", &entry.path().display().to_string()]);
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let _ = git(root, &["worktree", "prune"]);
    let pattern = format!("refs/heads/rlt/{}/lane-fork-*", crate::lanes::safe(project));
    for branch in git(root, &["for-each-ref", "--format=%(refname:short)", &pattern]).1.lines() {
        let _ = git(root, &["branch", "-D", branch]);
    }
    let _ = std::fs::remove_dir_all(forks_dir(root));
}

impl Relentless {
    /// A new instance of the engine in a worktree of its own, on a branch of its own cut from what this one has kept. It has
    /// this one's settings, shares the run's ceilings, its rate-limit gate and its stop, and is a full `Relentless`: it
    /// steps, gates, verifies, and forks again. Run it on any thread; one thread works on it at a time.
    ///
    /// The worktree is made at the fork's first call, so forking many items and running a few at a time makes a few
    /// worktrees at a time. This instance must have gated what it staged (a fork starts from what the run has kept), and the
    /// project must be a git repository without uncommitted changes. A fork is named by the number its call has in the run's
    /// ledger, so a resumed run finds its forks again: `fork` and `merge` take `&mut self` so that they happen in the order
    /// the driver's code has.
    pub fn fork(&mut self) -> Result<Relentless, Fatal> {
        let _call = self.enter();
        let started = self.ready()?;
        if self.ungated_span().is_some() {
            return Err(Fatal::from_message("fork: this instance has ungated work: call gate() first, a fork starts from what the run has kept".into()));
        }
        let (root, parent, project) = match &self.fork_of {
            Some(f) => (f.root.clone(), self.dir.clone(), f.root_project.clone()),
            None => (self.dir.clone(), self.dir.clone(), self.cfg.project.clone()),
        };
        self.prepare_forking(&root, &parent, &project)?;
        let name = match &self.fork_of {
            Some(f) => format!("{}-{}", f.name, self.ord.get()),
            None => self.ord.get().to_string(),
        };
        match self.replayed(started, "fork")? {
            Some(_) => self.skip(),
            None => self.record(CallRecord::Fork { ord: self.ord.get(), label: self.label_path(), name: name.clone(), ts: crate::util::now_epoch() })?,
        }
        Ok(self.new_fork(root, project, parent, name))
    }

    /// What the first fork of a run checks and sets up, and every fork checks again: git, a clean tree, the ignore rules.
    fn prepare_forking(&self, root: &Path, parent: &Path, project: &str) -> Result<(), Fatal> {
        if worktree::toplevel(parent).is_none() {
            return Err(Fatal::from_message("fork needs the project in a git repository: a fork is a worktree of it".into()));
        }
        if !self.resume && !crate::git::is_clean(parent) {
            return Err(Fatal::from_message("fork: the project has uncommitted changes, which the fork's worktree would not hold: commit them first".into()));
        }
        // committed here, as the first session would: otherwise every fork's branch adds the file and the merge meets the
        // parent's own untracked copy
        crate::git::ensure_rlt_gitignored(parent);
        crate::workflow::commit(parent, &[".gitignore"], "Ignore rlt's runtime folders").map_err(Fatal::Other)?;
        if self.fork_of.is_none() && self.forks.get().is_none() {
            if !self.resume {
                clear_stale(root, project);
            }
            let limits = self.cfg_limits();
            let _ = self.forks.set(Shared::new(Limits { tokens: limits.tokens, cost: limits.cost }));
        }
        Ok(())
    }

    /// The instance of fork `name`: this one's settings, not yet started.
    fn new_fork(&self, root: PathBuf, project: String, parent: PathBuf, name: String) -> Relentless {
        let lane = Lane::of(&root, &project, "fork", &name);
        let base = git(&parent, &["rev-parse", "HEAD"]).1;
        let resume = self.resume && (lane.exists() || forks_dir(&root).join(crate::lanes::safe(&name)).join("private").is_dir());
        let services: Arc<dyn LaneServices> = match &self.lane {
            Some((services, _)) => services.clone(),
            None => self.forks.get().expect("set by prepare_forking").clone(),
        };
        services.view(LaneView::queued(&name));
        let mut cfg = self.cfg.clone();
        cfg.project = lane.project.clone();
        let mut fork = Relentless::bare(cfg, lane.dir.clone(), resume);
        fork.custom_ruler = self.custom_ruler.clone();
        fork.memory = self.memory;
        fork.user_settings = self.user_settings;
        fork.user_env = self.user_env.clone();
        fork.system_prompt = self.system_prompt.clone();
        fork.vars = self.vars.clone();
        fork.fork_seed = self.fork_seed.clone();
        fork.lane = Some((services, name.clone()));
        fork.fork_of = Some(ForkOf {
            root, root_project: project, name, lane, base, parent_dir: parent, seed: self.fork_seed.clone(),
            made: OnceLock::new(), merged: AtomicBool::new(false), settled: AtomicBool::new(false),
        });
        fork
    }

    /// Merges `fork`'s branch into this instance's branch (`git merge --no-ff`, the subject `rlt: merge fork <name>`) and
    /// returns the merge commit. Both this instance and the fork must have gated what they staged. A conflict is aborted and
    /// comes back as [`Fatal::Conflict`]: this instance is as it was, and the fork's branch is kept for the operator. A fork
    /// that is dropped instead of merged is discarded.
    pub fn merge(&mut self, fork: Relentless) -> Result<String, Fatal> {
        let _call = self.enter();
        let started = self.ready()?;
        let Some(of) = fork.fork_of.as_ref().filter(|f| f.parent_dir == self.dir) else {
            return Err(Fatal::from_message("merge takes a fork this instance made (`Relentless::fork`)".into()));
        };
        if self.ungated_span().is_some() {
            return Err(Fatal::from_message("merge: this instance has ungated work: call gate() first".into()));
        }
        if fork.ungated_span().is_some() {
            return Err(Fatal::from_message(format!("merge: fork `{}` has ungated work: call gate() on it first", of.name)));
        }
        // a merge the earlier process made is on base: it is not made again
        if let Some(CallRecord::Merge { tip, .. }) = self.replayed(started, "merge")? {
            self.skip();
            of.merged.store(true, Ordering::SeqCst);
            return Ok(tip);
        }
        fork.wind_down("merging");
        let tip = if of.made() {
            match join::join_as(&self.dir, &of.lane, &format!("rlt: merge fork {}", of.name)) {
                Joined::Merged(tip) | Joined::Already(tip) => tip,
                Joined::Conflict(files) => return Err(Fatal::Conflict { branch: of.lane.branch.clone(), files }),
                Joined::Failed(why) => return Err(Fatal::from_message(format!("merge: fork `{}`: {why}", of.name))),
            }
        } else {
            git(&self.dir, &["rev-parse", "HEAD"]).1
        };
        of.merged.store(true, Ordering::SeqCst);
        self.record(CallRecord::Merge { ord: self.ord.get(), label: self.label_path(), name: of.name.clone(), tip: tip.clone(), ts: crate::util::now_epoch() })?;
        Ok(tip)
    }
}
