//! The startup checks of an OPTIONAL dependency: a metric or score judge refuses the run when
//! `swe-metrics` is missing where it will run, speaks another schema version, or — for a score
//! judge — scores by another definition or version than the one configured. A config without such
//! judges never runs any of this, which is what keeps agg working without swe-metrics installed.
//!
//! WHERE it is checked is where it will run ([`Site`]): on the host's `PATH`, or — when the run's
//! judges are containerized — in the judges' image, by short probe containers (no network, the
//! project read-only, as the project owner). A container run never needs it on the host.

use super::measure::PROGRAM;
use super::report::REPORT_SCHEMA;
use super::score::{ScoreDoc, SCORE_SCHEMA};
use super::spec::{Definition, ScoreJudge};
use crate::core::config::AggConfig;
use crate::problem::Problem;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How long one probe may take on the host (python's start-up, not a measurement).
const PROBE_SECS: u64 = 60;
/// …and in a container, where the first one may PULL the image.
const IMAGE_PROBE_SECS: u64 = 600;

/// Where the run's swe-metrics runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Site {
    /// on the host, from `PATH` (`none` and `sandbox` runs).
    Host,
    /// in the judges' image (`container` runs): the image, the config key that chose it, and the
    /// project dir (mounted read-only, as a measurement sees it).
    Image { image: String, at: String, project: PathBuf },
}

impl Site {
    /// Where `cfg`'s judges run swe-metrics; `None` = nowhere this probe can reach — a container run
    /// with no engine or no single judges' image, each refused at its own key already.
    pub fn of(cfg: &AggConfig, project: &Path) -> Option<Site> {
        if cfg.run_isolation() != crate::isolation::Isolation::Container {
            return Some(Site::Host);
        }
        let image = cfg.judge_image().ok()?;
        if !crate::isolation::container_available() {
            return None;
        }
        let at = crate::isolation::probe::judge_image_key(cfg).to_string();
        Some(Site::Image { image, at, project: std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf()) })
    }

    /// The config key a missing or incompatible swe-metrics is reported at: the image's, or (on
    /// the host, where no key chose it) `fallback`.
    fn at(&self, fallback: &str) -> String {
        match self {
            Site::Host => fallback.to_string(),
            Site::Image { at, .. } => at.clone(),
        }
    }

    /// How to install a compatible one, for messages.
    fn fix(&self) -> String {
        match self {
            Site::Host => "install a compatible swe-metrics".into(),
            Site::Image { image, .. } => format!("build the judges' image `{image}` with a compatible swe-metrics (docker/swe-metrics/)"),
        }
    }
}

/// Why a probe call failed.
#[derive(Debug)]
enum Fail {
    /// there is no swe-metrics where it would run.
    Missing,
    Other(String),
}

