//! Judges built on swe-metrics (A-20), end to end through the real `agg` binary.
//!
//! Most tests put a FAKE `swe-metrics` on PATH: a shell stub that logs every invocation, answers the
//! schema probes, "measures" by copying `fake-report.json` out of the measured directory and
//! "scores" by copying `fake-score.json` — so a fake worker changes the measurement by rewriting a
//! tracked file, and the log proves how often agg measured. One test runs the REAL swe-metrics on a
//! fixture with known complexity and duplication, and skips (saying so) when it is not installed.
//!
//! Unix-only (the fakes are shell scripts), like the rest of the suite.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agg::core::verdicts::{rows_for, Outcome, RatchetMark};

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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The fake swe-metrics. `{LOG}` and `{BIN}` are filled in by [`Project::new`].
const STUB: &str = r#"#!/bin/sh
echo "$*" >> '{LOG}'
cmd=$1; shift
case "$cmd" in
  schema)
    id=$(cat "{BIN}/schema-id-$1" 2>/dev/null || echo "swe-metrics/$1/v1")
    printf '{"$id": "%s"}\n' "$id"; exit 0 ;;
  measure)
    target=$1; shift; out=; det=; src=fake-report.json
    while [ $# -gt 0 ]; do case "$1" in --out) out=$2; shift ;; --details) det=$2; shift ;; --changed-since) [ -f "$target/fake-changed.json" ] && src=fake-changed.json; shift ;; esac; shift; done
    [ -f "$target/$src" ] || { echo "no $src in $target" >&2; exit 2; }
    cp "$target/$src" "$out"
    mkdir -p "$det"; echo '{"runs":[]}' > "$det/gitleaks.sarif"
    exit 0 ;;
  score)
    out=
    while [ $# -gt 0 ]; do case "$1" in --out) out=$2; shift ;; esac; shift; done
    src="$PWD/fake-score.json"; [ -f "$src" ] || src="{BIN}/default-score.json"
    if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
    exit 0 ;;
esac
exit 2
"#;

/// The fake worker: runs one line of `plan` per session (the plan sits outside the project, so a
/// rollback cannot replay it). agg commits the worker's tracked edits.
const CLAUDE: &str = r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
line=$(head -n 1 '{BIN}/plan')
tail -n +2 '{BIN}/plan' > '{BIN}/plan.rest' && mv '{BIN}/plan.rest' '{BIN}/plan'
sh -c "$line"
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.0}'
exit 0
"#;

/// One ledger row as the tests read it: `(session, outcome, value, met, error)`.
type Row = (Option<u32>, Outcome, Option<f64>, bool, Option<String>);

struct Project {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    /// PATH with the fakes first.
    path: String,
}

