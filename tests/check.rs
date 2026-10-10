//! `agg check` and `agg doctor --json`, driven through the real binary.
//!
//! PATH holds ONLY a private `bin/` with a fake `claude`, so the host is fully controlled: no
//! `sandbox-exec`/`bwrap` (the sandbox tier is unavailable), no `docker`/`podman` (no container
//! engine). The fake agent and the judge each leave a marker file when they RUN, which is how
//! "check runs nothing" is proved rather than assumed. HOME is redirected into the temp dir so the
//! judge library install never touches the developer's real home.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A project dir with a fake `claude` (answers `--version`; marks `agent_ran` on any real call) and
/// a `goal` judge (marks `judge_ran`). Returns (tempdir, PATH = the fake bin ONLY).
fn project(cfg: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "bin/claude",
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && { echo 'fake-claude 9.9.9'; exit 0; }\n: > agent_ran\nexit 0\n",
    );
    write(dir, "agg/judges/goal.sh", "#!/bin/sh\n: > judge_ran\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1}'\n");
    write(dir, "agg/state/STATE.md", "next\n");
    write(dir, "agg/agg.yaml", cfg);
    let path = dir.join("bin").display().to_string();
    (tmp, path)
}

fn agg(dir: &Path, path: &str, args: &[&str]) -> Output {
    Command::cargo_bin("agg")
        .unwrap()
        .current_dir(dir)
        .env("PATH", path)
        .env("HOME", dir.join(".home"))
        .args(args)
        .output()
        .unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout must be JSON ({e}):\n{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    })
}

fn problem_paths(v: &Value) -> Vec<String> {
    v["problems"].as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap().to_string()).collect()
}

const GOOD: &str = "project: \"p\"\nsummary: { enabled: false }\nsteps: { worker: {} }\n\
                    sequence: { steps: [\"worker\"], done_if: \"goal\" }\n";

/// One fault per config, each from a different startup stage.
const BAD_KEY: &str = "project: \"p\"\ndefaults: { agnet: \"claude\" }\nsummary: { enabled: false }\n\
                       steps: { worker: {} }\nsequence: { steps: [\"worker\"], done_if: \"goal\" }\n";
const BAD_JUDGE: &str = "project: \"p\"\nsummary: { enabled: false }\nsteps: { worker: {} }\n\
                         sequence: { steps: [\"worker\"], done_if: \"ghost\" }\n";
const BAD_TIER: &str = "project: \"p\"\nsummary: { enabled: false }\nsteps: { worker: { isolation: \"sandbox\" } }\n\
                        sequence: { steps: [\"worker\"], done_if: \"goal\" }\n";
const ALL_THREE: &str = "project: \"p\"\ndefaults: { agnet: \"claude\" }\nsummary: { enabled: false }\n\
                         steps: { worker: { isolation: \"sandbox\" } }\nsequence: { steps: [\"worker\"], done_if: \"ghost\" }\n";

