//! `verdicts.jsonl` end-to-end (§5.8): the durable, append-only GATE record is written by a real
//! `agg run`, with the right `outcome` per session disposition, and it SURVIVES across separate
//! invocations. A fake, committing `claude` on PATH drives the loop — no model, no network.
//!
//! Unix-only (the stub + PATH shimming use sh), like `tests/cli.rs`.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::Command;

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

fn git(dir: &Path, args: &[&str]) {
    Command::new("git").args(args).current_dir(dir).output().unwrap();
}

/// A clean git repo on `main` with one empty commit — mandatory session isolation needs it. The
/// empty commit keeps `bin/`, `agg/`, `judges/` UNTRACKED (which `is_clean` ignores), so sessions
/// branch from a born `main`.
fn git_init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "agg baseline"]);
}

fn agg(dir: &Path, path: &str) -> Command {
    let mut c = Command::cargo_bin("agg").expect("agg binary built");
    c.current_dir(dir).env("PATH", path);
    c
}

fn combined(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// Parse every line of `agg/state/verdicts.jsonl` into a JSON value.
fn read_rows(dir: &Path) -> Vec<serde_json::Value> {
    let text = fs::read_to_string(dir.join("agg/state/verdicts.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each verdicts.jsonl line must be valid JSON"))
        .collect()
}

/// A fake `claude` on a private `bin/` that, on a `-p` run, creates `feature_done` and COMMITS it
/// on the session branch (so the merge stages something real), then emits one stream-json result.
/// Returns (project_dir, PATH-with-fake-claude-first).
fn committing_project(project: &str, goals: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
: > feature_done
git add feature_done >/dev/null 2>&1
git commit -qm "worker: feature_done" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));

    write(dir, "judges/feature.sh", "#!/bin/sh\n[ -f feature_done ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"done\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    chmod_x(&dir.join("judges/feature.sh"));
    write(dir, "agg/goals.yaml", goals);
    write(
        dir,
        "agg/agg.yaml",
        &format!("project: {project}\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"),
    );
    write(dir, "agg/AGG_RESUME.md", "create feature_done\n");
    git_init(dir);

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (tmp, path)
}

#[test]
fn baseline_and_merged_rows_are_written_and_survive_a_restart() {
    // Run 1: baseline judges `feature` not-met (no file), the worker commits it, the merge is kept
    // → one `baseline` row (session null, met false) and one `merged` row (session 1, met true).
    // Run 2 on the SAME dir: `feature` is already satisfied at launch, so it stops after the
    // baseline — but the run-1 rows must still be there, with a second `baseline` row appended.
    let goals = "goals:\n  - id: feature\n    type: binary\n    judge: { kind: script, cmd: \"./judges/feature.sh\" }\nstop_when: feature\n";
    let (tmp, path) = committing_project("verd", goals);
    let dir = tmp.path();

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let c = combined(&out);
    assert_eq!(out.status.code(), Some(0), "the worker meets the goal → exit 0:\n{c}");

    let rows = read_rows(dir);
    // the baseline pass seeded session 1: outcome baseline, session null, not yet met.
    assert!(
        rows.iter().any(|r| r["outcome"].as_str() == Some("baseline")
            && r["session"].is_null()
            && r["met"].as_bool() == Some(false)
            && r["judge"].as_str() == Some("feature")),
        "a baseline row (session null, met false) must be written before session 1:\n{rows:#?}"
    );
    // session 1 merged → outcome merged, session 1, met true.
    assert!(
        rows.iter().any(|r| r["outcome"].as_str() == Some("merged")
            && r["session"].as_u64() == Some(1)
            && r["met"].as_bool() == Some(true)),
        "a merged session must append a merged row stamped with its session:\n{rows:#?}"
    );
    // envelope fields the spec fixes: `step` is the constant, `ts` is a real epoch second.
    assert!(rows.iter().all(|r| r["step"].as_str() == Some("worker")), "step is the 'worker' constant:\n{rows:#?}");
    assert!(rows.iter().all(|r| r["ts"].as_u64().unwrap_or(0) > 0), "ts is a real wall-clock epoch second:\n{rows:#?}");
    let n_run1 = rows.len();
    assert!(n_run1 >= 2, "run 1 writes at least a baseline + a merged row, got {n_run1}");

    // ── restart ─────────────────────────────────────────────────────────────────────────────
    let out2 = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let c2 = combined(&out2);
    assert_eq!(out2.status.code(), Some(0), "feature is already met on base → exit 0:\n{c2}");
    assert!(c2.contains("already satisfied at launch"), "run 2 stops at the baseline:\n{c2}");

    let rows2 = read_rows(dir);
    assert!(rows2.len() > n_run1, "run 2 must APPEND — the run-1 rows survive the restart (had {n_run1}, now {})", rows2.len());
    assert!(
        rows2.iter().any(|r| r["outcome"].as_str() == Some("merged") && r["session"].as_u64() == Some(1)),
        "run-1's merged row must still be present after the restart:\n{rows2:#?}"
    );
    assert_eq!(
        rows2.iter().filter(|r| r["outcome"].as_str() == Some("baseline")).count(),
        2,
        "each run writes its own baseline row (durable, cross-run):\n{rows2:#?}"
    );
}

