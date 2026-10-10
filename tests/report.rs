//! The RUN REPORT and the headless contract, end to end: the real `agg` binary, a fake `claude` on
//! PATH, a real git repo — as in `tests/cli.rs`.
//!
//! Every report a test produces is validated against the PUBLISHED schema
//! (`docs/schemas/report.v2.json`), with a small validator below that covers exactly the keywords
//! the schema uses — and refuses a schema that uses any other, so the check cannot quietly go blind.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

// ── fixtures ─────────────────────────────────────────────────────────────────────────────────────

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The fake worker's body when it does a session's work: mark the session as started (so a test can
/// signal it mid-session), then commit `did_work`.
const WORKS: &str = ": > started\n: > did_work\ngit add did_work >/dev/null 2>&1\ngit commit -qm work >/dev/null 2>&1\n";
/// …and when it hangs, so a test can signal the loop while a worker is running.
const HANGS: &str = ": > started\nsleep 30\n";

/// A project: a git repo on `main`, a fake `claude` running `worker_body`, a `worked` judge (met
/// once `did_work` exists) and an `agg.yaml` with the given `sequence:` lines.
fn project(worker_body: &str, sequence: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let claude = dir.join("bin/claude");
    write(
        &claude,
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = --version ] && {{ echo fake-claude 0.0.0; exit 0; }}; done\n\
             {worker_body}\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\",\"usage\":{{\"output_tokens\":1}},\"total_cost_usd\":0.01}}'\n"
        ),
    );
    chmod_x(&claude);
    let judge = dir.join("agg/judges/worked.sh");
    write(
        &judge,
        "#!/bin/sh\nif [ -f did_work ]; then echo '{\"met\":true,\"value\":1,\"max\":1,\"rationale\":\"worked\"}'; \
         else echo '{\"met\":false,\"value\":0,\"max\":1,\"rationale\":\"not yet\"}'; fi\n",
    );
    chmod_x(&judge);
    write(
        &dir.join("agg/agg.yaml"),
        &format!(
            "project: \"rep\"\ndefaults: {{ model: \"fake\" }}\nsteps:\n  worker: {{}}\nsequence:\n  steps: [\"worker\"]\n{sequence}summary: {{ enabled: false }}\n"
        ),
    );
    write(&agg::paths::agg_dir(dir).join("STATE.md"), "work\n");
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    (tmp, path)
}

/// `agg` with the fake on PATH and HOME inside the temp dir. No TTY: stdin is null.
fn agg(dir: &Path, path: &str) -> Command {
    let mut c = Command::cargo_bin("agg").unwrap();
    c.current_dir(dir).env("PATH", path).env("HOME", dir.join(".home")).env_remove("AGG_TEST_FAULT").stdin(Stdio::null());
    c
}

fn report(dir: &Path) -> Value {
    let p = agg::paths::report_json(dir);
    let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("no report at {}: {e}", p.display()));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_valid(&v);
    v
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn wait_for(p: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !p.exists() {
        assert!(Instant::now() < deadline, "{} never appeared", p.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ── the schema check ─────────────────────────────────────────────────────────────────────────────

fn schema() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/schemas/report.v2.json");
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn assert_valid(v: &Value) {
    let s = schema();
    let mut errs = Vec::new();
    validate(&s, &s, v, "$", &mut errs);
    assert!(errs.is_empty(), "report violates docs/schemas/report.v2.json:\n{}\n{v:#}", errs.join("\n"));
}

/// The keywords [`validate`] implements. Annotations are listed so they are not mistaken for gaps.
const SUPPORTED: &[&str] = &[
    "$schema", "$id", "title", "description", "type", "required", "properties", "additionalProperties",
    "const", "enum", "minimum", "items", "oneOf", "$ref", "$defs",
];

fn validate(root: &Value, s: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let name = r.strip_prefix("#/$defs/").expect("only local $defs refs");
        return validate(root, &root["$defs"][name], v, at, errs);
    }
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::String(x) => vec![x.as_str()],
            Value::Array(xs) => xs.iter().filter_map(Value::as_str).collect(),
            _ => panic!("bad type keyword"),
        };
        let ok = types.iter().any(|t| match *t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            "number" => v.is_number(),
            "integer" => v.is_u64() || v.is_i64(),
            other => panic!("unknown type {other}"),
        });
        if !ok {
            return errs.push(format!("{at}: expected {types:?}, got {v}"));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            errs.push(format!("{at}: expected {c}, got {v}"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            errs.push(format!("{at}: {v} not in {e:?}"));
        }
    }
    if let (Some(m), Some(n)) = (s.get("minimum").and_then(Value::as_f64), v.as_f64()) {
        if n < m {
            errs.push(format!("{at}: {n} < minimum {m}"));
        }
    }
    if let Some(alts) = s.get("oneOf").and_then(Value::as_array) {
        let passing = alts
            .iter()
            .filter(|a| {
                let mut e = Vec::new();
                validate(root, a, v, at, &mut e);
                e.is_empty()
            })
            .count();
        if passing != 1 {
            errs.push(format!("{at}: matches {passing} of oneOf, needs exactly 1"));
        }
    }
    if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
        for (i, x) in arr.iter().enumerate() {
            validate(root, items, x, &format!("{at}[{i}]"), errs);
        }
    }
    if let Some(obj) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        for r in s.get("required").and_then(Value::as_array).into_iter().flatten() {
            let r = r.as_str().unwrap();
            if !obj.contains_key(r) {
                errs.push(format!("{at}: missing required `{r}`"));
            }
        }
        for (k, x) in obj {
            match (props.and_then(|p| p.get(k)), s.get("additionalProperties")) {
                (Some(ps), _) => validate(root, ps, x, &format!("{at}.{k}"), errs),
                (None, Some(Value::Bool(false))) => errs.push(format!("{at}: unexpected field `{k}`")),
                (None, Some(extra @ Value::Object(_))) => validate(root, extra, x, &format!("{at}.{k}"), errs),
                (None, _) => {}
            }
        }
    }
}