/// Run `swe-metrics <args>` for a probe at `site`, with `mounts` visible read-only in a container:
/// its stdout, or why it could not.
fn call(site: &Site, args: &[&str], mounts: &[PathBuf]) -> Result<String, Fail> {
    let (cmd, secs) = match site {
        Site::Host => {
            let mut c = Command::new(PROGRAM);
            c.args(args);
            (c, PROBE_SECS)
        }
        Site::Image { image, project, .. } => (in_image(image, project, args, mounts), IMAGE_PROBE_SECS),
    };
    let mut cmd = cmd;
    cmd.stdin(Stdio::null());
    let out = crate::os::proc::run_with_timeout(cmd, secs).map_err(|e| {
        if *site == Site::Host && (e.contains("No such file") || e.contains("not found")) {
            Fail::Missing
        } else {
            Fail::Other(format!("`{PROGRAM} {}` could not run: {e}", args.join(" ")))
        }
    })?;
    if !out.success {
        let err = String::from_utf8_lossy(&out.stderr);
        if *site != Site::Host && no_executable(&err) {
            return Err(Fail::Missing);
        }
        return Err(Fail::Other(format!("`{PROGRAM} {}` failed: {}", args.join(" "), err.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The probe container: no network, the project read-only (a definition file under `agg/` is read
/// from there), `mounts` read-only, as the project owner, `HOME` in the container's tmp.
fn in_image(image: &str, project: &Path, args: &[&str], mounts: &[PathBuf]) -> Command {
    let mut c = Command::new(crate::isolation::container_engine().unwrap_or("docker"));
    c.args(["run", "--rm", "--network", "none", "-e", "HOME=/tmp", "-e", "PYTHONDONTWRITEBYTECODE=1"]);
    if let Some(u) = crate::isolation::container::owner(project) {
        c.arg("--user").arg(u);
    }
    for p in std::iter::once(project).chain(mounts.iter().map(PathBuf::as_path)) {
        c.arg("-v").arg(format!("{p}:{p}:ro", p = p.display()));
    }
    c.arg("-w").arg(project).arg(image).arg(PROGRAM).args(args);
    c
}

/// The engine's word for "the image has no such command" — docker/runc and podman/crun.
fn no_executable(stderr: &str) -> bool {
    stderr.contains(PROGRAM) && stderr.contains("executable file") && stderr.contains("not found")
}

/// The problem a probe failure is, at `at`.
fn problem(site: &Site, at: &str, f: Fail) -> Problem {
    match (f, site) {
        (Fail::Missing, Site::Host) => Problem::new(
            at,
            format!("`{PROGRAM}` is not on PATH — metric and score judges need swe-metrics installed (it is optional otherwise)"),
        ),
        (Fail::Missing, Site::Image { image, at, .. }) => crate::isolation::probe::missing_cli(at, image, PROGRAM),
        (Fail::Other(e), _) => Problem::new(at, e),
    }
}

/// The `$id` of `swe-metrics schema <which>` must be the one this agg reads.
fn schema(site: &Site, which: &str, want: &str) -> Result<(), Fail> {
    let text = call(site, &["schema", which], &[])?;
    let id = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("$id").and_then(|i| i.as_str()).map(String::from))
        .unwrap_or_default();
    if id == want {
        Ok(())
    } else {
        Err(Fail::Other(format!("swe-metrics speaks the {which} schema `{id}`, but this agg reads `{want}` — {}", site.fix())))
    }
}

/// A score of an EMPTY report: what definition and version the configured one really is.
fn definition_of(site: &Site, j: &ScoreJudge) -> Result<ScoreDoc, String> {
    // in a container the report must be where the engine sees it: under the (mounted) project.
    let dir = match site {
        Site::Host => std::env::temp_dir(),
        Site::Image { project, .. } => crate::paths::container_dir(project).join("judge"),
    }
    .join(format!("agg-swe-metrics-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("probe dir: {e}"))?;
    let report = dir.join("empty-report.json");
    let empty = serde_json::json!({"schema": REPORT_SCHEMA, "subject": {"target": ".", "kind": "directory", "commit": null,
        "changed_since": null, "files": 0, "languages": {}}, "analyzers": [], "metrics": [], "details": []});
    std::fs::write(&report, empty.to_string()).map_err(|e| format!("probe report: {e}"))?;
    let mut args = vec!["score".to_string(), report.display().to_string()];
    args.extend(j.definition.args());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mounts: Vec<PathBuf> = match (site, &j.definition) {
        (Site::Image { project, .. }, Definition::File(p)) => super::contained::outside_project(project, p).into_iter().collect(),
        _ => vec![],
    };
    let out = call(site, &refs, &mounts);
    let _ = std::fs::remove_dir_all(&dir);
    match out {
        Ok(text) => ScoreDoc::parse(&text),
        Err(Fail::Missing) => Err(format!("`{PROGRAM}` is missing")),
        Err(Fail::Other(e)) => Err(e),
    }
}

/// Every startup problem of the metric/score judges named here (`metric` names, `score` names with
/// their specs), probed at `site`: a missing or incompatible swe-metrics at the key that chose where
/// it runs (`judges.<first>` on the host), a definition mismatch at `judges.<name>.score`. Empty when
/// there are none — swe-metrics is then never called. `trivy`: a judge reads trivy's metrics.
pub fn problems(metric: &[&str], score: &[(&str, &ScoreJudge)], site: &Site, trivy: bool) -> Vec<Problem> {
    let Some(first) = metric.first().or(score.first().map(|(n, _)| n)) else { return vec![] };
    let names: Vec<&str> = metric.iter().copied().chain(score.iter().map(|(n, _)| *n)).collect();
    let at = site.at(&format!("judges.{first}"));
    if let Err(f) = schema(site, "report", REPORT_SCHEMA) {
        let mut p = problem(site, &at, f);
        if *site == Site::Host {
            p.message = format!("{} (judges: {})", p.message, names.join(", "));
        }
        return vec![p];
    }
    if trivy && matches!(site, Site::Image { .. }) && super::contained::trivy_cache().is_none() {
        // not a refusal: a project with no manifests never runs trivy at all.
        eprintln!(
            "  ⚠ no trivy vulnerability database at {} — in the judges' image swe-metrics reads the host's, read-only, \
             and every `dependencies.*` value (a score's vulnerability gate too) fails until it is there: \
             `trivy image --download-db-only` (docs/CONFIG.md → metric judges)",
            super::contained::trivy_expected()
        );
    }
    if score.is_empty() {
        return vec![];
    }
    if let Err(f) = schema(site, "score", SCORE_SCHEMA) {
        return vec![problem(site, &at, f)];
    }
    score
        .iter()
        .filter_map(|(name, j)| {
            let checked = definition_of(site, j).and_then(|d| d.check_definition(j));
            checked.err().map(|e| Problem::new(format!("judges.{name}.score"), e))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> AggConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// Off the container tier swe-metrics is the host's; on it, the judges' image's, reported at the
    /// key that chose that image.
    #[test]
    fn the_probe_goes_where_swe_metrics_will_run() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Site::of(&cfg("project: p\nsteps: { a: { isolation: sandbox } }\n"), tmp.path()), Some(Site::Host));
        let c = cfg("project: p\nsteps: { a: { isolation: container, image: \"w:1\" } }\njudge: { image: \"j:1\" }\n");
        match Site::of(&c, tmp.path()) {
            None => assert!(!crate::isolation::container_available(), "only without an engine"),
            Some(Site::Image { image, at, .. }) => assert_eq!((image.as_str(), at.as_str()), ("j:1", "judge.image")),
            Some(s) => panic!("{s:?}"),
        }
    }

    /// The probe container: no network, the project read-only and the working dir, the image, then
    /// swe-metrics with its arguments.
    #[test]
    fn the_image_probe_is_offline_and_reads_the_project_read_only() {
        let p = Path::new("/proj");
        let c = in_image("j:1", p, &["schema", "report"], &[PathBuf::from("/defs")]);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["--network", "none"]), "{args:?}");
        assert!(args.contains(&"/proj:/proj:ro".to_string()) && args.contains(&"/defs:/defs:ro".to_string()), "{args:?}");
        assert_eq!(args[args.len() - 4..], ["j:1", PROGRAM, "schema", "report"]);
    }

    #[test]
    fn the_engines_missing_executable_is_recognized() {
        assert!(no_executable(r#"exec: "swe-metrics": executable file not found in $PATH: unknown"#));
        assert!(no_executable("Error: crun: executable file `swe-metrics` not found in $PATH: No such file or directory"));
        assert!(!no_executable("Unable to find image 'x:1' locally"));
    }

    /// A missing swe-metrics in the image is the image probe's problem, spelled the same way, so
    /// `agg check` lists it once.
    #[test]
    fn a_missing_swe_metrics_in_the_image_is_the_image_probes_problem() {
        let site = Site::Image { image: "j:1".into(), at: "judge.image".into(), project: PathBuf::from("/p") };
        assert_eq!(problem(&site, "judge.image", Fail::Missing), crate::isolation::probe::missing_cli("judge.image", "j:1", PROGRAM));
        let host = problem(&Site::Host, "judges.cc", Fail::Missing);
        assert!(host.path == "judges.cc" && host.message.contains("not on PATH"), "{host:?}");
        assert_eq!(site.at("judges.cc"), "judge.image");
        assert!(site.fix().contains("docker/swe-metrics/"));
    }
}
