//! Metric and score judges (A-20) on the container tier (A-11), end to end: the real `agg` binary, a
//! REAL container engine, the worker in the fake-agent image and swe-metrics in a judges' test image
//! with a FAKE `swe-metrics` baked in (`tests/fixtures/container/fake-metrics`).
//!
//! What this proves:
//! - every `swe-metrics` call runs INSIDE `judge.image` — the host's PATH has a stub that fails and
//!   leaves a mark if anything calls it, startup probe included;
//! - the project is READ-ONLY to it, its home is fresh, it writes the session's judge scratch;
//! - ONE measurement per judged tree, shared by a metric and a score judge;
//! - a score is taken against the incumbent's report, which the container reads from
//!   `agg/private/metrics/incumbent/`, and agg keeps it as before;
//! - the host's trivy database is mounted READ-ONLY, with updates off;
//! - startup refuses a judges' image without swe-metrics at `judge.image`, once, and `agg doctor
//!   --json` reports swe-metrics under `container_images`.
//!
//! Skipped LOUDLY without a running engine. Projects live under `target/`, which every engine agg
//! supports can see.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn write(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `(worker image, judges' image)`, built once per test process. `None` = no engine (skip).
fn images() -> Option<(&'static str, &'static str)> {
    static IMAGES: OnceLock<Option<(String, String)>> = OnceLock::new();
    IMAGES
        .get_or_init(|| {
            if !agg::isolation::container_available() {
                return None;
            }
            let engine = agg::isolation::container_engine().unwrap_or("docker");
            let build = |tag: &str, dir: &str| {
                let ctx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container").join(dir);
                let out = Command::new(engine).args(["build", "-q", "-t", tag]).arg(&ctx).output().unwrap();
                assert!(out.status.success(), "building {tag}: {}", String::from_utf8_lossy(&out.stderr));
                tag.to_string()
            };
            Some((build("agg-test-fake-agent:1", "fake-agent"), build("agg-test-fake-metrics:1", "fake-metrics")))
        })
        .as_ref()
        .map(|(w, j)| (w.as_str(), j.as_str()))
}

/// A scratch root under `target/` — visible to every engine — removed when dropped.
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        if std::env::var_os("KEEP").is_none() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn root(name: &str) -> Root {
    let r = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/agg-ctr-test/metrics-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&r);
    fs::create_dir_all(&r).unwrap();
    Root(fs::canonicalize(&r).unwrap())
}

/// A host stub that must never run: it leaves a mark and fails.
const HOST_STUB: &str = "#!/bin/sh\ntouch \"$(dirname \"$0\")/../HOST_SWE_METRICS_CALLED\"\necho 'host swe-metrics called' >&2\nexit 1\n";

struct Ctr {
    root: Root,
    dir: PathBuf,
    path: String,
    trivy: PathBuf,
}

impl Ctr {
    /// A container-tier project: the worker in `worker`, the judges in `judges`, a metric judge
    /// (`sessions ≥ 2`) and a score judge (`≥ 60`), the host stub first on PATH, and a seeded trivy
    /// cache the run is pointed at.
    fn new(name: &str, worker: &str, judges: &str) -> Ctr {
        let root = root(name);
        let d = root.0.join("proj");
        chmod_x(&{
            write(&root.0, "hostbin/swe-metrics", HOST_STUB);
            root.0.join("hostbin/swe-metrics")
        });
        let trivy = root.0.join("trivy-cache");
        write(&trivy, "db/trivy.db", "fake db\n");
        git(&root.0, &["init", "-q", "-b", "main", d.to_str().unwrap()]);
        git(&d, &["config", "user.email", "t@t"]);
        git(&d, &["config", "user.name", "t"]);
        write(&d, ".gitignore", "agg/state/\nagg/private/\n");
        write(
            &d,
            "agg/agg.yaml",
            &format!(
                "project: \"mctr\"\n\
                 defaults: {{ model: \"fake\", isolation: \"container\", image: \"{worker}\" }}\n\
                 judge: {{ image: \"{judges}\" }}\n\
                 judges:\n  \
                   sessions: {{ metric: {{ id: \"work.sessions\" }}, min: 2 }}\n  \
                   quality: {{ score: {{ definition: \"swe-default\", version: \"2\" }}, min: 60 }}\n\
                 steps: {{ worker: {{}} }}\n\
                 sequence: {{ steps: [\"worker\"], done_if: \"sessions AND quality\" }}\n\
                 summary: {{ enabled: false }}\n"
            ),
        );
        write(&d, "agg/state/STATE.md", "work\n");
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-qm", "init"]);
        let path = format!("{}:{}", root.0.join("hostbin").display(), std::env::var("PATH").unwrap_or_default());
        Ctr { root, dir: d, path, trivy }
    }

    fn agg(&self, args: &[&str]) -> (Option<i32>, String) {
        let (code, _, all) = self.agg_split(args);
        (code, all)
    }