/// Every keyword in the schema must be one [`validate`] understands — otherwise a constraint added
/// to the schema would be silently skipped and the reports "validate" against less than is published.
fn assert_supported(s: &Value, at: &str) {
    let Some(obj) = s.as_object() else { return };
    for (k, x) in obj {
        assert!(SUPPORTED.contains(&k.as_str()), "{at}: the test validator does not implement `{k}`");
        match k.as_str() {
            // maps of NAME → schema: the keys are names, not keywords.
            "properties" | "$defs" => x.as_object().unwrap().iter().for_each(|(n, sub)| assert_supported(sub, &format!("{at}.{n}"))),
            "oneOf" => x.as_array().unwrap().iter().for_each(|sub| assert_supported(sub, at)),
            "items" | "additionalProperties" => assert_supported(x, at),
            _ => {}
        }
    }
}

/// The schema file and the code agree on the version, the validator covers the whole schema, and it
/// actually REJECTS drift — an extra field, a missing one, an outcome off the list.
#[test]
fn the_published_schema_is_versioned_complete_and_strict() {
    let s = schema();
    assert_supported(&s, "$");
    assert_eq!(s["properties"]["schema_version"]["const"], agg::report::SCHEMA_VERSION);

    let tmp = tempfile::tempdir().unwrap();
    let (rep, _) = agg::report::write_fallback(tmp.path(), "p", "x", false);
    let good = serde_json::to_value(&rep).unwrap();
    assert_valid(&good);

    for (label, bad) in [
        ("extra field", { let mut b = good.clone(); b["surprise"] = 1.into(); b }),
        ("missing field", { let mut b = good.clone(); b.as_object_mut().unwrap().remove("run_id"); b }),
        ("unknown outcome", { let mut b = good.clone(); b["outcome"] = "maybe".into(); b }),
    ] {
        let mut errs = Vec::new();
        validate(&s, &s, &bad, "$", &mut errs);
        assert!(!errs.is_empty(), "the validator accepted a report with an {label}");
    }
}

// ── the outcomes ─────────────────────────────────────────────────────────────────────────────────

