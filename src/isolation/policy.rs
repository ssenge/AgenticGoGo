//! The RUN-LEVEL confinement policy: what every confined process of one run gets beyond its tier —
//! the read-only context directories today, the egress allow-list and the container runtime next.
//!
//! # Why a registry keyed by project dir, and not a parameter
//! The policy reaches five spawn surfaces — the worker, a script judge, an LLM judge's one-shot, a
//! hook, `notify.cmd` — and every one of them already confines by calling [`super::wrap`] /
//! [`super::containerize`] with the project dir. Deriving the policy from that dir is the same
//! choice `agg/private/`'s carve-out made ([`super::deny_set`]): a parameter is a thing a future
//! call site can forget, and forgetting this one would hand a confined process a writable context
//! or an open network. It also keeps the public [`crate::backend::AgentBackend::one_shot`]
//! signature stable for driver-supplied backends.
//!
//! The registry is not the deleted backend singleton in a new coat: nothing here is latched on
//! first read and nothing defaults to a guess. A run REGISTERS its policy for its project before
//! anything is confined and the [`PolicyGuard`] removes it when the run ends; a dir no run has
//! registered gets the EMPTY policy — exactly the behaviour every confined spawn had before the
//! policy existed. Keyed by the canonical dir, so two runs in one process (tests) never see each
//! other's.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One read-only context directory, resolved and validated ([`super::context::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMount {
    /// the `as:` name.
    pub name: String,
    /// the canonical host directory.
    pub host: PathBuf,
}

impl ContextMount {
    /// Where the directory appears inside a container: `/context/<as>`.
    pub fn container_path(&self) -> PathBuf {
        Path::new(CONTEXT_ROOT).join(&self.name)
    }

    /// The directory as a process on `tier` sees it — the mount point in a container, the host path
    /// everywhere else.
    pub fn seen_on(&self, tier: super::Isolation) -> PathBuf {
        match tier {
            super::Isolation::Container => self.container_path(),
            _ => self.host.clone(),
        }
    }

    /// `AGG_CONTEXT_<AS>` — the env var a confined process finds this directory under.
    pub fn env_key(&self) -> String {
        format!("AGG_CONTEXT_{}", self.name.to_uppercase().replace('-', "_"))
    }
}

/// Where context directories are mounted inside a container.
pub const CONTEXT_ROOT: &str = "/context";

/// Everything one run's confined processes get beyond their tier.
#[derive(Debug)]
pub struct RunPolicy {
    /// read-only context directories, in config order.
    pub contexts: Vec<ContextMount>,
    /// env var names every container of the run receives by name (`isolation_policy.pass_env`).
    pub pass_env: Vec<String>,
    /// canonical host paths every container of the run mounts read-only (`isolation_policy.credentials`).
    pub credentials: Vec<PathBuf>,
    /// the image judges run in when the run's tier is `container` (`judge.image`, resolved).
    pub judge_image: String,
    /// the run's judge tier from its config (`cfg.run_isolation()`). Under `container` swe-metrics
    /// runs in `judge_image` even for a judge run outside the loop (`agg judge`, `agg plan`): that
    /// is where startup found it, and the host need not have it at all.
    pub judge_tier: super::Isolation,
    /// the network allow-list and its enforcement; `None` = `network: open`.
    pub egress: Option<super::egress::Egress>,
}

impl Default for RunPolicy {
    fn default() -> Self {
        RunPolicy {
            contexts: Vec::new(),
            pass_env: Vec::new(),
            credentials: Vec::new(),
            judge_image: super::DEFAULT_IMAGE.to_string(),
            judge_tier: super::Isolation::None,
            egress: None,
        }
    }
}

impl RunPolicy {
    /// The `AGG_CONTEXT_<AS>` variables for a process confined on `tier`.
    pub fn context_env(&self, tier: super::Isolation) -> Vec<(String, PathBuf)> {
        self.contexts.iter().map(|c| (c.env_key(), c.seen_on(tier))).collect()
    }
}

/// The registered runs: canonical project dir → its policy.
static RUNS: Mutex<Vec<(PathBuf, Arc<RunPolicy>)>> = Mutex::new(Vec::new());

/// Keeps a run's policy registered; dropping it unregisters.
#[must_use = "the policy is unregistered the moment the guard drops"]
pub struct PolicyGuard {
    key: PathBuf,
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        runs.retain(|(k, _)| k != &self.key);
    }
}

fn key(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Register `policy` for the run in `dir`, replacing any earlier one for the same dir.
pub fn register(dir: &Path, policy: RunPolicy) -> PolicyGuard {
    let key = key(dir);
    let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    runs.retain(|(k, _)| k != &key);
    runs.push((key.clone(), Arc::new(policy)));
    PolicyGuard { key }
}

/// The policy of the run in `dir` — the EMPTY policy when no run registered one.
pub fn for_dir(dir: &Path) -> Arc<RunPolicy> {
    let key = key(dir);
    let runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    runs.iter().find(|(k, _)| k == &key).map(|(_, p)| p.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(name: &str, host: &str) -> ContextMount {
        ContextMount { name: name.into(), host: host.into() }
    }

    #[test]
    fn a_dir_no_run_registered_gets_the_empty_policy() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(for_dir(tmp.path()).contexts.is_empty());
    }

    /// Registered under the CANONICAL dir, so `wrap(cwd = ".")` and a symlinked spelling find it;
    /// and gone the moment the guard drops.
    #[test]
    fn a_registered_policy_is_found_by_any_spelling_until_its_guard_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("link");
        std::fs::create_dir(tmp.path().join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), &link).unwrap();
        let guard = register(&link, RunPolicy { contexts: vec![ctx("brain", "/b")], ..RunPolicy::default() });
        assert_eq!(for_dir(&tmp.path().join("real")).contexts.len(), 1, "the canonical spelling");
        assert_eq!(for_dir(&link).contexts.len(), 1, "the symlinked spelling");
        drop(guard);
        assert!(for_dir(&link).contexts.is_empty(), "unregistered on drop");
    }

    #[test]
    fn a_context_is_seen_at_its_mount_point_only_in_a_container() {
        let c = ctx("my-brain", "/home/u/brain");
        assert_eq!(c.seen_on(super::super::Isolation::Container), Path::new("/context/my-brain"));
        assert_eq!(c.seen_on(super::super::Isolation::Sandbox), Path::new("/home/u/brain"));
        assert_eq!(c.env_key(), "AGG_CONTEXT_MY_BRAIN");
        let p = RunPolicy { contexts: vec![c], ..RunPolicy::default() };
        assert_eq!(p.context_env(super::super::Isolation::Container), [("AGG_CONTEXT_MY_BRAIN".to_string(), PathBuf::from("/context/my-brain"))]);
    }
}