impl Project {
    /// A git project with the fake worker running `plan`, and (with `stub`) the fake swe-metrics.
    fn new(plan: &[&str], stub: bool) -> Project {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let dir = tmp.path().join("proj");
        let log = tmp.path().join("swe-metrics.log");
        fs::create_dir_all(&dir).unwrap();
        let fill = |s: &str| s.replace("{LOG}", &log.display().to_string()).replace("{BIN}", &bin.display().to_string());
        write(&bin, "plan", &fill(&format!("{}\n", plan.join("\n"))));
        chmod_x(&write(&bin, "claude", &fill(CLAUDE)));
        if stub {
            chmod_x(&write(&bin, "swe-metrics", &fill(STUB)));
            write(&bin, "default-score.json", &score(json!(null), "2", json!([]), &["no report"]).to_string());
        }
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            git(&dir, args);
        }
        // the fakes first, then only the system dirs — a real swe-metrics elsewhere on PATH must not leak in.
        let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
        Project { _tmp: tmp, dir, bin, log, path }
    }

    fn config(&self, judges: &str, seq: &str) {
        write(
            &self.dir,
            "agg/agg.yaml",
            &format!(
                "project: metrics\ndefaults: {{ model: fake }}\njudges:\n{judges}steps:\n  worker: {{}}\n\
                 sequence:\n  steps: [worker]\n{seq}summary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"
            ),
        );
        // a never-met DoD goal, so a run goes to its session cap.
        chmod_x(&write(&self.dir, "agg/judges/feature.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1}'\n"));
    }

    /// A report the next sessions' plan lines can copy over `fake-report.json`.
    fn stage_report(&self, name: &str, r: &Value) {
        write(&self.bin, name, &r.to_string());
    }

    fn commit(&self) {
        write(&self.dir, ".gitignore", "agg/state/\nagg/private/\n");
        git(&self.dir, &["add", "-A"]);
        git(&self.dir, &["commit", "-qm", "base"]);
    }

    fn agg(&self, args: &[&str]) -> (Option<i32>, String) {
        let out = Command::cargo_bin("agg")
            .unwrap()
            .current_dir(&self.dir)
            .env("PATH", &self.path)
            .env("HOME", self.dir.join(".home"))
            .args(args)
            .output()
            .unwrap();
        (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    /// Every swe-metrics invocation, in order.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log).unwrap_or_default().lines().map(String::from).collect()
    }

    fn calls_of(&self, cmd: &str) -> Vec<String> {
        self.calls().into_iter().filter(|c| c.starts_with(cmd)).collect()
    }

    /// `(session, outcome, value, met, error)` per row of `judge`.
    fn rows(&self, judge: &str) -> Vec<Row> {
        rows_for(&self.dir, judge, false).into_iter().map(|r| (r.session, r.outcome, r.value, r.met, r.error)).collect()
    }
}

/// One analyzer of a report: `(id, tool, status, reason, families)`.
fn analyzer(id: &str, tool: &str, status: &str, reason: &str, families: &[&str]) -> Value {
    json!({"id": id, "tool": tool, "status": status, "reason": reason, "families": families})
}

fn row(id: &str, tool: &str, direction: &str, v: Value) -> Value {
    let mut r = json!({"id": id, "tool": tool, "unit": "x", "direction": direction, "scope": {"level": "target"}});
    for (k, x) in v.as_object().unwrap() {
        r[k] = x.clone();
    }
    r
}

/// The fixture report: cyclomatic p95 `cc` (lizard), cognitive p95 4 (complexipy) and 5
/// (swe-metrics), duplication 2 %, no secrets, three LOW vulnerabilities (none critical/high).
fn report(cc: f64) -> Value {
    json!({
        "schema": "swe-metrics/report/v1",
        "subject": {"target": ".", "kind": "repository", "commit": null, "changed_since": null, "files": 3, "languages": {}},
        "analyzers": [
            analyzer("lizard", "lizard", "ran", "", &["size", "complexity"]),
            analyzer("complexipy", "complexipy", "ran", "", &["complexity"]),
            analyzer("cognitive", "swe-metrics", "ran", "", &["complexity"]),
            analyzer("jscpd", "jscpd", "ran", "", &["duplication"]),
            analyzer("gitleaks", "gitleaks", "ran", "", &["secrets"]),
            analyzer("trivy", "trivy", "ran", "", &["dependencies"]),
        ],
        "metrics": [
            row("complexity.cyclomatic", "lizard", "lower_better", json!({"stats": {"count": 3, "sum": 9, "mean": 3, "p50": 2, "p95": cc, "max": cc}})),
            row("complexity.cognitive", "complexipy", "lower_better", json!({"stats": {"count": 3, "sum": 6, "mean": 2, "p50": 2, "p95": 4, "max": 4}})),
            row("complexity.cognitive", "swe-metrics", "lower_better", json!({"stats": {"count": 3, "sum": 7, "mean": 2, "p50": 2, "p95": 5, "max": 5}})),
            row("duplication.percent", "jscpd", "lower_better", json!({"value": 2.0})),
            row("secrets.findings", "gitleaks", "lower_better", json!({"value": 0.0})),
            row("dependencies.vulnerabilities", "trivy", "lower_better", json!({"value": 3.0})),
            row("dependencies.vulnerabilities", "trivy", "lower_better", json!({"value": 3.0, "labels": {"severity": "low"}})),
        ],
        "details": [{"analyzer": "gitleaks", "format": "sarif", "path": "gitleaks.sarif"}],
    })
}