/// DONE, through `--report-json`: stdout is the report and nothing else, the logs went to run.log,
/// and the report's facts match git and the run.
#[test]
fn done_prints_only_the_report_on_stdout_and_logs_to_run_log() {
    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    let out = agg(dir, &path).args(["run", "--report-json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    // the WHOLE of stdout parses as one JSON document: no banner, no worker stream around it.
    let printed: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not just the report ({e}):\n{}", String::from_utf8_lossy(&out.stdout)));
    let r = report(dir);
    assert_eq!(printed, r, "stdout and report.json must be the same report");
    assert_eq!(r["outcome"], "done");
    assert_eq!(r["exit_code"], 0);
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["sessions"]["total"], 1);
    assert_eq!(r["sessions"]["merged"], 1);
    assert_eq!(r["judges"]["worked"]["last"]["met"], true);
    assert_eq!(r["judges"]["worked"]["baseline"]["met"], false, "the baseline ran before any work");
    assert_eq!(r["result"]["base_branch"], "main");
    assert_eq!(r["result"]["final_commit"], git(dir, &["rev-parse", "main"]));

    let log = fs::read_to_string(agg::paths::run_log(dir)).unwrap();
    assert!(log.contains("AgenticGoGo — project rep"), "the banner belongs in run.log:\n{log}");
    assert!(log.contains(r["run_id"].as_str().unwrap()), "run.log must carry the run id:\n{log}");
}

/// ABORTED by `abort_if` after a session, and the precedence rule: when `done_if` and `abort_if`
/// hold at the same session end, done wins.
#[test]
fn abort_if_reports_aborted_and_loses_to_done_in_the_same_session() {
    let (tmp, path) = project(WORKS, "  done_if: \"never_met\"\n  abort_if: \"worked\"\n");
    let dir = tmp.path();
    write(&dir.join("agg/judges/never_met.sh"), "#!/bin/sh\necho '{\"met\":false}'\n");
    chmod_x(&dir.join("agg/judges/never_met.sh"));
    let out = agg(dir, &path).arg("run").output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    let r = report(dir);
    assert_eq!((r["outcome"].as_str(), r["exit_code"].as_u64()), (Some("aborted"), Some(3)));
    assert!(r["stop_reason"].as_str().unwrap().contains("worked"), "name the term that fired: {r}");

    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n  abort_if: \"worked\"\n");
    let out = agg(tmp.path(), &path).arg("run").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "done must beat aborted:\n{}", text(&out));
    assert_eq!(report(tmp.path())["outcome"], "done");
}

/// The session cap (exit 4) is an `aborted` run whose reason names the cap.
#[test]
fn the_session_cap_reports_aborted_with_exit_4() {
    let (tmp, path) = project(": > started\n", "  done_if: \"worked\"\n");
    let out = agg(tmp.path(), &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    let r = report(tmp.path());
    assert_eq!((r["outcome"].as_str(), r["exit_code"].as_u64()), (Some("aborted"), Some(4)));
    assert!(r["stop_reason"].as_str().unwrap().contains("max_sessions=2"), "{r}");
    assert_eq!(r["sessions"]["total"], 2);
}

/// STOPPED by SIGTERM from a process with no TTY in its own process group (what a supervisor does):
/// the session ends, the report says `stopped`, and the exit code is 5.
#[test]
fn sigterm_without_a_tty_reports_stopped_and_exits_5() {
    use std::os::unix::process::CommandExt;
    let (tmp, path) = project(HANGS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    let child = agg(dir, &path)
        .args(["run", "--report-json"])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&dir.join("started"));
    Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    let t = Instant::now();
    let out = child.wait_with_output().unwrap();
    assert!(t.elapsed() < Duration::from_secs(20), "the hung worker was not killed");
    assert_eq!(out.status.code(), Some(5), "{}", text(&out));
    let r = report(dir);
    assert_eq!(r["outcome"], "stopped");
    assert_eq!(r["exit_code"], 5);
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), r);
}

/// `agg stop --project <dir>` from ANOTHER directory stops the run, and a stop beats `done` in the
/// session it lands in — the session below meets the DoD after the stop was queued.
#[test]
fn agg_stop_from_another_process_beats_done_in_the_same_session() {
    let worker = ": > started\nwhile [ ! -f go ]; do sleep 0.1; done\n: > did_work\ngit add did_work >/dev/null 2>&1\ngit commit -qm w >/dev/null 2>&1\n";
    let (tmp, path) = project(worker, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    let child = agg(dir, &path).arg("run").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    wait_for(&dir.join("started"));
    let elsewhere = tempfile::tempdir().unwrap();
    let stop = agg(elsewhere.path(), &path).args(["stop", "--project"]).arg(dir).output().unwrap();
    assert!(stop.status.success(), "agg stop --project failed: {}", text(&stop));
    fs::write(dir.join("go"), "").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(5), "{}", text(&out));
    let r = report(dir);
    assert_eq!(r["outcome"], "stopped");
    assert_eq!(r["judges"]["worked"]["last"]["met"], true, "the DoD held, and the stop still won");
}

