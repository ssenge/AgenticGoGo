//! End-to-end integration tests driving the real `agg` binary.
//!
//! The loop spawns `claude -p` workers, so these tests put a FAKE `claude` on PATH: a tiny
//! shell stub that emits valid stream-json and, as a side effect, advances the project state
//! so the judge can flip a goal to met. That lets us exercise the genuinely risky path — the
//! actual launch → stream → judge → stop machinery — without a real model or network.
//!
//! Unix-only (the stub + PATH shimming use sh). The harness's own platform is unix-first.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Write `content` to `dir/name`, creating parents. Returns the full path.
fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, content).unwrap();
    p
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

/// Build a throwaway directory holding a fake `claude` on a private `bin/`, and return
/// (project_dir, PATH-with-fake-claude-prepended). The fake claude, when invoked, writes a
/// marker file `did_work` into the project dir and emits one stream-json result line.
fn project_with_fake_claude() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    // The fake `claude`: handles `--version` (preflight) and a `-p` run. On a `-p` run it
    // touches `did_work` in its CWD (the project dir) and prints a minimal stream-json result
    // so the worker reader + token accounting have something well-formed to parse.
    let claude = bin.join("claude");
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
# a -p run: do the "work" (create the file the judge checks), emit one result event.
: > did_work
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1}}'
exit 0
"#,
    );
    chmod_x(&claude);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (tmp, path)
}

/// `agg` command rooted at `dir`, with the given PATH (so the fake claude is found).
fn agg(dir: &Path, path: &str) -> Command {
    let mut c = Command::cargo_bin("agg").expect("agg binary built");
    c.current_dir(dir).env("PATH", path);
    c
}

#[test]
fn init_then_plan_shows_scoreboard() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // PATH doesn't matter for init/plan (no worker launched); keep the real one.
    let path = std::env::var("PATH").unwrap_or_default();

    let out = agg(dir, &path).arg("init").output().unwrap();
    assert!(out.status.success(), "agg init failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.join("agg.yaml").exists(), "init should scaffold agg.yaml");
    assert!(dir.join("goals.yaml").exists(), "init should scaffold goals.yaml");

    let out = agg(dir, &path).arg("plan").output().unwrap();
    assert!(out.status.success(), "agg plan failed: {}", String::from_utf8_lossy(&out.stderr));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("Goals"), "plan output should show a scoreboard, got:\n{combined}");
}

#[test]
fn run_drives_a_correction_loop_to_stop() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // A goal met when `did_work` exists. The judge prints the verdict JSON contract.
    write(
        dir,
        "judges/check.sh",
        r#"#!/bin/sh
if [ -f did_work ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"did_work present"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
fi
"#,
    );
    chmod_x(&dir.join("judges/check.sh"));

    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: \"./judges/check.sh\" }\nstop_when: worked\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: itest\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\n",
    );
    write(dir, "AGG_RESUME.md", "create the file did_work\n");

    // Cap sessions so a logic bug can't hang the test. One fake session should suffice:
    // baseline judge says not-met → launch worker (creates did_work) → judge met → stop.
    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "agg run failed:\n{combined}");
    assert!(dir.join("did_work").exists(), "the fake worker should have created did_work");
    assert!(
        combined.contains("STOP condition satisfied"),
        "loop should reach its stop condition, got:\n{combined}"
    );
}

#[test]
fn run_stops_immediately_when_goal_already_met() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // Pre-create the marker so the baseline judge is already satisfied → zero sessions.
    write(dir, "did_work", "");
    write(
        dir,
        "judges/check.sh",
        "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true}' || echo '{\"met\":false}'\n",
    );
    chmod_x(&dir.join("judges/check.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: \"./judges/check.sh\" }\nstop_when: worked\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: itest\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\n",
    );
    write(dir, "AGG_RESUME.md", "noop\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "agg run failed:\n{combined}");
    assert!(
        combined.contains("already satisfied at launch"),
        "an already-met goal should stop before any session, got:\n{combined}"
    );
}

#[test]
fn run_without_config_gives_actionable_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = std::env::var("PATH").unwrap_or_default();
    let out = agg(dir, &path).arg("run").output().unwrap();
    assert!(!out.status.success(), "run with no config must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("agg init") || err.contains("/agg:new"),
        "missing-config error should point at init/new, got:\n{err}"
    );
}

#[test]
fn doctor_flags_a_broken_setup() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = std::env::var("PATH").unwrap_or_default();
    // No config at all → doctor should fail and name what's missing.
    let out = agg(dir, &path).arg("doctor").output().unwrap();
    assert!(!out.status.success(), "doctor on an empty dir should report failures");
}
