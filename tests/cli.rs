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
# the result carries total_cost_usd so the dollar-budget plumbing has real data to sum.
: > did_work
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.05}'
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
fn config_lives_in_the_optional_agg_folder() {
    // Put ALL user config under `agg/` and prove the loop finds + uses it: agg.yaml,
    // goals.yaml, the resume prompt, and the judge all resolve through config_base, while the
    // judge SCRIPT still runs from the project root (so `did_work` lands where the next judge
    // looks for it). This is the end-to-end proof of the opt-in config folder.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // judge lives under agg/judges/ and checks a root-level marker the worker creates.
    write(
        dir,
        "agg/judges/check.sh",
        "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true}' || echo '{\"met\":false}'\n",
    );
    chmod_x(&dir.join("agg/judges/check.sh"));
    // the judge cmd path is relative to the PROJECT ROOT (scripts run there), hence agg/judges/…
    write(
        dir,
        "agg/goals.yaml",
        "goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: \"./agg/judges/check.sh\" }\nstop_when: worked\n",
    );
    write(
        dir,
        "agg/agg.yaml",
        "project: folded\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\n",
    );
    // resume prompt resolves against config_base (the agg/ folder), so it sits inside it.
    write(dir, "agg/AGG_RESUME.md", "create the file did_work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "foldered agg run failed:\n{combined}");
    assert!(dir.join("did_work").exists(), "worker should create did_work at the project root");
    assert!(
        combined.contains("STOP condition satisfied"),
        "foldered config should drive the loop to its stop condition, got:\n{combined}"
    );
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

    // `agg status` and `dashboard --once` must read the snapshot the run just published —
    // showing the met goal — WITHOUT re-running judges. (Both go through status::render.)
    for args in [vec!["status"], vec!["dashboard", "--once"]] {
        let snap = agg(dir, &path).args(&args).output().unwrap();
        let text = String::from_utf8_lossy(&snap.stdout).into_owned();
        assert!(snap.status.success(), "`agg {args:?}` failed");
        assert!(
            text.contains("itest") && text.contains("worked"),
            "`agg {args:?}` should render the published snapshot (project + goal), got:\n{text}"
        );
    }
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

#[test]
fn judge_runs_one_goal_and_prints_raw_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = std::env::var("PATH").unwrap_or_default(); // no worker → real PATH is fine
    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: ok\n    type: binary\n    judge: { kind: script, cmd: \"echo '{\\\"met\\\":true,\\\"rationale\\\":\\\"fine\\\"}'\" }\nstop_when: ok\n",
    );
    write(dir, "agg.yaml", "project: jt\nresume_prompt: AGG_RESUME.md\n");
    write(dir, "AGG_RESUME.md", "noop\n");

    // a known goal: raw verdict JSON on stdout
    let out = agg(dir, &path).args(["judge", "ok"]).output().unwrap();
    assert!(out.status.success(), "judge ok failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"met\":true"), "stdout should be the raw verdict, got: {stdout}");

    // an unknown goal: error that lists the available ids
    let out = agg(dir, &path).args(["judge", "nope"]).output().unwrap();
    assert!(!out.status.success(), "unknown goal id must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no goal `nope`") && err.contains("ok"), "should list available ids, got: {err}");
}