    /// `(exit code, stdout, stdout + stderr)`.
    fn agg_split(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let mut c = Command::cargo_bin("agg").unwrap();
        c.args(args).current_dir(&self.dir).env("PATH", &self.path).env("HOME", self.root.0.join("home")).env("TRIVY_CACHE_DIR", &self.trivy);
        // HOME is redirected, which would hide the engine client's config (colima's context).
        if std::env::var_os("DOCKER_CONFIG").is_none() {
            if let Some(h) = std::env::var_os("HOME") {
                c.env("DOCKER_CONFIG", Path::new(&h).join(".docker"));
            }
        }
        let out = c.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.code(), stdout.clone(), format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)))
    }

    fn host_called(&self) -> bool {
        self.root.0.join("HOST_SWE_METRICS_CALLED").exists()
    }

    /// The fake's log of one session's scratch (`0` = the baseline pass).
    fn log(&self, session: u32) -> String {
        let p = self.dir.join(format!("agg/private/container/judge/scratch-{session}/swe-metrics.log"));
        fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }
}

#[test]
fn metric_and_score_judges_measure_in_the_judges_image_never_on_the_host() {
    let Some((worker, judges)) = images() else {
        eprintln!("SKIPPED: no container engine");
        return;
    };
    let p = Ctr::new("run", worker, judges);
    let (code, out) = p.agg(&["run", "--max-sessions", "3"]);
    assert_eq!(code, Some(0), "two sessions meet `sessions AND quality`:\n{out}");
    assert!(!p.host_called(), "swe-metrics ran on the HOST:\n{out}");

    let uid = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&p.dir).unwrap().uid()
    };
    for s in 0..=2 {
        let log = p.log(s);
        assert_eq!(log.matches("measure ").count(), 1, "ONE measurement per judged tree, shared by both judges (scratch-{s}):\n{log}");
        for want in [
            "MEASURE_IN_CONTAINER",
            "MEASURE_PROJECT_WRITE_DENIED",
            "SCORE_IN_CONTAINER",
            "TRIVY_NO_UPDATE",
            "TRIVY_DB_SEEN",
            "TRIVY_DB_WRITE_DENIED",
            &format!("uid={uid} "),
            &format!("home={}", p.dir.join("agg/private/container/home/metrics-").display()),
        ] {
            assert!(log.contains(want), "scratch-{s}: `{want}` missing\n{log}\n{out}");
        }
    }
    assert!(p.log(0).contains("score baseline=none"), "the baseline pass has no incumbent:\n{}", p.log(0));
    for s in 1..=2 {
        let log = p.log(s);
        let incumbent = p.dir.join("agg/private/metrics/incumbent/target.json");
        assert!(log.contains(&format!("score baseline={}", incumbent.display())), "scored against the incumbent:\n{log}");
        assert!(log.contains("BASELINE_READ"), "the container reads the incumbent's report:\n{log}");
    }
    // the incumbent agg kept is the last kept tree's report — two sessions in
    let kept = fs::read_to_string(p.dir.join("agg/private/metrics/incumbent/target.json")).unwrap();
    assert!(kept.contains("\"value\":2"), "{kept}");
    // the project stayed clean: nothing the container tried to write landed
    assert!(!p.dir.join("MEASURE_probe").exists() && !p.dir.join("SCORE_probe").exists());
    // evidence: the shared report, kept from the container's scratch
    let ev = fs::read_dir(p.dir.join("agg/private/evidence")).unwrap().flatten().next().unwrap().path();
    assert!(ev.join("2/sessions/swe-metrics/target/report.json").is_file(), "the report is kept as evidence: {}", ev.display());

    // `agg judge` on a container run measures in the image too — the host need not have swe-metrics
    let (code, out) = p.agg(&["judge", "sessions"]);
    assert!(code == Some(0) && out.contains("work.sessions"), "{out}");
    assert!(!p.host_called(), "`agg judge` ran swe-metrics on the host:\n{out}");
}

#[test]
fn startup_refuses_a_judges_image_without_swe_metrics_and_doctor_reports_it() {
    let Some((worker, judges)) = images() else {
        eprintln!("SKIPPED: no container engine");
        return;
    };
    // the worker's image has no swe-metrics: refused at the key that chose the judges' image, once
    let bad = Ctr::new("refuse", worker, worker);
    let (code, json, out) = bad.agg_split(&["check", "--json"]);
    assert_eq!(code, Some(1), "{out}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let missing: Vec<&serde_json::Value> =
        v["problems"].as_array().unwrap().iter().filter(|p| p["message"].as_str().unwrap_or("").contains("has no `swe-metrics` on PATH")).collect();
    assert_eq!(missing.len(), 1, "listed exactly once: {v:#}");
    assert_eq!(missing[0]["path"], "judge.image", "{v:#}");
    let (code, out) = bad.agg(&["run", "--max-sessions", "1"]);
    assert_ne!(code, Some(0), "the run must not start:\n{out}");
    assert!(out.contains("has no `swe-metrics` on PATH"), "{out}");
    assert!(!bad.host_called(), "the probe asked the HOST:\n{out}");

    // the image that has it passes, and doctor says so under container_images
    let good = Ctr::new("doctor", worker, judges);
    let (_, json, out) = good.agg_split(&["doctor", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let img = v["container_images"].as_array().unwrap().iter().find(|i| i["image"] == judges).unwrap_or_else(|| panic!("{v:#}"));
    assert_eq!(img["clis"]["swe-metrics"], "fake-swe-metrics 1.0.0", "{v:#}");
    assert!(v["problems"].as_array().unwrap().is_empty(), "{v:#}");
    assert!(!good.host_called());
}
