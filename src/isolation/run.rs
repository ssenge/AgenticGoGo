//! Build a run's [`RunPolicy`] from its config and register it for the project — the one call both
//! front ends (`loop_::run_with`, the driver's `boot`) make before anything is confined.
//!
//! Startup has already refused every config this could fail on ([`crate::capability::assess`]
//! reports the same problems, located); failing here too is the backstop for a caller that skipped
//! that check, and it fails CLOSED — a run with a context it cannot enforce does not start.

use super::policy::{self, PolicyGuard, RunPolicy};
use crate::core::config::AggConfig;
use anyhow::Result;
use std::path::Path;

/// Resolve `cfg`'s run-level confinement for the project in `dir` and register it until the
/// returned guard drops.
pub fn register(cfg: &AggConfig, dir: &Path) -> Result<PolicyGuard> {
    Ok(policy::register(dir, build(cfg, dir)?))
}

/// The policy `cfg` asks for, or the problems that refuse it.
pub fn build(cfg: &AggConfig, dir: &Path) -> Result<RunPolicy> {
    let (contexts, problems) = super::context::resolve(&cfg.context, dir);
    if !problems.is_empty() {
        anyhow::bail!(crate::problem::render(&problems));
    }
    Ok(RunPolicy { contexts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_a_policy_registers_the_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg: AggConfig = serde_yaml::from_str("project: p\n").unwrap();
        let _g = register(&cfg, tmp.path()).unwrap();
        assert!(policy::for_dir(tmp.path()).contexts.is_empty());
    }

    #[test]
    fn a_context_is_registered_and_a_bad_one_refuses_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let kb = tempfile::tempdir().unwrap();
        let yaml = format!("project: p\ncontext: [{{ path: \"{}\", as: \"kb\" }}]\n", kb.path().display());
        let cfg: AggConfig = serde_yaml::from_str(&yaml).unwrap();
        let g = register(&cfg, tmp.path()).unwrap();
        assert_eq!(policy::for_dir(tmp.path()).contexts[0].name, "kb");
        drop(g);
        let bad: AggConfig = serde_yaml::from_str("project: p\ncontext: [{ path: \"/no/such/dir\", as: \"kb\" }]\n").unwrap();
        let err = register(&bad, tmp.path()).err().expect("refused").to_string();
        assert!(err.contains("does not exist"), "{err}");
    }
}