#[test]
fn dollar_budget_halts_the_loop() {
    // End-to-end proof of #2: the worker reports total_cost_usd=0.05 per session; with a
    // cost cap of 0 and `halt_when: over_cost`, the FIRST session blows the cap and the loop
    // halts (the goal never gets a chance to be met). This exercises the whole chain:
    // stub result → cost_usd_from_result → SessionOutcome → loop accumulation → over_cost.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // a goal that can never be met (judge always reports not-met), so ONLY the cost guard
    // can end the loop — if cost weren't wired, the loop would run to max_sessions instead.
    write(dir, "judges/never.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    chmod_x(&dir.join("judges/never.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: impossible\n    type: binary\n    judge: { kind: script, cmd: \"./judges/never.sh\" }\nstop_when: impossible\nhalt_when: over_cost\n",
    );
    // cost.total: 0 → any spend (the stub's $0.05) is over budget.
    write(
        dir,
        "agg.yaml",
        "project: itest\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\ncost: { total: 0 }\n",
    );
    write(dir, "AGG_RESUME.md", "spend money\n");

    // generous session cap so the HALT (not the cap) is what stops us.
    let out = agg(dir, &path).args(["run", "--max-sessions", "20"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "agg run failed:\n{combined}");
    assert!(
        combined.contains("HALT") && combined.contains("over_cost"),
        "over_cost should halt the loop after the first spend, got:\n{combined}"
    );
    // it must NOT have run to the session cap — the dollar guard stops it early.
    assert!(
        !combined.contains("reached max_sessions"),
        "the cost guard, not max_sessions, should end the run:\n{combined}"
    );
}

#[test]
fn status_and_history_json_are_machine_readable() {
    // #10: `--json` on status + history emits parseable JSON of the existing serde types.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write(
        dir,
        "judges/check.sh",
        "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n",
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
        "project: jsonproj\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\ncost: { total: 5.0 }\n",
    );
    write(dir, "AGG_RESUME.md", "create the file did_work\n");

    // run once so both the snapshot (state.json) and the ledger (project.json) exist.
    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));

    // status --json: valid JSON, carries the project + the cost fields we added.
    let snap = agg(dir, &path).args(["status", "--json"]).output().unwrap();
    assert!(snap.status.success(), "status --json failed: {}", String::from_utf8_lossy(&snap.stderr));
    let v: serde_json::Value = serde_json::from_slice(&snap.stdout).expect("status --json must be valid JSON");
    assert_eq!(v["project"], "jsonproj");
    assert_eq!(v["cost_limit"], 5.0, "cost_limit should round-trip into the snapshot JSON");
    assert!(v["cost_spent"].as_f64().unwrap() > 0.0, "cost_spent should reflect the stub's spend");

    // history --json: valid JSON with a runs array containing at least our run.
    let hist = agg(dir, &path).args(["history", "--json"]).output().unwrap();
    assert!(hist.status.success(), "history --json failed: {}", String::from_utf8_lossy(&hist.stderr));
    let h: serde_json::Value = serde_json::from_slice(&hist.stdout).expect("history --json must be valid JSON");
    assert_eq!(h["name"], "jsonproj");
    assert!(h["runs"].as_array().map(|a| !a.is_empty()).unwrap_or(false), "history should have at least one run");
}