/// THE acceptance criterion: an invalid key, an unknown judge and an unavailable sandbox are
/// returned together, each at its path — and nothing ran to find them.
#[test]
fn check_json_returns_every_problem_at_once_and_runs_nothing() {
    let (tmp, path) = project(ALL_THREE);
    let dir = tmp.path();
    let out = agg(dir, &path, &["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "problems must exit non-zero");
    let v = json(&out);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["ok"], false);
    assert_eq!(problem_paths(&v), ["defaults.agnet", "sequence.done_if", "steps.worker.isolation"]);
    assert!(!dir.join("judge_ran").exists(), "check must never run a judge");
    assert!(!dir.join("agent_ran").exists(), "check must never run an agent");
}

/// "the same messages startup would give": each single fault's `check` message appears verbatim in
/// what `agg run` refuses with.
#[test]
fn each_check_message_is_what_startup_says() {
    for (cfg, at) in [(BAD_KEY, "defaults.agnet"), (BAD_JUDGE, "sequence.done_if"), (BAD_TIER, "steps.worker.isolation")] {
        let (tmp, path) = project(cfg);
        let v = json(&agg(tmp.path(), &path, &["check", "--json"]));
        assert_eq!(problem_paths(&v), [at], "exactly one problem for this fixture: {v}");
        let msg = v["problems"][0]["message"].as_str().unwrap();

        let run = agg(tmp.path(), &path, &["run", "--max-sessions", "1"]);
        let err = String::from_utf8_lossy(&run.stderr);
        assert!(!run.status.success(), "`agg run` must refuse this config too");
        assert!(err.contains(msg), "startup must say what check says.\n check: {msg}\n run:   {err}");
    }
}

#[test]
fn check_passes_a_good_config_and_runs_nothing() {
    let (tmp, path) = project(GOOD);
    let dir = tmp.path();
    let out = agg(dir, &path, &["check", "--json"]);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["ok"], true);
    assert!(v["problems"].as_array().unwrap().is_empty());

    let out = agg(dir, &path, &["check"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 judge(s) resolve"));
    assert!(!dir.join("judge_ran").exists() && !dir.join("agent_ran").exists(), "check must run nothing");
}

/// Without `--json` the same problems are listed for a human, located, and the exit code agrees.
#[test]
fn check_human_mode_lists_every_problem_and_exits_1() {
    let (tmp, path) = project(ALL_THREE);
    let out = agg(tmp.path(), &path, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    for want in ["✗ defaults.agnet: unknown field `agnet`", "✗ sequence.done_if:", "✗ steps.worker.isolation:", "3 problem(s)"] {
        assert!(err.contains(want), "missing `{want}`:\n{err}");
    }
}

/// A missing agent CLI is a startup refusal too (`agg run` preflights every agent it names).
#[test]
fn check_reports_a_missing_agent_cli_at_the_key_that_chose_it() {
    let (tmp, path) = project(&GOOD.replace("steps: { worker: {} }", "steps: { worker: { agent: \"codex\" } }"));
    let v = json(&agg(tmp.path(), &path, &["check", "--json"]));
    assert_eq!(problem_paths(&v), ["steps.worker.agent"], "{v}");
}

#[test]
fn check_without_a_config_is_one_root_problem() {
    let tmp = tempfile::tempdir().unwrap();
    let out = agg(tmp.path(), "/nonexistent", &["check", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(problem_paths(&json(&out)), [""]);
}

// ── agg doctor --json ──────────────────────────────────────────────────────────────────────────

/// A complete setup: ok, the fake agent found at its path with its version and capability flags,
/// the other backends reported as absent, and this PATH-starved host offering only `none`.
#[test]
fn doctor_json_reports_backends_tiers_and_ok() {
    let (tmp, path) = project(GOOD);
    let dir = tmp.path();
    let out = agg(dir, &path, &["doctor", "--json"]);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(0), "{v:#}");
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["agg_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["ok"], true);
    assert_eq!(v["config"]["valid"], true);
    assert_eq!(v["config"]["judges"], 1);

    let claude = &v["backends"][0];
    assert_eq!(claude["name"], "claude");
    assert_eq!((claude["used"].clone(), claude["installed"].clone()), (Value::Bool(true), Value::Bool(true)));
    assert_eq!(claude["version"], "fake-claude 9.9.9");
    assert_eq!(claude["path"], dir.join("bin/claude").display().to_string());
    for flag in ["reports_output_tokens", "reports_cost_usd", "supports_resume", "supports_effort", "detects_rate_limits", "supports_one_shot"] {
        assert!(claude["capabilities"][flag].is_boolean(), "capability `{flag}` must be reported");
    }
    let codex = &v["backends"][1];
    assert_eq!((codex["name"].as_str(), codex["installed"].as_bool(), codex["used"].as_bool()), (Some("codex"), Some(false), Some(false)));
    assert!(codex["path"].is_null() && codex["version"].is_null());

    assert_eq!(v["isolation"]["none"]["available"], true);
    assert_eq!(v["isolation"]["none"]["requested"], true);
    assert_eq!(v["isolation"]["sandbox"]["available"], false, "no wrapper on this PATH");
    assert_eq!(v["isolation"]["container"]["available"], false);
    assert!(v["container_engine"].is_null());
    assert!(!dir.join("judge_ran").exists() && !dir.join("agent_ran").exists(), "doctor must run nothing");
}

/// A requested tier the host lacks is a problem AT ITS KEY, and the tier says it was requested.
#[test]
fn doctor_json_flags_an_unavailable_requested_tier() {
    let (tmp, path) = project(BAD_TIER);
    let out = agg(tmp.path(), &path, &["doctor", "--json"]);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["ok"], false);
    assert_eq!(v["isolation"]["sandbox"]["requested"], true);
    assert_eq!(problem_paths(&v), ["steps.worker.isolation"]);
}

/// Doctor adds the one check startup does not make: the forward state file exists.
#[test]
fn doctor_json_flags_a_missing_state_file_and_a_missing_config() {
    let (tmp, path) = project(GOOD);
    fs::remove_file(tmp.path().join("agg/state/STATE.md")).unwrap();
    let v = json(&agg(tmp.path(), &path, &["doctor", "--json"]));
    assert_eq!(problem_paths(&v), ["defaults.state"]);
    assert_eq!(v["config"]["valid"], true, "a missing state file is not a config error");

    let empty = tempfile::tempdir().unwrap();
    let out = agg(empty.path(), &path, &["doctor", "--json"]);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!((v["config"]["found"].as_bool(), v["ok"].as_bool()), (Some(false), Some(false)));
}

/// A container engine that answers is reported by name, and makes the container tier available.
#[test]
fn doctor_json_names_a_running_container_engine() {
    let (tmp, path) = project(&GOOD.replace("steps: { worker: {} }", "steps: { worker: { isolation: \"container\" } }"));
    write(tmp.path(), "bin/docker", "#!/bin/sh\nexit 0\n");
    let out = agg(tmp.path(), &path, &["doctor", "--json"]);
    let v = json(&out);
    assert_eq!(out.status.code(), Some(0), "{v:#}");
    assert_eq!(v["container_engine"], "docker");
    assert_eq!(v["isolation"]["container"], serde_json::json!({ "available": true, "mechanism": "docker", "requested": true }));
}
