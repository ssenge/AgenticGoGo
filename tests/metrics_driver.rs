//! Metric and score judges on the RUST DRIVER path (A-20): `Judge::metric` / `Judge::score` run
//! through the same engine as the YAML path — one shared `swe-metrics measure` per tree, the
//! ratchet, the incumbent.
//!
//! A separate test binary because it puts a fake `swe-metrics` on the PROCESS PATH (the facade runs
//! in-process), which must not leak into `tests/metrics.rs`' real-tool test.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use agg::prelude::*;

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    p
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).output().unwrap().status.success(), "git {args:?}");
}

/// The fakes: a worker that makes p95 worse (9) by rewriting the tracked report, and a swe-metrics
/// that logs its calls to `$CALLS` in the project and "measures" `fake-report.json`.
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-metrics-driver-{}", std::process::id()));
    let bin = root.join("bin");
    fs::create_dir_all(root.join("home")).unwrap();
    chmod_x(&write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sed 's/"p95": 6/"p95": 9/' fake-report.json > r.tmp && mv r.tmp fake-report.json
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.0}'
"#,
    ));
    chmod_x(&write(
        &bin,
        "swe-metrics",
        r#"#!/bin/sh
echo "$*" >> "$AGG_PROJECT_DIR/../calls.log"
cmd=$1; shift
case "$cmd" in
  measure)
    target=$1; shift; out=
    while [ $# -gt 0 ]; do case "$1" in --out) out=$2; shift ;; esac; shift; done
    cp "$target/fake-report.json" "$out"; exit 0 ;;
  score)
    out=
    while [ $# -gt 0 ]; do case "$1" in --out) out=$2; shift ;; esac; shift; done
    echo '{"schema":"swe-metrics/score/v1","definition":{"name":"swe-default","version":"2"},"score":77,"gates":[],"components":[],"regressions":[],"notes":[]}' > "$out"
    exit 0 ;;
esac
exit 2
"#,
    ));
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    std::env::set_var("HOME", root.join("home"));
    root
});

const REPORT: &str = r#"{"schema":"swe-metrics/report/v1","analyzers":[
 {"id":"lizard","tool":"lizard","status":"ran","families":["size","complexity"]},
 {"id":"jscpd","tool":"jscpd","status":"ran","families":["duplication"]}],
 "metrics":[
 {"id":"complexity.cyclomatic","tool":"lizard","unit":"x","direction":"lower_better","scope":{"level":"target"},"stats":{"count":2,"sum":7,"mean":3.5,"p50":1,"p95": 6,"max":6}},
 {"id":"duplication.percent","tool":"jscpd","unit":"%","direction":"lower_better","scope":{"level":"target"},"value":2.0}],
 "details":[]}"#;

/// The project dir (inside a tempdir that also holds `calls.log`).
fn project() -> (tempfile::TempDir, PathBuf) {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("proj");
    fs::create_dir_all(&dir).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(&dir, args);
    }
    write(&dir, "fake-report.json", REPORT);
    write(&dir, ".gitignore", "agg/state/\nagg/private/\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "base"]);
    (tmp, dir)
}

fn measures(dir: &Path) -> usize {
    fs::read_to_string(dir.join("../calls.log")).unwrap_or_default().lines().filter(|l| l.starts_with("measure")).count()
}

/// Two metric judges and a score judge asked in one span share ONE measurement; the ratchet on the
/// metric judge (down, the driver's own) rolls back a span that makes p95 worse.
#[test]
fn driver_metric_judges_share_a_measurement_and_ratchet() {
    let (_tmp, dir) = project();
    let cc = Judge::metric("cc", MetricJudge::new("complexity.cyclomatic").stat(Stat::P95).tool("lizard").max(10.0));
    let dup = Judge::metric("dup", MetricJudge::new("duplication.percent").max(3.0));
    let q = Judge::score("q", ScoreJudge::new("swe-default", "2").min(50.0));
    let agg = Agg::open(&dir).unwrap().ratchet("cc", Ratchet::down(0.0));

    assert_eq!(agg.judge(&cc).value(), Some(6.0), "the pre-step ask is the baseline");
    assert_eq!(agg.judge(&dup).value(), Some(2.0));
    let score = agg.judge(&q);
    assert_eq!((score.value(), score.met()), (Some(77.0), true), "{}", score.rationale());
    assert_eq!(measures(&dir), 1, "three judges, one measurement");

    agg.step(&Step::new("implement").model("fake").prompt("work")).unwrap();
    let worse = agg.judge(&cc);
    assert_eq!(worse.value(), Some(9.0), "{}", worse.rationale());
    agg.judge(&dup);
    assert_eq!(measures(&dir), 2, "the changed tree is measured once more");
    assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack, "p95 6 → 9 under a zero-tolerance ratchet");
}