#[test]
fn institutional_memory_is_written_without_worker_cooperation() {
    // #3 ENFORCEMENT FLOOR: the default fake worker writes NO memory note, yet agg must still
    // produce AGG_MEMORY.md from mechanical facts — the worker is never trusted to persist.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // a goal that never meets, so the loop runs the full max_sessions and folds memory each time.
    write(dir, "judges/never.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    chmod_x(&dir.join("judges/never.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: impossible\n    type: binary\n    judge: { kind: script, cmd: \"./judges/never.sh\" }\nstop_when: impossible\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: memproj\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: true, max_kb: 64, inject_kb: 8 }\n",
    );
    write(dir, "AGG_RESUME.md", "do work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "agg run failed:\n{combined}");

    // the durable memory file must exist at the PROJECT ROOT, with a folded mechanical entry.
    let mem = dir.join("AGG_MEMORY.md");
    assert!(mem.exists(), "AGG_MEMORY.md must be written even when the worker writes no note");
    let text = fs::read_to_string(&mem).unwrap();
    assert!(text.contains("## session 1"), "session 1 folded into memory, got:\n{text}");
    assert!(text.contains("exited cleanly") || text.contains("Goals:"), "mechanical facts recorded:\n{text}");
    // the loop logs the fold.
    assert!(combined.contains("[memory] session #1 folded"), "fold should be logged:\n{combined}");
}

#[test]
fn worker_written_memory_note_is_folded() {
    // #3 Tier 3a: when the worker writes .agg/memory/session-<N>.md on a clean session, agg folds
    // that note (preferred over the mechanical fallback) into the durable AGG_MEMORY.md.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // a stub that, on a -p run, writes a worker memory note for session 1 then exits cleanly.
    let claude = bin.join("claude");
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
mkdir -p .agg/memory
printf 'GOTCHA: the frobnicator needs a warm cache before the second pass\n' > .agg/memory/session-1.md
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&claude);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    write(dir, "judges/never.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    chmod_x(&dir.join("judges/never.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  - id: impossible\n    type: binary\n    judge: { kind: script, cmd: \"./judges/never.sh\" }\nstop_when: impossible\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: memproj2\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: true }\n",
    );
    write(dir, "AGG_RESUME.md", "do work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "agg run failed:\n{combined}");

    let text = fs::read_to_string(dir.join("AGG_MEMORY.md")).unwrap();
    assert!(text.contains("GOTCHA: the frobnicator"), "worker note folded into memory, got:\n{text}");
    // the worker note is appended as a fenced, lower-trust hint after the mechanical fact —
    // never standing alone — so the fold source is 'mechanical+worker'.
    assert!(combined.contains("folded (mechanical+worker)"), "fold source should be 'mechanical+worker':\n{combined}");
    assert!(text.contains("UNTRUSTED hint"), "worker note flagged as untrusted hint:\n{text}");
    // exactly ONE entry for session 1 (the early floor was superseded, not double-folded).
    assert_eq!(text.matches("## session 1 (").count(), 1, "single entry per session, got:\n{text}");
    // the scratch note is cleaned up after folding.
    assert!(!dir.join(".agg/memory/session-1.md").exists(), "scratch note deleted after fold");
}

#[test]
fn rollback_gate_unlands_a_regressing_merge() {
    // #11 Phase 1 end-to-end: with session_isolation + rollback_on_regression on, a worker change
    // that makes a previously-met goal REGRESS must be rolled back — base stays pristine.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // fake claude: on a -p run, append a line to tracked.txt + COMMIT it on the session branch
    // (the worker's "work"). It also writes a marker so the judge can flip met→not-met after it runs.
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

    // a clean git repo on `main` with one committed file — isolation requires a clean repo.
    let g = |args: &[&str]| { std::process::Command::new("git").args(args).current_dir(dir).output().unwrap(); };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    write(dir, "tracked.txt", "ok\n");
    // build_ok (invariant): met at baseline, REGRESSES once the worker drops `.regressed`.
    write(dir, "judges/build.sh", "#!/bin/sh\n[ -f .regressed ] && echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"broke the build\"}' || echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    chmod_x(&dir.join("judges/build.sh"));
    // feature: never met (so the loop actually launches a worker rather than stopping at baseline).
    write(dir, "judges/feature.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    chmod_x(&dir.join("judges/feature.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  \
         - id: build_ok\n    type: binary\n    invariant: true\n    judge: { kind: script, cmd: \"./judges/build.sh\" }\n  \
         - id: feature\n    type: binary\n    judge: { kind: script, cmd: \"./judges/feature.sh\" }\nstop_when: feature\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: rbk\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: false }\nsession_isolation: { enabled: true, rollback_on_regression: true }\n",
    );
    write(dir, "AGG_RESUME.md", "do work\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "agg run failed:\n{combined}");
    assert!(combined.contains("ROLLED BACK"), "the regressing merge must be rolled back:\n{combined}");
    // base must be pristine: the worker's "broke-it" line must NOT be on main.
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(!content.contains("broke-it"), "base must NOT contain the rolled-back change, got: {content:?}");
    assert!(content.contains("ok"), "base keeps its original content");
}

#[test]
fn rollback_gate_keeps_merge_when_a_judge_merely_flakes() {
    // Regression test for the delta-clause bug: a previously-MET goal whose judge FAILS transiently
    // (rate-limit/timeout/error → Verdict::failed, error set → Goal marks it Regressed) must NOT
    // trigger a rollback. A flake is "judge couldn't run", not "the work regressed" — discarding a
    // good session's merge because a judge flaked is the bug. The good work must be KEPT.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: does clean, GOOD work on its session branch (adds a wanted line + commits).
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
printf 'good-work\n' >> tracked.txt
touch .flake
git add -A >/dev/null 2>&1
git commit -qm "worker change" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    let g = |args: &[&str]| { std::process::Command::new("git").args(args).current_dir(dir).output().unwrap(); };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    write(dir, "tracked.txt", "ok\n");
    // build_ok (invariant): met at baseline; once the worker drops `.flake`, the judge ERRORS —
    // exits non-zero with no verdict JSON → Verdict::failed (error set), NOT a clean not-met.
    write(dir, "judges/build.sh", "#!/bin/sh\nif [ -f .flake ]; then echo 'transient judge failure' >&2; exit 3; fi\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    chmod_x(&dir.join("judges/build.sh"));
    // feature: never met, so the loop actually runs a session (doesn't stop at baseline).
    write(dir, "judges/feature.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    chmod_x(&dir.join("judges/feature.sh"));
    write(
        dir,
        "goals.yaml",
        "goals:\n  \
         - id: build_ok\n    type: binary\n    invariant: true\n    judge: { kind: script, cmd: \"./judges/build.sh\" }\n  \
         - id: feature\n    type: binary\n    judge: { kind: script, cmd: \"./judges/feature.sh\" }\nstop_when: feature\n",
    );
    write(
        dir,
        "agg.yaml",
        "project: flake\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: false }\nsession_isolation: { enabled: true, rollback_on_regression: true }\n",
    );
    write(dir, "AGG_RESUME.md", "do work\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "agg run failed:\n{combined}");
    // the flake must NOT have rolled anything back — the good work is KEPT on main.
    assert!(!combined.contains("ROLLED BACK"), "a transient judge flake must NOT trigger rollback:\n{combined}");
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(content.contains("good-work"), "the worker's good work must be KEPT despite the judge flake, got: {content:?}");
}