/// ERROR from inside agg during VERIFY — both a returned error and a panic — is caught, reported
/// with its message, and exits 1.
#[test]
fn an_internal_fault_during_verify_reports_error() {
    for (fault, needle) in [("verify-error", "injected fault at verify"), ("verify-panic", "injected panic at verify")] {
        let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
        let out = agg(tmp.path(), &path).env("AGG_TEST_FAULT", fault).args(["run", "--report-json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{fault}: {}", text(&out));
        let r = report(tmp.path());
        assert_eq!((r["outcome"].as_str(), r["exit_code"].as_u64()), (Some("error"), Some(1)), "{fault}");
        assert!(r["error"].as_str().unwrap().contains(needle), "{fault}: {r}");
        assert_eq!(r["sessions"]["total"], 1, "{fault}: the rich report, built from the run's state");
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), r, "{fault}: stdout is the report");
        assert!(String::from_utf8_lossy(&out.stderr).contains(needle), "{fault}: the reason reaches stderr");
    }
}

/// A run refused before its loop starts (an unknown `--base-branch`) still leaves an `error` report
/// — and it REPLACES the previous run's, so no consumer reads a stale `done`.
#[test]
fn a_refused_start_replaces_the_previous_report_with_an_error() {
    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    assert_eq!(agg(dir, &path).arg("run").status().unwrap().code(), Some(0));
    let first = report(dir);

    let out = agg(dir, &path).args(["run", "--report-json", "--base-branch", "no-such-branch"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let r = report(dir);
    assert_eq!(r["outcome"], "error");
    assert_ne!(r["run_id"], first["run_id"], "the previous run's report must not survive");
    assert!(r["error"].as_str().unwrap().contains("no-such-branch"), "{r}");
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), r);
}

/// SIGKILL leaves NO report — and in particular not the previous run's.
#[test]
fn a_sigkilled_run_leaves_no_report_at_all() {
    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    assert_eq!(agg(dir, &path).arg("run").status().unwrap().code(), Some(0));
    assert!(agg::paths::report_json(dir).exists());

    // a second run whose worker hangs, against a DoD the merged work does not meet.
    let claude = fs::read_to_string(dir.join("bin/claude")).unwrap().replace(WORKS, HANGS);
    write(&dir.join("bin/claude"), &claude);
    let cfg = fs::read_to_string(dir.join("agg/agg.yaml")).unwrap().replace("done_if: \"worked\"", "done_if: \"never_met\"");
    write(&dir.join("agg/agg.yaml"), &cfg);
    write(&dir.join("agg/judges/never_met.sh"), "#!/bin/sh\necho '{\"met\":false}'\n");
    chmod_x(&dir.join("agg/judges/never_met.sh"));
    let _ = fs::remove_file(dir.join("started"));
    // the edits above touch files the first run's merge committed: commit them, the tree must be clean.
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "second run"]);
    let mut child = agg(dir, &path).arg("run").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    wait_for(&dir.join("started"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!agg::paths::report_json(dir).exists(), "a stale or partial report survived a SIGKILL");
}