#[test]
fn a_rolled_back_session_writes_rolled_back_rows_and_no_merged_row() {
    // A previously-met invariant regresses on the merged tree → the whole session rolls back. Its
    // verdicts must land as `rolled_back` (never authoritative for a later "was met"), and NOTHING
    // may be recorded as `merged`.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: breaks the build invariant (drops `.regressed`) and commits a change on its branch.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
printf 'broke-it\n' >> tracked.txt
touch .regressed
git add -A >/dev/null 2>&1
git commit -qm "worker change" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    write(dir, "tracked.txt", "ok\n");
    // build_ok invariant: met at baseline, REGRESSES once `.regressed` exists on the merged tree.
    write(dir, "judges/build.sh", "#!/bin/sh\n[ -f .regressed ] && echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"broke the build\"}' || echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    chmod_x(&dir.join("judges/build.sh"));
    // feature: never met, so the loop actually launches a worker (doesn't stop at baseline).
    write(dir, "judges/feature.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    chmod_x(&dir.join("judges/feature.sh"));
    write(
        dir,
        "agg/goals.yaml",
        "goals:\n  \
         - id: build_ok\n    type: binary\n    invariant: true\n    judge: { kind: script, cmd: \"./judges/build.sh\" }\n  \
         - id: feature\n    type: binary\n    judge: { kind: script, cmd: \"./judges/feature.sh\" }\nstop_when: feature\n",
    );
    write(
        dir,
        "agg/agg.yaml",
        "project: rbkv\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: false }\nsession_isolation: { rollback_on_regression: true }\n",
    );
    write(dir, "agg/AGG_RESUME.md", "do work\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let c = combined(&out);
    assert_eq!(out.status.code(), Some(4), "`feature` never met → session cap (exit 4):\n{c}");
    assert!(c.contains("ROLLED BACK"), "the regressing merge must be rolled back:\n{c}");

    let rows = read_rows(dir);
    // baseline seeded build_ok as met — the "was met" the gate regressed against.
    assert!(
        rows.iter().any(|r| r["outcome"].as_str() == Some("baseline")
            && r["judge"].as_str() == Some("build_ok")
            && r["met"].as_bool() == Some(true)),
        "the baseline must record build_ok as met (the seed the gate compares against):\n{rows:#?}"
    );
    // the rolled-back session's verdicts land as `rolled_back`, stamped with session 1.
    assert!(
        rows.iter().any(|r| r["outcome"].as_str() == Some("rolled_back")
            && r["session"].as_u64() == Some(1)
            && r["judge"].as_str() == Some("build_ok")
            && r["met"].as_bool() == Some(false)),
        "a rolled-back session must append rolled_back rows:\n{rows:#?}"
    );
    // and NOTHING may be recorded as merged — nothing landed.
    assert!(
        !rows.iter().any(|r| r["outcome"].as_str() == Some("merged")),
        "a rolled-back session must not write a merged row:\n{rows:#?}"
    );
}
