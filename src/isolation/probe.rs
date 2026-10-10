//! The container tier's STARTUP PROBE: one short container per image the run will use, asking the
//! two questions a container run cannot answer for itself until a session has already failed.
//!
//! 1. **Does the image have the CLI?** A worker step runs `claude`/`codex` inside its image, and the
//!    judges' image runs the ruler's CLI for an LLM judge and `swe-metrics` for a metric or score
//!    judge. Missing, every session dies at spawn.
//! 2. **Can the engine see the paths agg mounts?** A VM-backed engine (colima, lima) shares only
//!    some host directories with its VM, and a bind of one it cannot see is not an error — the
//!    container gets an EMPTY directory there. A worker in an empty project, or a judge reading an
//!    empty context, is a session that runs, reports success, and did nothing real.
//!
//! Both are refusals, located at the key that chose the image or the path ([`assess`]), so `agg
//! check` lists them and `agg run` will not start. The facts also feed `agg doctor --json`.

use crate::core::config::AggConfig;
use crate::core::model::{Judge, JudgeKind};
use crate::problem::Problem;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the probe learned about one image (`agg doctor --json` → `container_images`).
#[derive(Debug, Clone, Serialize)]
pub struct ImageFacts {
    pub image: String,
    /// the agent CLIs the run needs from this image → the first line of `<bin> --version`, or null
    /// when the image has no such command.
    pub clis: BTreeMap<String, Option<String>>,
    /// mounted paths the engine could not see (an empty directory inside the container).
    pub invisible: Vec<PathBuf>,
    /// the probe container could not run at all (no such image, no `/bin/sh`, …).
    pub error: Option<String>,
}

/// One image the run needs, with what it needs it for.
struct Need {
    /// the config key that chose it first — where a failure to start it is reported.
    at: String,
    /// each CLI it must have, with the key of what runs it there (a step's image, the judges').
    bins: Vec<(String, String)>,
}

/// Probe every image `cfg` runs in and report each problem at its key. Runs nothing when the config
/// has no container tier or no engine answers (that refusal is the tier check's).
pub fn assess(cfg: &AggConfig, judges: &[Judge], dir: &Path) -> (Vec<ImageFacts>, Vec<Problem>) {
    let needs = needs(cfg, judges);
    if needs.is_empty() || !super::container_available() {
        return (Vec::new(), Vec::new());
    }
    let paths = mounted_paths(cfg, dir);
    // as the run will: the project's owner, never root — "on PATH for any uid" is the contract.
    let user = super::container::owner(dir);
    let mut facts = Vec::new();
    let mut problems = Vec::new();
    for (i, (image, need)) in needs.iter().enumerate() {
        // the paths are the same for every image; one probe of them is enough.
        let probe_paths: &[(PathBuf, String)] = if i == 0 { &paths } else { &[] };
        let bins: Vec<String> = need.bins.iter().map(|(b, _)| b.clone()).collect();
        let f = probe(image, &bins, &probe_paths.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), user.as_deref());
        if let Some(e) = &f.error {
            problems.push(Problem::new(need.at.clone(), format!("the container image `{image}` cannot be started: {e}")));
        }
        for (bin, at) in &need.bins {
            if f.clis.get(bin).is_some_and(Option::is_none) && f.error.is_none() {
                problems.push(missing_cli(at, image, bin));
            }
        }
        for p in &f.invisible {
            let at = probe_paths.iter().find(|(q, _)| q == p).map(|(_, a)| a.clone()).unwrap_or_default();
            problems.push(Problem::new(at, format!(
                "the container engine cannot see `{}` — mounted, it would be an EMPTY directory inside \
                 the container. A VM-backed engine shares only some host directories (colima and lima: \
                 your home directory by default); move it there, or share it with the engine's VM",
                p.display()
            )));
        }
        facts.push(f);
    }
    (facts, problems)
}

/// The refusal of an image that lacks a CLI the run runs inside it — ONE spelling, so the swe-metrics
/// probe (`metrics::probe`) and this one report the same problem once.
pub fn missing_cli(at: &str, image: &str, bin: &str) -> Problem {
    Problem::new(at, format!(
        "the container image `{image}` has no `{bin}` on PATH — `{bin}` runs INSIDE it under \
         `isolation: container`. Use an image that follows the image contract (reference \
         Dockerfiles: `docker/{bin}/`), see docs/CONFIG.md → container tier"
    ))
}