fn score(value: Value, version: &str, gates: Value, notes: &[&str]) -> Value {
    json!({"schema": "swe-metrics/score/v1", "definition": {"name": "swe-default", "version": version},
           "score": value, "gates": gates, "components": [], "regressions": [], "notes": notes})
}

const FIVE: &str = "  cc_p95: { metric: { id: \"complexity.cyclomatic\", stat: \"p95\", tool: \"lizard\" }, max: 10 }\n  \
                    cog: { metric: { id: \"complexity.cognitive\", stat: \"p95\", combine: \"strictest\" }, max: 15 }\n  \
                    dup: { metric: { id: \"duplication.percent\" }, max: 3 }\n  \
                    secrets: { metric: { id: \"secrets.findings\" }, max: 0 }\n  \
                    vulns: { metric: { id: \"dependencies.vulnerabilities\", labels: { severity: [\"critical\", \"high\"] } }, max: 0 }\n";

/// ACCEPTANCE: one session with FIVE metric judges runs `swe-metrics measure` ONCE — one measurement
/// per judged tree (the baseline pass, then each session) — and each judge reads its number out of
/// it: a tool's statistic, the strictest of two tools, a value, a label sum that is a measured 0.
/// The shared report and the judge's SARIF are kept as evidence.
#[test]
fn five_metric_judges_share_one_measurement_per_session() {
    let p = Project::new(&["cp '{BIN}/r7.json' fake-report.json"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    p.stage_report("r7.json", &report(7.0));
    p.config(FIVE, "  done_if: \"feature\"\n  invariants: [cc_p95, cog, dup, secrets, vulns]\n");
    p.commit();

    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(4), "the never-met DoD runs to the cap:\n{out}");
    assert_eq!(p.calls_of("measure").len(), 2, "baseline + session 1, five judges each: {:?}", p.calls());
    assert_eq!(p.calls_of("schema report").len(), 1, "probed once, at startup");
    let only = &p.calls_of("measure")[0];
    for f in ["--only complexity", "--only dependencies", "--only duplication", "--only secrets"] {
        assert!(only.contains(f), "the shared measurement covers every judge's family: {only}");
    }

    let value = |j: &str| p.rows(j).iter().map(|r| r.2).collect::<Vec<_>>();
    assert_eq!(value("cc_p95"), [Some(6.0), Some(7.0)], "baseline 6, session 1 measured 7");
    assert_eq!(value("cog"), [Some(5.0), Some(5.0)], "the strictest of complexipy 4 and swe-metrics 5");
    assert_eq!(value("dup"), [Some(2.0), Some(2.0)]);
    assert_eq!(value("vulns"), [Some(0.0), Some(0.0)], "no critical/high row, the total is there: a measured 0");
    for j in ["cc_p95", "cog", "dup", "secrets", "vulns"] {
        assert!(p.rows(j).iter().all(|r| r.3 && r.4.is_none()), "`{j}` met on every row: {:?}", p.rows(j));
    }

    // A-16: the shared report and the secrets family's SARIF are kept for session 1's verdict.
    let kept: Vec<String> = rows_for(&p.dir, "secrets", false).last().unwrap().evidence_saved.clone();
    assert!(kept.iter().any(|f| f.ends_with("swe-metrics/target/report.json")), "{kept:?}");
    assert!(kept.iter().any(|f| f.ends_with("details/gitleaks.sarif")), "{kept:?}");
    assert!(kept.iter().all(|f| p.dir.join(f).is_file()), "{kept:?}");
    assert!(p.dir.join("agg/private/metrics/incumbent/target.json").is_file(), "the incumbent's report is kept");
}

/// ACCEPTANCE: a RATCHETED metric judge rolls back a session that makes the p95 worse — its ratchet
/// names no direction (the judge's `max:` says lower is better, and the report agrees) — and keeps
/// one that improves it.
#[test]
fn a_ratcheted_metric_judge_rolls_back_a_worse_p95() {
    let p = Project::new(&["cp '{BIN}/r9.json' fake-report.json", "cp '{BIN}/r5.json' fake-report.json"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    p.stage_report("r9.json", &report(9.0));
    p.stage_report("r5.json", &report(5.0));
    p.config(FIVE, "  done_if: \"feature\"\n  ratchet:\n    cc_p95: { tolerance: 0.5 }\n");
    p.commit();

    let (code, out) = p.agg(&["run", "--max-sessions", "2"]);
    assert_eq!(code, Some(4), "{out}");
    let rows: Vec<_> = rows_for(&p.dir, "cc_p95", false).into_iter().map(|r| (r.session, r.outcome, r.ratchet)).collect();
    assert_eq!(
        rows,
        [
            (None, Outcome::Baseline, None),
            (Some(1), Outcome::RolledBack, Some(RatchetMark { from: Some(6.0), to: Some(9.0) })),
            (Some(2), Outcome::Merged, None),
        ],
        "{out}"
    );
    assert!(out.contains("ratchet `cc_p95`: 6 → 9"), "{out}");
    assert_eq!(agg::core::ratchet::load(&p.dir).unwrap().reference("cc_p95"), Some(5.0));
}

/// ACCEPTANCE: a SKIPPED analyzer makes the judge ERROR with the analyzer's reason — never pass. The
/// DoD never holds, and the error is on every row.
#[test]
fn a_skipped_analyzer_makes_the_judge_error_not_pass() {
    let p = Project::new(&["true"], true);
    let mut r = report(6.0);
    r["analyzers"][5] = analyzer("trivy", "trivy", "skipped", "no applicable files", &["dependencies"]);
    r["metrics"] = json!(r["metrics"].as_array().unwrap()[..5].to_vec());
    write(&p.dir, "fake-report.json", &r.to_string());
    p.config(FIVE, "  done_if: \"vulns\"\n");
    p.commit();

    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(4), "never done — an unmeasured metric is not a met one:\n{out}");
    let rows = p.rows("vulns");
    assert_eq!(rows.len(), 2);
    for (_, _, value, met, err) in rows {
        assert!(!met && value.is_none(), "an error, not a number");
        assert!(err.as_deref().unwrap_or_default().contains("trivy skipped: no applicable files"), "{err:?}");
    }

    // and by hand: `agg judge` gives the same error.
    let (_, out) = p.agg(&["judge", "vulns"]);
    assert!(out.contains("trivy skipped: no applicable files") && out.contains("\"error\""), "{out}");
}

/// Requirement 7: swe-metrics is OPTIONAL. Without it a plain config is fine; a metric judge refuses
/// at startup. An incompatible report schema, or a scoring definition of another version, refuses
/// too — at the key to edit.
#[test]
fn metric_judges_refuse_at_startup_without_a_compatible_swe_metrics() {
    let p = Project::new(&["true"], false);
    p.config("  slow: { timeout: 900 }\n", "  done_if: \"feature\"\n");
    let (code, out) = p.agg(&["check"]);
    assert_eq!(code, Some(0), "no metric judge, no swe-metrics needed:\n{out}");

    p.config(FIVE, "  done_if: \"feature AND cc_p95\"\n");
    let (code, out) = p.agg(&["check", "--json"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("\"path\": \"judges.cc_p95\"") && out.contains("not on PATH"), "{out}");

    let p = Project::new(&["true"], true);
    write(&p.bin, "schema-id-report", "swe-metrics/report/v2");
    p.config(FIVE, "  done_if: \"feature AND cc_p95\"\n");
    let (code, out) = p.agg(&["check"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("swe-metrics/report/v2") && out.contains("swe-metrics/report/v1"), "{out}");

    let p = Project::new(&["true"], true);
    write(&p.bin, "default-score.json", &score(json!(null), "1", json!([]), &[]).to_string());
    p.config("  q: { score: { definition: \"swe-default\", version: \"2\" }, min: 80 }\n", "  done_if: \"q\"\n");
    let (code, out) = p.agg(&["check", "--json"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("judges.q.score") && out.contains("version \\\"1\\\""), "{out}");
}

/// The SCORE judge: computed from the shared report — the pass before session 1 without a baseline
/// (it measures the incumbent), every session WITH the incumbent's report as `--baseline`. A capped
/// score names its failed gates; a `null` score is an error carrying the score's notes.
#[test]
fn a_score_judge_scores_against_the_incumbent_and_fails_closed() {
    let p = Project::new(&["cp '{BIN}/null-score.json' fake-score.json"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    let capped = json!([{"metric": "secrets.findings", "passed": false, "value": 2.0, "max": 0, "cap": 40.0, "status": "ok"}]);
    write(&p.dir, "fake-score.json", &score(json!(40.0), "2", capped, &[]).to_string());
    write(&p.bin, "null-score.json", &score(json!(null), "2", json!([]), &["component coverage: cobertura failed"]).to_string());
    p.config("  quality: { score: { definition: \"swe-default\", version: \"2\" }, min: 80 }\n", "  done_if: \"feature\"\n  invariants: [quality]\n");
    p.commit();

    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(4), "{out}");
    let judged: Vec<String> = p.calls_of("score").into_iter().filter(|c| c.contains("--out")).collect();
    assert_eq!(judged.len(), 2, "the baseline pass and session 1: {:?}", p.calls());
    assert!(!judged[0].contains("--baseline"), "the pass before session 1 IS the incumbent: {}", judged[0]);
    assert!(judged[1].contains("--baseline") && judged[1].contains("agg/private/metrics/incumbent/target.json"), "{}", judged[1]);
    assert!(!judged[0].contains("--definition"), "swe-metrics takes no `--definition` for its default");

    let rows = rows_for(&p.dir, "quality", false);
    assert_eq!((rows[0].value, rows[0].met), (Some(40.0), false));
    assert!(rows[0].rationale.contains("capped by failed gates: secrets.findings = 2"), "{}", rows[0].rationale);
    let err = rows[1].error.clone().unwrap_or_default();
    assert!(err.contains("incomplete (null)") && err.contains("cobertura failed"), "{err}");
    assert!(!rows[1].met);
}

/// `scope: changed` measures only the changes since the base branch — its own measurement, beside
/// the project's.
#[test]
fn a_changed_scope_measures_since_the_base_branch() {
    let p = Project::new(&["echo x >> README"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    p.config(
        "  cc: { metric: { id: \"complexity.cyclomatic\", stat: \"p95\", tool: \"lizard\" }, max: 10 }\n  \
         new_cc: { metric: { id: \"complexity.cyclomatic\", stat: \"p95\", tool: \"lizard\", scope: \"changed\" }, max: 10 }\n",
        "  done_if: \"feature\"\n  invariants: [cc, new_cc]\n",
    );
    p.commit();
    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(4), "{out}");
    let measures = p.calls_of("measure");
    assert_eq!(measures.len(), 4, "two scopes × (baseline + session 1): {measures:?}");
    assert_eq!(measures.iter().filter(|m| m.contains("--changed-since main")).count(), 2, "{measures:?}");
}

/// A `--changed-since` report of the cyclomatic family: lizard either measured `cc` or had nothing to
/// measure (`files` changed, none of them code), or `status`/`reason` say otherwise.
fn changed_report(files: u64, lizard: (&str, &str), cc: Option<f64>) -> Value {
    let rows = match cc {
        Some(v) => json!([row("complexity.cyclomatic", "lizard", "lower_better", json!({"stats": {"count": 1, "sum": v, "mean": v, "p50": v, "p95": v, "max": v}}))]),
        None => json!([]),
    };
    json!({"schema": "swe-metrics/report/v1",
           "subject": {"target": ".", "kind": "repository", "commit": null, "changed_since": "main", "files": files, "languages": {}},
           "analyzers": [analyzer("lizard", "lizard", lizard.0, lizard.1, &["size", "complexity"])],
           "metrics": rows, "details": []})
}

const NEW_CC: &str = "  new_cc: { metric: { id: \"complexity.cyclomatic\", stat: \"max\", tool: \"lizard\", scope: \"changed\" }, max: 3 }\n";

/// "New code must be clean" with no new code in scope is VACUOUSLY met — and a vacuous met moves
/// nothing at the gate: unmet (measured) → vacuously met is NO improvement for
/// `merge_requires_improvement`, so that session is not merged.
#[test]
fn a_vacuous_changed_scope_pass_is_met_but_no_improvement() {
    let p = Project::new(&["cp '{BIN}/docs-only.json' fake-changed.json"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    write(&p.dir, "fake-changed.json", &changed_report(1, ("ran", ""), Some(9.0)).to_string());
    p.stage_report("docs-only.json", &changed_report(1, ("skipped", "no applicable files"), None));
    p.config(NEW_CC, "  done_if: \"feature AND new_cc\"\n  merge_requires_improvement: true\n");
    p.commit();
    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(4), "{out}");
    let rows = rows_for(&p.dir, "new_cc", false);
    assert_eq!((rows[0].value, rows[0].met, rows[0].vacuous), (Some(9.0), false, false), "baseline: measured, unmet");
    let s1 = &rows[1];
    assert_eq!((s1.value, s1.met, s1.vacuous, s1.error.as_deref()), (None, true, true, None), "{}", s1.rationale);
    assert!(s1.rationale.contains("no changed files in scope for lizard"), "{}", s1.rationale);
    assert_eq!(s1.outcome, Outcome::NoImprovement, "a vacuous pass is not progress:\n{out}");
}

/// Fail closed stays for REAL failures in the changed scope: a failed analyzer errors even though no
/// file changed, and so does a skip for any reason but "no applicable files".
#[test]
fn a_failed_or_otherwise_skipped_analyzer_in_the_changed_scope_still_errors() {
    for (status, reason, files) in [("failed", "lizard is not installed", 0), ("skipped", "no SAST rules given (--sast-rules)", 2)] {
        let p = Project::new(&["true"], true);
        write(&p.dir, "fake-report.json", &report(6.0).to_string());
        write(&p.dir, "fake-changed.json", &changed_report(files, (status, reason), None).to_string());
        p.config(NEW_CC, "  done_if: \"feature\"\n  invariants: [new_cc]\n");
        p.commit();
        let (_, out) = p.agg(&["run", "--max-sessions", "1"]);
        for (_, _, value, met, err) in p.rows("new_cc") {
            assert!(!met && value.is_none(), "{status}: {out}");
            assert!(err.as_deref().unwrap_or_default().contains(reason), "{status}: {err:?}");
        }
    }
}

/// `combine: all` — two tools moving in DIFFERENT directions are no improvement for the ratchet,
/// even when the reported (strictest) value got better; `strictest` counts the same move.
#[test]
fn tools_disagreeing_under_combine_all_are_no_improvement() {
    let two = |a: f64, b: f64| {
        let mut r = report(a);
        r["analyzers"].as_array_mut().unwrap().push(analyzer("radon", "radon", "ran", "", &["complexity"]));
        r["metrics"].as_array_mut().unwrap().push(row(
            "complexity.cyclomatic",
            "radon",
            "lower_better",
            json!({"stats": {"count": 1, "sum": b, "mean": b, "p50": b, "p95": b, "max": b}}),
        ));
        r
    };
    for (combine, outcome) in [("all", Outcome::NoImprovement), ("strictest", Outcome::Merged)] {
        let p = Project::new(&["cp '{BIN}/split.json' fake-report.json"], true);
        write(&p.dir, "fake-report.json", &two(10.0, 6.0).to_string());
        p.stage_report("split.json", &two(8.0, 7.0)); // lizard better, radon worse; the worst 10 → 8
        p.config(
            &format!("  cc: {{ metric: {{ id: \"complexity.cyclomatic\", stat: \"p95\", combine: \"{combine}\" }}, max: 20 }}\n"),
            "  done_if: \"feature\"\n  merge_requires_improvement: true\n  ratchet:\n    cc: {}\n",
        );
        p.commit();
        let (_, out) = p.agg(&["run", "--max-sessions", "1"]);
        let s1 = rows_for(&p.dir, "cc", false).into_iter().find(|r| r.session == Some(1)).expect("session 1 judged");
        assert_eq!((s1.value, s1.outcome), (Some(8.0), outcome), "combine: {combine}\n{out}");
        assert_eq!(s1.rationale.contains("different directions"), combine == "all", "{}", s1.rationale);
    }
}

/// A ratchet whose direction contradicts the metric's own is an ERROR — never a ratchet held the
/// wrong way. (`min:` implies up; duplication is lower-is-better.)
#[test]
fn a_ratchet_against_the_metrics_direction_errors() {
    let p = Project::new(&["true"], true);
    write(&p.dir, "fake-report.json", &report(6.0).to_string());
    p.config("  dup: { metric: { id: \"duplication.percent\" }, min: 0 }\n", "  done_if: \"feature\"\n  ratchet:\n    dup: {}\n");
    p.commit();
    let (code, out) = p.agg(&["run", "--max-sessions", "1"]);
    assert_eq!(code, Some(1), "the baseline pass errors and there is no seed — refused before session 1:\n{out}");
    assert!(out.contains("ratcheted `up`") && out.contains("lower_better"), "{out}");
}

// ── the REAL swe-metrics ───────────────────────────────────────────────────────────────────────

/// Is the real swe-metrics on the inherited PATH? Prints why a test is skipped when not.
fn real_swe_metrics() -> bool {
    let ok = Command::new("swe-metrics").args(["schema", "report"]).output().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        eprintln!("SKIPPED: `swe-metrics` is not on PATH — install it (github.com/ssenge/swe-metrics) to run this test");
    }
    ok
}

/// Run `agg run --max-sessions <n>` with the REAL swe-metrics on PATH (the fake worker first).
fn run_real(p: &Project, sessions: &str) -> (Option<i32>, String) {
    let out = Command::cargo_bin("agg")
        .unwrap()
        .current_dir(&p.dir)
        .env("PATH", format!("{}:{}", p.bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", p.dir.join(".home"))
        .args(["run", "--max-sessions", sessions])
        .output()
        .unwrap();
    (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// Against the REAL swe-metrics, a `scope: changed` code-metric judge: the baseline pass (nothing
/// changed) and a session that touches only docs are VACUOUSLY met; a session that changes code is
/// measured as before (a 5-decision function: cyclomatic 6 > 3).
#[test]
fn the_real_changed_scope_is_vacuous_without_changed_code_and_measured_with_it() {
    if !real_swe_metrics() {
        return;
    }
    let p = Project::new(
        &[
            "echo more >> README.md",
            "printf 'def worse(a):\\n    if a > 1:\\n        a += 1\\n    if a > 2:\\n        a += 2\\n    if a > 3:\\n        a += 3\\n    if a > 4:\\n        a += 4\\n    if a > 5:\\n        a += 5\\n    return a\\n' >> calc.py",
        ],
        false,
    );
    write(&p.dir, "calc.py", "def add(a, b):\n    return a + b\n");
    write(&p.dir, "README.md", "# Fixture\n");
    p.config(NEW_CC, "  done_if: \"feature\"\n  invariants: [new_cc]\n");
    p.commit();
    let (code, out) = run_real(&p, "2");
    assert_eq!(code, Some(4), "{out}");
    let rows = rows_for(&p.dir, "new_cc", false);
    assert_eq!(rows.len(), 3, "{out}");
    for (r, what) in [(&rows[0], "the baseline pass (nothing changed)"), (&rows[1], "a docs-only session")] {
        assert!(r.met && r.vacuous && r.value.is_none() && r.error.is_none(), "{what}: {} {:?}", r.rationale, r.error);
        assert!(r.rationale.contains("no changed files in scope for lizard"), "{what}: {}", r.rationale);
    }
    let code_row = &rows[2];
    assert_eq!((code_row.value, code_row.met, code_row.vacuous), (Some(6.0), false, false), "{} {:?}", code_row.rationale, code_row.error);
}


const CALC: &str = "\"\"\"A small calculator with one branchy function.\"\"\"\n\n\ndef classify(n):\n    \"\"\"Classify a number.\"\"\"\n    if n < 0:\n        return \"negative\"\n    if n == 0:\n        return \"zero\"\n    if n < 10:\n        return \"small\"\n    if n < 100:\n        return \"medium\"\n    return \"large\"\n\n\ndef add(a, b):\n    \"\"\"Add two numbers.\"\"\"\n    return a + b\n";

const REPORT_A: &str = "\"\"\"Report A.\"\"\"\n\n\ndef render(rows):\n    \"\"\"Render rows as text.\"\"\"\n    lines = []\n    for row in rows:\n        name = row.get(\"name\", \"\")\n        value = row.get(\"value\", 0)\n        unit = row.get(\"unit\", \"\")\n        label = f\"{name}: {value} {unit}\".strip()\n        lines.append(label.upper())\n        lines.append(\"-\" * len(label))\n    total = sum(r.get(\"value\", 0) for r in rows)\n    lines.append(f\"total: {total}\")\n    return \"\\n\".join(lines)\n";

/// ACCEPTANCE, against the REAL swe-metrics: a fixture with KNOWN complexity (`classify` has four
/// decisions — cyclomatic 5, the project's maximum) and KNOWN duplication (`report_b.py` is
/// `report_a.py` with one word changed — one clone) produces those values and the verdicts their
/// thresholds imply; the swe-default score of it is a number in 0..=100.
#[test]
fn the_real_swe_metrics_measures_a_fixture_with_known_values() {
    if !real_swe_metrics() {
        return;
    }
    let p = Project::new(&["true"], false);
    write(&p.dir, "calc.py", CALC);
    write(&p.dir, "report_a.py", REPORT_A);
    write(&p.dir, "report_b.py", &REPORT_A.replace("Report A", "Report B"));
    p.config(
        "  cc_max: { metric: { id: \"complexity.cyclomatic\", stat: \"max\", tool: \"lizard\" }, max: 4 }\n  \
         cc_p95: { metric: { id: \"complexity.cyclomatic\", stat: \"p95\", tool: \"lizard\" }, max: 10 }\n  \
         clones: { metric: { id: \"duplication.clones\" }, max: 0 }\n  \
         dup_pct: { metric: { id: \"duplication.percent\" }, max: 50 }\n  \
         quality: { score: { definition: \"swe-default\", version: \"2\" }, min: 0 }\n",
        "  done_if: \"feature\"\n  invariants: [cc_max, cc_p95, clones, dup_pct, quality]\n",
    );
    p.commit();
    let (code, text) = run_real(&p, "1");
    assert_eq!(code, Some(4), "{text}");
    let baseline = |j: &str| rows_for(&p.dir, j, false).into_iter().next().unwrap_or_else(|| panic!("no row for {j}:\n{text}"));
    let cc_max = baseline("cc_max");
    assert_eq!((cc_max.value, cc_max.met), (Some(5.0), false), "classify: 4 decisions + 1 = 5 > 4: {}", cc_max.rationale);
    let cc_p95 = baseline("cc_p95");
    assert!(cc_p95.met && cc_p95.value.is_some_and(|v| v <= 5.0), "{}", cc_p95.rationale);
    let clones = baseline("clones");
    assert_eq!((clones.value, clones.met), (Some(1.0), false), "report_a/report_b are one clone: {}", clones.rationale);
    let dup = baseline("dup_pct");
    assert!(dup.met && dup.value.is_some_and(|v| v > 0.0), "{} {:?}", dup.rationale, dup.error);
    let q = baseline("quality");
    assert!(q.error.is_none() && q.value.is_some_and(|v| (0.0..=100.0).contains(&v)), "{} {:?}", q.rationale, q.error);
}