/// `--base-branch` overrides the config: the run bases on and merges into that branch, and the
/// report says so.
#[test]
fn base_branch_flag_overrides_the_config() {
    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    git(dir, &["branch", "release"]);
    let main_before = git(dir, &["rev-parse", "main"]);
    let out = agg(dir, &path).args(["run", "--base-branch", "release"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let r = report(dir);
    assert_eq!(r["result"]["base_branch"], "release");
    assert_eq!(r["result"]["final_commit"], git(dir, &["rev-parse", "release"]));
    assert_eq!(git(dir, &["rev-parse", "main"]), main_before, "main must not move");
}

/// No `sequence.limits` at all: nothing implicit ends the run early — it runs until `done_if`.
#[test]
fn a_run_without_limits_runs_until_done() {
    // the third session's work is the first that meets the judge.
    let worker = ": > started\necho x >> tally\nif [ \"$(wc -l < tally)\" -ge 3 ]; then : > did_work; git add did_work >/dev/null 2>&1; git commit -qm w >/dev/null 2>&1; fi\n";
    let (tmp, path) = project(worker, "  done_if: \"worked\"\n");
    let out = agg(tmp.path(), &path).arg("run").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let r = report(tmp.path());
    assert_eq!((r["outcome"].as_str(), r["sessions"]["total"].as_u64()), (Some("done"), Some(3)));
}

/// Two runs in two project directories at the same time finish independently, each with its own
/// report and run id.
#[test]
fn two_parallel_runs_in_different_directories_do_not_interfere() {
    let slow = ": > started\nsleep 1\n: > did_work\ngit add did_work >/dev/null 2>&1\ngit commit -qm w >/dev/null 2>&1\n";
    let (a, pa) = project(slow, "  done_if: \"worked\"\n");
    let (b, pb) = project(slow, "  done_if: \"worked\"\n");
    let ca = agg(a.path(), &pa).args(["run", "--report-json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let cb = agg(b.path(), &pb).args(["run", "--report-json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (oa, ob) = (ca.wait_with_output().unwrap(), cb.wait_with_output().unwrap());
    assert_eq!(oa.status.code(), Some(0), "{}", text(&oa));
    assert_eq!(ob.status.code(), Some(0), "{}", text(&ob));
    let (ra, rb) = (report(a.path()), report(b.path()));
    assert_eq!((ra["outcome"].as_str(), rb["outcome"].as_str()), (Some("done"), Some("done")));
    assert_ne!(ra["run_id"], rb["run_id"]);
    assert_eq!(serde_json::from_slice::<Value>(&oa.stdout).unwrap(), ra);
    assert_eq!(serde_json::from_slice::<Value>(&ob.stdout).unwrap(), rb);
}

/// `--report-json` outside any project still answers on stdout with an `error` report, and writes
/// no `agg/` directory into a directory that is not a project.
#[test]
fn report_json_outside_a_project_prints_an_error_report() {
    let tmp = tempfile::tempdir().unwrap();
    let out = agg(tmp.path(), "/usr/bin:/bin").args(["run", "--report-json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_valid(&v);
    assert_eq!(v["outcome"], "error");
    assert!(!tmp.path().join("agg").exists(), "agg must not scaffold state into a non-project");
}

/// A judge that writes `cc.sarif` in its scratch and cites it as evidence has the file KEPT under
/// `agg/private/evidence/<run>/<session>/<judge>/`, named on its ledger row and in the report (for
/// the baseline verdict and the last one) — while a path outside the scratch and the project, and a
/// scratch symlink pointing out of them, are refused and recorded rather than copied.
#[test]
fn a_judge_evidence_file_is_kept_and_referenced() {
    let (tmp, path) = project(WORKS, "  done_if: \"worked\"\n");
    let dir = tmp.path();
    let judge = dir.join("agg/judges/worked.sh");
    write(
        &judge,
        "#!/bin/sh\necho '<sarif/>' > \"$AGG_JUDGE_SCRATCH/cc.sarif\"\nln -sf /etc/hosts \"$AGG_JUDGE_SCRATCH/link\"\n\
         if [ -f did_work ]; then m=true; else m=false; fi\n\
         echo \"{\\\"met\\\":$m,\\\"value\\\":1,\\\"max\\\":1,\\\"rationale\\\":\\\"r\\\",\\\"evidence\\\":[\\\"cc.sarif\\\",\\\"/etc/hosts\\\",\\\"link\\\"]}\"\n",
    );
    chmod_x(&judge);
    let out = agg(dir, &path).arg("run").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    let r = report(dir);
    let run = r["run_id"].as_str().unwrap();
    let ev_dir = r["paths"]["evidence_dir"].as_str().expect("evidence was kept, so the dir is named");
    assert!(ev_dir.ends_with(&format!("agg/private/evidence/{run}")), "{ev_dir}");
    let last = r["judges"]["worked"]["last"]["evidence"].as_array().expect("the last verdict lists its evidence");
    assert_eq!(last.len(), 1, "only the scratch file is kept: {last:?}");
    let kept = last[0].as_str().unwrap();
    assert!(kept.ends_with(&format!("{run}/1/worked/cc.sarif")), "{kept}");
    assert_eq!(fs::read_to_string(kept).unwrap(), "<sarif/>\n");
    let base = r["judges"]["worked"]["baseline"]["evidence"][0].as_str().unwrap();
    assert!(base.ends_with(&format!("{run}/baseline/worked/cc.sarif")), "{base}");

    let ledger = fs::read_to_string(agg::paths::verdicts_jsonl(dir)).unwrap();
    let row: Value = serde_json::from_str(ledger.lines().last().unwrap()).unwrap();
    assert_eq!(row["evidence_saved"][0], format!("agg/private/evidence/{run}/1/worked/cc.sarif"));
    let skipped: Vec<&str> = row["evidence_skipped"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert!(skipped.iter().all(|s| s.contains("refused")), "{skipped:?}");
    let copied = fs::read_dir(Path::new(ev_dir).join("1/worked")).unwrap().count();
    assert_eq!(copied, 1, "neither /etc/hosts nor the symlink's target was copied");
}

// ── usage per session (A-19) ─────────────────────────────────────────────────────────────────────

/// A fake `claude` that reports KNOWN usage and a model name other than the configured one: as a
/// worker (stream-json) it reports model `reported-worker` and fixed tokens; as the ruler (a one-shot,
/// `--output-format json`) it grades `{"met":true}` and reports model `reported-ruler`. The second
/// worker session does the work.
const USAGE_FAKE: &str = r#"#!/bin/sh
oneshot=0
for a in "$@"; do
  [ "$a" = --version ] && { echo fake-claude 0.0.0; exit 0; }
  [ "$a" = json ] && oneshot=1
done
if [ "$oneshot" = 1 ]; then
  printf '%s\n' '{"type":"result","subtype":"success","result":"{\"met\":true,\"value\":1,\"max\":1,\"rationale\":\"fine\"}","usage":{"input_tokens":3,"output_tokens":7,"cache_creation_input_tokens":11,"cache_read_input_tokens":13},"total_cost_usd":0.125,"modelUsage":{"reported-ruler":{"outputTokens":7}}}'
  exit 0
fi
printf '%s\n' '{"type":"system","subtype":"init","model":"reported-worker","session_id":"s"}'
echo x >> tally
if [ "$(wc -l < tally)" -ge 2 ]; then : > did_work; git add did_work >/dev/null 2>&1; git commit -qm w >/dev/null 2>&1; fi
printf '%s\n' '{"type":"result","subtype":"success","result":"ok","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":1000,"cache_read_input_tokens":2000},"total_cost_usd":0.5}'
"#;

/// Per session and in total, the report states exactly what the agent reported — split four ways,
/// worker and ruler apart, the price `reported` — and names the model the agent REPORTED, not the
/// configured one. `agg history --json` carries the same totals.
#[test]
fn usage_per_session_matches_what_the_agents_reported() {
    let (tmp, path) = project("", "  done_if: \"worked AND rubric\"\n");
    let dir = tmp.path();
    write(&dir.join("bin/claude"), USAGE_FAKE);
    chmod_x(&dir.join("bin/claude"));
    write(&dir.join("agg/judges/rubric.md"), "Grade the work.\n");
    let cfg = fs::read_to_string(dir.join("agg/agg.yaml")).unwrap();
    write(&dir.join("agg/agg.yaml"), &format!("{cfg}judge: {{ agent: \"claude\", model: \"configured-ruler\" }}\n"));
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "judges"]);

    let out = agg(dir, &path).arg("run").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let r = report(dir);
    assert_eq!(r["schema_version"], 2);

    let tokens = |i, o, w, rd| serde_json::json!({"input": i, "output": o, "cache_write": w, "cache_read": rd});
    let worker = serde_json::json!({
        "role": "worker", "agent": "claude", "model": "reported-worker",
        "tokens": tokens(100, 50, 1000, 2000), "cost_usd": 0.5, "cost_source": "reported"
    });
    let ruler = serde_json::json!({
        "role": "ruler", "agent": "claude", "model": "reported-ruler",
        "tokens": tokens(3, 7, 11, 13), "cost_usd": 0.125, "cost_source": "reported"
    });
    let log = r["session_log"].as_array().unwrap();
    assert_eq!(log.len(), 3, "the baseline pass (session 0) and two sessions: {r:#}");
    assert_eq!(log[0], serde_json::json!({"session": 0, "step": "", "usage": [ruler]}), "the baseline used the ruler only");
    for (i, s) in log[1..].iter().enumerate() {
        assert_eq!(s["session"], i + 1);
        assert_eq!(s["step"], "worker");
        assert_eq!(s["usage"], serde_json::json!([worker, ruler]), "session {}: worker and ruler apart", i + 1);
    }

    let totals = |t, c: f64| serde_json::json!({"tokens": t, "cost_usd": c, "cost_source": "reported"});
    assert_eq!(r["usage"]["worker"], totals(tokens(200, 100, 2000, 4000), 1.0));
    assert_eq!(r["usage"]["ruler"], totals(tokens(9, 21, 33, 39), 0.375));
    assert_eq!(r["usage"]["total"], totals(tokens(209, 121, 2033, 4039), 1.375));
    let text = r.to_string();
    assert!(!text.contains("configured-ruler") && !text.contains("\"fake\""), "only reported models are named: {r:#}");

    let hist = agg(dir, &path).args(["history", "--json"]).output().unwrap();
    let h: Value = serde_json::from_slice(&hist.stdout).unwrap();
    assert_eq!(h["runs"][0]["usage"], r["usage"], "history carries the run's totals");
}
