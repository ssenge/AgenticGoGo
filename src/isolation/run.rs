//! Build a run's [`RunPolicy`] from its config and register it for the project — the one call both
//! front ends (`loop_::run_with`, the driver's `boot`) make before anything is confined.
//!
//! [`resolve`] is ALSO what startup validation reports from ([`crate::capability::assess`]), so
//! `agg check` and the run cannot disagree about what is wrong. Registering re-checks and fails
//! CLOSED — a run whose policy cannot be enforced does not start, even for a caller that skipped
//! the check.

use super::policy::{self, PolicyGuard, RunPolicy};
use crate::core::config::AggConfig;
use crate::problem::Problem;
use anyhow::Result;
use std::path::Path;

/// Resolve `cfg`'s run-level confinement for the project in `dir` and register it until the
/// returned guard drops.
pub fn register(cfg: &AggConfig, dir: &Path) -> Result<PolicyGuard> {
    Ok(policy::register(dir, build(cfg, dir)?))
}

/// The policy `cfg` asks for, or the problems that refuse it.
pub fn build(cfg: &AggConfig, dir: &Path) -> Result<RunPolicy> {
    let (policy, problems) = resolve(cfg, dir);
    if !problems.is_empty() {
        anyhow::bail!(crate::problem::render(&problems));
    }
    Ok(policy)
}

/// The policy `cfg` asks for, plus every problem with it, each at its config key.
pub fn resolve(cfg: &AggConfig, dir: &Path) -> (RunPolicy, Vec<Problem>) {
    let (contexts, mut problems) = super::context::resolve(&cfg.context, dir);
    let ip = &cfg.isolation_policy;
    let mut pass_env = Vec::new();
    for (i, n) in ip.pass_env.iter().enumerate() {
        if is_env_name(n) {
            pass_env.push(n.clone());
        } else {
            problems.push(Problem::new(
                format!("isolation_policy.pass_env[{i}]"),
                format!("`{n}` is not an environment variable NAME — list names only (`NPM_TOKEN`); the value is read from agg's environment"),
            ));
        }
    }
    let project = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut credentials = Vec::new();
    for (i, raw) in ip.credentials.iter().enumerate() {
        match std::fs::canonicalize(super::context::expand(raw.trim(), &project)) {
            Ok(p) => credentials.push(p),
            Err(e) => problems.push(Problem::new(
                format!("isolation_policy.credentials[{i}]"),
                format!("credential path `{raw}` does not exist ({e}) — it is mounted read-only into every container"),
            )),
        }
    }
    let judge_image = match cfg.judge_image() {
        Ok(i) => i,
        Err(why) => {
            // only a problem when judges actually run in a container
            if cfg.run_isolation() == super::Isolation::Container {
                problems.push(Problem::new("judge.image", why));
            }
            super::DEFAULT_IMAGE.to_string()
        }
    };
    let egress = match &ip.network {
        super::egress::Network::Open => None,
        super::egress::Network::Allow(entries) => match super::egress::allow::AllowList::parse(entries) {
            Ok(list) => Some(super::egress::Egress::new(list, dir)),
            Err(bad) => {
                for (i, why) in bad {
                    problems.push(Problem::new(format!("isolation_policy.network.allow[{i}]"), why));
                }
                None
            }
        },
    };
    (RunPolicy { contexts, pass_env, credentials, judge_image, egress }, problems)
}

/// `[A-Za-z_][A-Za-z0-9_]*` — what `-e NAME` can carry without being read as `NAME=value`.
fn is_env_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> AggConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn a_config_without_a_policy_registers_the_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = register(&cfg("project: p\n"), tmp.path()).unwrap();
        let p = policy::for_dir(tmp.path());
        assert!(p.contexts.is_empty() && p.pass_env.is_empty() && p.credentials.is_empty() && p.egress.is_none());
        assert_eq!(p.judge_image, super::super::DEFAULT_IMAGE);
    }

    #[test]
    fn a_context_is_registered_and_a_bad_one_refuses_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let kb = tempfile::tempdir().unwrap();
        let yaml = format!("project: p\ncontext: [{{ path: \"{}\", as: \"kb\" }}]\n", kb.path().display());
        let g = register(&cfg(&yaml), tmp.path()).unwrap();
        assert_eq!(policy::for_dir(tmp.path()).contexts[0].name, "kb");
        drop(g);
        let bad = cfg("project: p\ncontext: [{ path: \"/no/such/dir\", as: \"kb\" }]\n");
        let err = register(&bad, tmp.path()).err().expect("refused").to_string();
        assert!(err.contains("does not exist"), "{err}");
    }

    /// `pass_env` takes NAMES (a `NAME=value` would put the secret in agg.yaml); `credentials` must
    /// exist. Each refusal at its own key.
    #[test]
    fn pass_env_and_credentials_are_validated_at_their_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let cred = tempfile::NamedTempFile::new().unwrap();
        let yaml = format!(
            "project: p\nisolation_policy:\n  pass_env: [\"NPM_TOKEN\", \"BAD=1\", \"_OK\"]\n  credentials: [\"{}\", \"/no/such/cred\"]\n",
            cred.path().display()
        );
        let (p, problems) = resolve(&cfg(&yaml), tmp.path());
        let at: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(at, ["isolation_policy.pass_env[1]", "isolation_policy.credentials[1]"]);
        assert_eq!(p.pass_env, ["NPM_TOKEN", "_OK"]);
        assert_eq!(p.credentials, [std::fs::canonicalize(cred.path()).unwrap()]);
    }

    /// A malformed allow-list entry is refused at its index; a good list arms the egress policy.
    #[test]
    fn an_allow_list_is_parsed_and_each_bad_entry_located() {
        let tmp = tempfile::tempdir().unwrap();
        let (p, problems) = resolve(&cfg("project: p\nisolation_policy: { network: { allow: [\"api.x.com\", \"https://bad\"] } }\n"), tmp.path());
        assert_eq!(problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["isolation_policy.network.allow[1]"]);
        assert!(p.egress.is_none());
        let (p, problems) = resolve(&cfg("project: p\nisolation_policy: { network: { allow: [\"api.x.com\"] } }\n"), tmp.path());
        assert!(problems.is_empty() && p.egress.is_some_and(|e| e.allow.allows_host("api.x.com")));
    }

    /// Judges in a container need ONE image; two container steps on different images with no
    /// `judge.image` is refused — but only when the run's judges actually run in a container.
    #[test]
    fn an_ambiguous_judge_image_is_refused_only_for_a_container_run() {
        let tmp = tempfile::tempdir().unwrap();
        let two = "project: p\nsteps:\n  a: { isolation: container, image: \"x:1\" }\n  b: { isolation: container, image: \"y:1\" }\n";
        let (_, problems) = resolve(&cfg(two), tmp.path());
        assert_eq!(problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["judge.image"]);
        assert!(problems[0].message.contains("x:1, y:1"), "{}", problems[0].message);
        let named = format!("{two}judge: {{ image: \"j:1\" }}\n");
        let (p, problems) = resolve(&cfg(&named), tmp.path());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(p.judge_image, "j:1");
        let one = "project: p\nsteps:\n  a: { isolation: container, image: \"x:1\" }\n  b: { isolation: sandbox }\n";
        assert_eq!(resolve(&cfg(one), tmp.path()).0.judge_image, "x:1", "the one container image");
    }
}