/// The config key that chose the judges' image — where its problems are reported.
pub fn judge_image_key(cfg: &AggConfig) -> &'static str {
    if cfg.judge.image.is_some() {
        "judge.image"
    } else if cfg.defaults.image.is_some() {
        "defaults.image"
    } else {
        "defaults.isolation"
    }
}

/// Every image the run starts a container from, with the CLIs it must have.
fn needs(cfg: &AggConfig, judges: &[Judge]) -> BTreeMap<String, Need> {
    use super::Isolation::Container;
    let mut out: BTreeMap<String, Need> = BTreeMap::new();
    let mut add = |image: String, at: String, bin: Option<&str>| {
        let n = out.entry(image).or_insert(Need { at: at.clone(), bins: Vec::new() });
        if let Some(b) = bin.filter(|b| !n.bins.iter().any(|(x, _)| x == b)) {
            n.bins.push((b.to_string(), at));
        }
    };
    for name in cfg.steps.keys() {
        let Ok(s) = cfg.resolve_step(name) else { continue };
        if s.isolation != Container {
            continue;
        }
        let body = &cfg.steps[name];
        let at = if body.image.is_some() {
            format!("steps.{name}.image")
        } else if cfg.defaults.image.is_some() {
            "defaults.image".into()
        } else if body.isolation.is_some() {
            format!("steps.{name}.isolation")
        } else {
            "defaults.isolation".into()
        };
        add(s.image.clone(), at, s.backend().ok().map(|b| b.bin()));
    }
    if cfg.run_isolation() == Container {
        if let Ok(image) = cfg.judge_image() {
            let at = judge_image_key(cfg);
            let llm = judges.iter().any(|j| matches!(j.kind, JudgeKind::Llm { .. }));
            let ruler = cfg.ruler_backend().ok().filter(|_| llm).map(|b| b.bin());
            add(image.clone(), at.into(), ruler);
            let measured = judges.iter().any(|j| matches!(j.kind, JudgeKind::Metric(_) | JudgeKind::Score(_)));
            add(image, at.into(), measured.then_some(crate::metrics::measure::PROGRAM));
        }
    }
    out
}

/// The host paths a container of this run mounts, each with the key that names it.
fn mounted_paths(cfg: &AggConfig, dir: &Path) -> Vec<(PathBuf, String)> {
    let project = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut out = vec![(project, String::new())];
    let (policy, _) = super::run::resolve(cfg, dir);
    for (i, c) in policy.contexts.iter().enumerate() {
        out.push((c.host.clone(), format!("context[{i}].path")));
    }
    for (i, c) in policy.credentials.iter().enumerate() {
        out.push((c.clone(), format!("isolation_policy.credentials[{i}]")));
    }
    out
}

/// Run the probe container for `image`, as `user` (`uid:gid`) when given: which of `bins` it has
/// (and their versions), and which of `paths` (non-empty on the host) arrive empty.
pub fn probe(image: &str, bins: &[String], paths: &[PathBuf], user: Option<&str>) -> ImageFacts {
    let mut facts = ImageFacts { image: image.to_string(), clis: BTreeMap::new(), invisible: Vec::new(), error: None };
    let mut cmd = Command::new(super::container_engine().unwrap_or("docker"));
    cmd.args(["run", "--rm", "--network", "none", "--entrypoint", "/bin/sh", "-e", "HOME=/tmp"]);
    if let Some(u) = user {
        cmd.arg("--user").arg(u);
    }
    let mut script = String::new();
    for b in bins {
        script.push_str(&format!(
            "if command -v {b} >/dev/null 2>&1; then echo \"BIN {b} $({b} --version 2>/dev/null | head -n 1)\"; else echo \"NOBIN {b}\"; fi\n"
        ));
    }
    for (i, p) in paths.iter().enumerate() {
        cmd.arg("-v").arg(format!("{}:/agg-probe/{i}:ro", p.display()));
        script.push_str(&format!("if [ -n \"$(ls -A /agg-probe/{i} 2>/dev/null)\" ]; then echo SEES {i}; else echo EMPTY {i}; fi\n"));
    }
    cmd.arg(image).arg("-c").arg(&script).stdin(Stdio::null());
    let out = match crate::os::proc::run_with_timeout(cmd, PROBE_TIMEOUT_SECS) {
        Ok(o) => o,
        Err(e) => {
            facts.error = Some(e);
            return facts;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.success {
        let err = String::from_utf8_lossy(&out.stderr);
        facts.error = Some(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("the probe failed").trim().to_string());
        return facts;
    }
    for b in bins {
        let v = text.lines().find_map(|l| l.strip_prefix(&format!("BIN {b} "))).map(|v| v.trim().to_string());
        let found = v.is_some() || text.lines().any(|l| l == format!("BIN {b}"));
        facts.clis.insert(b.clone(), if found { Some(v.unwrap_or_default()) } else { None });
    }
    for (i, p) in paths.iter().enumerate() {
        if text.lines().any(|l| l == format!("EMPTY {i}")) && non_empty(p) {
            facts.invisible.push(p.clone());
        }
    }
    facts
}

/// How long one probe may take — an image PULL included, which is what a first run would wait for.
const PROBE_TIMEOUT_SECS: u64 = 600;

/// A directory with something in it, or a file — what a container must not see as empty.
fn non_empty(p: &Path) -> bool {
    if p.is_file() {
        return true;
    }
    std::fs::read_dir(p).map(|mut d| d.next().is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> AggConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// Each image once, with the CLIs it needs and the key that chose it: a container step's agent;
    /// the ruler in the judges' image only when an LLM judge exists.
    #[test]
    fn every_container_image_is_needed_with_its_clis_at_its_key() {
        let c = cfg("project: p\nsteps:\n  a: { isolation: container, image: \"x:1\" }\n  b: { isolation: container, image: \"x:1\", agent: codex }\n  c: { isolation: sandbox }\njudge: { image: \"j:1\" }\n");
        let n = needs(&c, &[]);
        assert_eq!(n.keys().collect::<Vec<_>>(), ["j:1", "x:1"]);
        let bins = |n: &Need| n.bins.iter().map(|(b, _)| b.clone()).collect::<Vec<_>>();
        assert_eq!(bins(&n["x:1"]), ["claude", "codex"]);
        assert_eq!(n["x:1"].at, "steps.a.image");
        assert!(n["j:1"].bins.is_empty(), "no LLM judge ⇒ the judges' image needs no CLI");
        let llm = Judge {
            name: "r".into(),
            kind: JudgeKind::Llm { path: "r.md".into(), inputs: vec![] },
            invariant: false,
            in_dod: true,
            timeout: None,
            state: crate::core::model::Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        };
        assert_eq!(bins(&needs(&c, &[llm])["j:1"]), ["claude"]);
        assert!(needs(&cfg("project: p\nsteps: { a: { isolation: sandbox } }\n"), &[]).is_empty());
        let metric = Judge {
            name: "cc".into(),
            kind: JudgeKind::Metric(Box::new(crate::metrics::MetricJudge::new("complexity.cyclomatic").max(10.0))),
            invariant: false,
            in_dod: true,
            timeout: None,
            state: crate::core::model::Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        };
        assert_eq!(needs(&c, std::slice::from_ref(&metric))["j:1"].bins, [("swe-metrics".to_string(), "judge.image".to_string())], "a metric judge ⇒ swe-metrics in the judges' image");
        // one image for the worker and the judges: each CLI is reported at the key of what runs it
        let shared = cfg("project: p\ndefaults: { isolation: container, image: \"x:1\" }\nsteps: { a: {} }\njudge: { image: \"x:1\" }\n");
        let n = needs(&shared, &[metric]);
        assert_eq!(n["x:1"].bins, [("claude".to_string(), "defaults.image".to_string()), ("swe-metrics".to_string(), "judge.image".to_string())]);
        assert_eq!(judge_image_key(&c), "judge.image");
    }

    #[test]
    fn a_file_or_a_populated_dir_is_non_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(!non_empty(d.path()));
        std::fs::write(d.path().join("f"), "x").unwrap();
        assert!(non_empty(d.path()) && non_empty(&d.path().join("f")));
    }

    /// REAL ENGINE (skipped without one): a path the engine cannot see arrives empty and is
    /// reported; one under the project tree (shared with every engine agg supports) is seen; and a
    /// CLI the image lacks is reported missing.
    #[test]
    fn the_probe_reports_missing_clis_and_paths_the_engine_cannot_see() {
        if !super::super::container_available() {
            eprintln!("SKIPPED: no container engine");
            return;
        }
        let seen = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let f = probe(super::super::DEFAULT_IMAGE, &["sh".into(), "agg-no-such-cli".into()], std::slice::from_ref(&seen), Some("1000:1000"));
        assert!(f.error.is_none(), "{f:?}");
        assert!(f.clis["sh"].is_some() && f.clis["agg-no-such-cli"].is_none(), "{f:?}");
        assert!(f.invisible.is_empty(), "the repo is visible to the engine: {f:?}");
        let f = probe("agg-no-such-image:0", &[], &[], None);
        assert!(f.error.is_some(), "an image that does not exist cannot be probed: {f:?}");
    }
}
