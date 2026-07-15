//! End-to-end integration tests driving the real `agg` binary.
//!
//! A judge IS a goal now (§7.1): there is no `goals.yaml`. A judge is a file at
//! `agg/judges/<name>.sh` (or `.md`), resolved by the NAME referenced in `done_if` / `invariants` /
//! an `if` condition. The config is one file, `agg/agg.yaml` (defaults/judge/steps/sequence).
//!
//! The loop spawns `claude -p` workers, so these tests put a FAKE `claude` on PATH: a tiny shell
//! stub that emits valid stream-json and, as a side effect, advances the project state so a judge
//! can flip to met. That exercises the genuinely risky path — the actual launch → stream → judge →
//! gate machinery — without a real model or network.
//!
//! Session isolation is MANDATORY (§4.1), so every `agg run` needs a git repo, a clean tree and a
//! non-detached HEAD. HOME is redirected into each test's temp dir so the `~/.agg/judges` standard
//! library install (`ensure_library`) never touches the developer's real home.
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

/// Write a script judge to `agg/judges/<name>.sh` (the project judges dir, §5.1) and make it
/// executable. `name` is what `done_if` / `invariants` / an `if` condition references.
fn write_judge(dir: &Path, name: &str, body: &str) {
    let p = write(dir, &format!("agg/judges/{name}.sh"), body);
    chmod_x(&p);
}

/// A single-`worker`-step `agg.yaml`. `done_if` is the DoD expression; `seq_extra` is extra lines
/// under `sequence:` (each already 2-space indented, may be ""); `top_extra` is extra top-level
/// lines (may be ""). `summary` is off — these fake-worker tests make no ruler calls.
fn cfg(project: &str, done_if: &str, seq_extra: &str, top_extra: &str) -> String {
    format!(
        "project: {project}\n\
         defaults: {{ model: fake }}\n\
         steps:\n  worker: {{}}\n\
         sequence:\n  steps: [worker]\n  done_if: \"{done_if}\"\n\
         {seq_extra}summary: {{ enabled: false }}\n{top_extra}"
    )
}

/// Write `agg/agg.yaml` (via [`cfg`]) + the forward state file `agg/AGG_STATE.md`.
fn write_cfg(dir: &Path, project: &str, done_if: &str, seq_extra: &str, top_extra: &str) {
    write(dir, "agg/agg.yaml", &cfg(project, done_if, seq_extra, top_extra));
    write(dir, "agg/AGG_STATE.md", "do work\n");
}

/// Turn `dir` into a clean git repo on `main` with one (empty) commit. Session isolation is
/// MANDATORY, so `agg run` refuses to start without a git repo + clean tree + non-detached HEAD;
/// every test that drives the loop needs this. The commit is empty so the config, the fake
/// `claude`, and the worker's output all stay UNTRACKED — which `is_clean` ignores, so the tree
/// reads clean and sessions branch from a born `main`.
fn git_init(dir: &Path) {
    let g = |args: &[&str]| {
        Command::new("git").args(args).current_dir(dir).output().unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    g(&["commit", "-q", "--allow-empty", "-m", "agg baseline"]);
}

/// Assert `agg run` ended with a specific exit code. Codes: 0 done/stopped, 3 abort, 4 max-sessions,
/// 1 hard error. (A run that reaches the session cap with the DoD unmet exits 4.)
fn assert_exit(out: &std::process::Output, code: i32, combined: &str) {
    assert_eq!(out.status.code(), Some(code), "expected exit {code}:\n{combined}");
}

/// Build a throwaway directory holding a fake `claude` on a private `bin/`, and return
/// (project_dir, PATH-with-fake-claude-prepended). The fake claude, when invoked, writes a marker
/// file `did_work` into the project dir and emits one stream-json result line.
fn project_with_fake_claude() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    // The fake `claude`: handles `--version` (preflight) and a `-p` run. On a `-p` run it creates
    // `did_work` in its CWD (the project dir) and COMMITS it on the session branch, then prints a
    // minimal stream-json result. The commit matters: session isolation is mandatory (§5.7), and
    // UNCOMMITTED work resolves as `NoChanges` — the gate then restores base truth and the judge's
    // met verdict on it never counts. A worker that means its work commits it. The result carries
    // total_cost_usd so the dollar-budget plumbing has real data to sum.
    let claude = bin.join("claude");
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.05}'
exit 0
"#,
    );
    chmod_x(&claude);
    // mandatory session isolation needs a clean git base to branch from.
    git_init(tmp.path());

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (tmp, path)
}

/// `agg` command rooted at `dir`, with the given PATH (so the fake claude is found). HOME is
/// redirected into the project temp so `ensure_library` writes the standard judge library there,
/// never the developer's real `~/.agg/judges`.
fn agg(dir: &Path, path: &str) -> Command {
    let mut c = Command::cargo_bin("agg").expect("agg binary built");
    c.current_dir(dir).env("PATH", path).env("HOME", dir.join(".home"));
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
    // init scaffolds ONE config file + the forward state file + a starter judge. No goals.yaml,
    // no AGG_RESUME.md (§7.1).
    assert!(dir.join("agg/agg.yaml").exists(), "init should scaffold agg.yaml");
    assert!(dir.join("agg/AGG_STATE.md").exists(), "init should scaffold the forward state file");
    assert!(dir.join("agg/judges/tests_pass.sh").exists(), "init should scaffold a starter judge");

    let out = agg(dir, &path).arg("plan").output().unwrap();
    assert!(out.status.success(), "agg plan failed: {}", String::from_utf8_lossy(&out.stderr));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("Goals"), "plan output should show a scoreboard, got:\n{combined}");
    // `plan` RE-RUNS the judges rather than reading a snapshot — proven by its output carrying the
    // rationale the scaffolded judge SCRIPT prints, which only exists if the script executed.
    assert!(
        combined.contains("replace me"),
        "plan should re-run the judges and show their live rationale, got:\n{combined}"
    );
}

/// `agg doctor` on a COMPLETE setup exits 0 and confirms the agent CLI is on PATH — the
/// happy-path counterpart to `doctor_flags_a_broken_setup`. (`agg init` supplies the config, so
/// this also proves the scaffold it writes is one doctor actually accepts.)
#[test]
fn doctor_passes_a_good_setup() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    let init = agg(dir, &path).arg("init").output().unwrap();
    assert!(init.status.success(), "init failed: {}", String::from_utf8_lossy(&init.stderr));

    let out = agg(dir, &path).arg("doctor").output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_exit(&out, 0, &combined);
    assert!(combined.contains("claude"), "doctor should report the agent CLI on PATH:\n{combined}");
}

/// The DoD never met + the session cap reached → exit 4, with a banner naming the cap. The exit
/// code alone isn't enough: 4 must be distinguishable from an ABORT (3) by what it PRINTS too.
#[test]
fn max_sessions_cap_exits_4_and_says_so() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // a judge that is NEVER satisfied → the loop can only end by hitting the cap.
    write_judge(dir, "worked", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "cap", "worked", "", "");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_exit(&out, 4, &combined);
    assert!(
        combined.contains("reached max_sessions=2"),
        "the cap must be named in the banner, not just the exit code:\n{combined}"
    );
}

#[test]
fn config_lives_in_the_agg_folder() {
    // ALL user config lives under the mandatory `agg/` folder and the loop finds + uses it:
    // agg.yaml, the state file, and the judge (resolved by NAME from agg/judges/) all resolve
    // through config_base, while the judge SCRIPT still runs from the project root (so `did_work`
    // lands where the next judge looks for it). This is the end-to-end proof of the config folder.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // judge `worked` lives under agg/judges/ and checks a root-level marker the worker creates.
    write_judge(dir, "worked", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true}' || echo '{\"met\":false}'\n");
    write_cfg(dir, "folded", "worked", "", "");
    write(dir, "agg/AGG_STATE.md", "create the file did_work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "foldered agg run failed:\n{combined}");
    assert!(dir.join("did_work").exists(), "worker should create did_work at the project root");
    assert!(
        combined.contains("done_if satisfied"),
        "foldered config should drive the loop to its Definition of Done, got:\n{combined}"
    );
}

#[test]
fn run_drives_a_correction_loop_to_stop() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // A DoD judge met when `did_work` exists. The judge prints the verdict JSON contract.
    write_judge(
        dir,
        "worked",
        r#"#!/bin/sh
if [ -f did_work ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"did_work present"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
fi
"#,
    );
    write_cfg(dir, "itest", "worked", "", "");
    write(dir, "agg/AGG_STATE.md", "create the file did_work\n");

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
        combined.contains("done_if satisfied"),
        "loop should reach its Definition of Done, got:\n{combined}"
    );

    // `agg status` and `dashboard --once` must read the snapshot the run just published —
    // showing the met judge — WITHOUT re-running judges. (Both go through status::render.)
    for args in [vec!["status"], vec!["dashboard", "--once"]] {
        let snap = agg(dir, &path).args(&args).output().unwrap();
        let text = String::from_utf8_lossy(&snap.stdout).into_owned();
        assert!(snap.status.success(), "`agg {args:?}` failed");
        assert!(
            text.contains("itest") && text.contains("worked"),
            "`agg {args:?}` should render the published snapshot (project + judge), got:\n{text}"
        );
    }
}

#[test]
fn run_stops_immediately_when_goal_already_met() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // Pre-create the marker so the baseline judge is already satisfied → zero sessions.
    write(dir, "did_work", "");
    write_judge(dir, "worked", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true}' || echo '{\"met\":false}'\n");
    write_cfg(dir, "itest", "worked", "", "");

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "agg run failed:\n{combined}");
    assert!(
        combined.contains("already satisfied at launch"),
        "an already-met DoD should stop before any session, got:\n{combined}"
    );
    // and it burned ZERO sessions — the point of the baseline check is to spend nothing.
    let snap = fs::read_to_string(dir.join("agg/state/state.json")).expect("state.json published");
    let v: serde_json::Value = serde_json::from_str(&snap).expect("state.json parses");
    assert_eq!(v["session"], 0, "a baseline-satisfied run must launch no worker:\n{snap}");
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
fn judge_runs_one_name_and_prints_raw_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = std::env::var("PATH").unwrap_or_default(); // no worker → real PATH is fine
    // a judge is a FILE resolved by NAME now (§5.1) — no goals.yaml, no inline cmd.
    write_judge(dir, "ok", "#!/bin/sh\necho '{\"met\":true,\"rationale\":\"fine\"}'\n");
    write(dir, "agg/agg.yaml", "project: jt\nsteps: { worker: {} }\nsequence: { steps: [worker], done_if: ok }\n");
    write(dir, "agg/AGG_STATE.md", "noop\n");

    // a known judge: raw verdict JSON on stdout
    let out = agg(dir, &path).args(["judge", "ok"]).output().unwrap();
    assert!(out.status.success(), "judge ok failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"met\":true"), "stdout should be the raw verdict, got: {stdout}");

    // an unknown judge: error that lists what IS available
    let out = agg(dir, &path).args(["judge", "nope"]).output().unwrap();
    assert!(!out.status.success(), "unknown judge name must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no judge named `nope`") && err.contains("ok"),
        "should name the miss and list available judges, got: {err}"
    );
}

#[test]
fn dollar_budget_aborts_the_loop() {
    // End-to-end proof: the worker reports total_cost_usd=0.05 per session; with a cost cap of 0
    // and `abort_if: over_cost`, the FIRST session blows the cap and the loop ABORTS (the DoD never
    // gets a chance to be met). This exercises the whole chain: stub result → cost_usd_from_result →
    // SessionOutcome → loop accumulation → over_cost. `cost` now lives under `sequence:` (§4.1).
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();

    // a judge that can never be met, so ONLY the cost guard can end the loop — if cost weren't
    // wired, the loop would run to max_sessions instead.
    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    // cost.total: 0 → any spend (the stub's $0.05) is over budget.
    write_cfg(dir, "itest", "impossible", "  abort_if: \"over_cost\"\n  cost: { total: 0 }\n", "");

    // generous session cap so the ABORT (not the cap) is what stops us.
    let out = agg(dir, &path).args(["run", "--max-sessions", "20"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // an ABORT is exit code 3 (a guard fired — NOT success), so automation can branch on it.
    assert_eq!(out.status.code(), Some(3), "an ABORT must exit 3:\n{combined}");
    assert!(
        combined.contains("ABORT") && combined.contains("over_cost"),
        "over_cost should abort the loop after the first spend, got:\n{combined}"
    );
    // it must NOT have run to the session cap — the dollar guard stops it early.
    assert!(
        !combined.contains("reached max_sessions"),
        "the cost guard, not max_sessions, should end the run:\n{combined}"
    );
}

#[test]
fn status_and_history_json_are_machine_readable() {
    // `--json` on status + history emits parseable JSON of the existing serde types.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "worked", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n");
    write_cfg(dir, "jsonproj", "worked", "  cost: { total: 5.0 }\n", "");
    write(dir, "agg/AGG_STATE.md", "create the file did_work\n");

    // run once so both the snapshot (state.json) and the ledger (project.json) exist.
    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));

    // status --json: valid JSON, carries the project + the cost fields.
    let snap = agg(dir, &path).args(["status", "--json"]).output().unwrap();
    assert!(snap.status.success(), "status --json failed: {}", String::from_utf8_lossy(&snap.stderr));
    let v: serde_json::Value = serde_json::from_slice(&snap.stdout).expect("status --json must be valid JSON");
    assert_eq!(v["project"], "jsonproj");
    assert_eq!(v["cost_limit"], 5.0, "cost_limit should round-trip into the snapshot JSON");
    assert!(v["cost_spent"].as_f64().unwrap() > 0.0, "cost_spent should reflect the stub's spend");

    // a finished run publishes the terminal stage + the ledger's machine-readable end reason.
    assert_eq!(v["finished"], true, "a completed run must be marked finished");
    assert_eq!(v["phase"], "done", "…and its final phase is `done` (the state.json wire value)");

    // history --json: valid JSON with a runs array containing at least our run.
    let hist = agg(dir, &path).args(["history", "--json"]).output().unwrap();
    assert!(hist.status.success(), "history --json failed: {}", String::from_utf8_lossy(&hist.stderr));
    let h: serde_json::Value = serde_json::from_slice(&hist.stdout).expect("history --json must be valid JSON");
    assert_eq!(h["name"], "jsonproj");
    let runs = h["runs"].as_array().expect("history should have a runs array");
    assert!(!runs.is_empty(), "history should have at least one run");
    assert_eq!(
        runs.last().unwrap()["end_reason"], "goals-met",
        "the ledger records WHY the run ended, not just that it did"
    );

    // The HUMAN renderers: `agg status` and the headless `agg dashboard --once` both read the same
    // published snapshot and must name the project and the judge.
    for args in [vec!["status"], vec!["dashboard", "--once"]] {
        let out = agg(dir, &path).args(&args).output().unwrap();
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_exit(&out, 0, &combined);
        assert!(combined.contains("jsonproj"), "`agg {args:?}` should render the project:\n{combined}");
        assert!(combined.contains("worked"), "`agg {args:?}` should render the judge:\n{combined}");
    }
}

#[test]
fn institutional_memory_is_written_without_worker_cooperation() {
    // ENFORCEMENT FLOOR: the default fake worker writes NO memory note, yet agg must still produce
    // AGG_MEMORY.md from mechanical facts — the worker is never trusted to persist.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // a judge that never meets, so the loop runs the full max_sessions and folds memory each time.
    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "memproj", "impossible", "", "memory: { enabled: true, max_kb: 64, inject_kb: 8 }\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // DoD is `impossible` → reaches the session cap unmet.

    // the durable memory file must exist under agg/state/, with a folded mechanical entry.
    let mem = dir.join("agg/state/AGG_MEMORY.md");
    assert!(mem.exists(), "AGG_MEMORY.md must be written even when the worker writes no note");
    let text = fs::read_to_string(&mem).unwrap();
    assert!(text.contains("## session 1"), "session 1 folded into memory, got:\n{text}");
    assert!(text.contains("exited cleanly") || text.contains("Goals:"), "mechanical facts recorded:\n{text}");
    // the loop logs the fold.
    assert!(combined.contains("[memory] session #1 folded"), "fold should be logged:\n{combined}");
}

#[test]
fn lifetime_session_is_published_to_state_json() {
    // Regression for the publish!-macro bug: `lifetime_session` was never copied into state.json,
    // so `agg status`/the dashboard always showed "#0 lifetime" and the "(of N)" total never
    // rendered. A run that completes 2 sessions must publish lifetime_session >= 2.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "lifeproj", "impossible", "", "");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined);

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("agg/state/state.json")).unwrap()).unwrap();
    let lifetime = state["lifetime_session"].as_u64().unwrap_or(0);
    assert!(lifetime >= 2, "lifetime_session must be published (was the publish! bug); got {lifetime}\nstate: {state}");
}

#[test]
fn worker_written_memory_note_is_folded() {
    // Tier 3a: when the worker writes agg/state/memory/session-<N>.md on a clean session, agg folds
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
mkdir -p agg/state/memory
printf 'GOTCHA: the frobnicator needs a warm cache before the second pass\n' > agg/state/memory/session-1.md
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&claude);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "memproj2", "impossible", "", "memory: { enabled: true }\n");
    git_init(dir); // mandatory session isolation needs a git base

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // DoD is `impossible` → reaches the session cap unmet.

    let text = fs::read_to_string(dir.join("agg/state/AGG_MEMORY.md")).unwrap();
    assert!(text.contains("GOTCHA: the frobnicator"), "worker note folded into memory, got:\n{text}");
    // the worker note is appended as a fenced, lower-trust hint after the mechanical fact —
    // never standing alone — so the fold source is 'mechanical+worker'.
    assert!(combined.contains("folded (mechanical+worker)"), "fold source should be 'mechanical+worker':\n{combined}");
    assert!(text.contains("UNTRUSTED hint"), "worker note flagged as untrusted hint:\n{text}");
    // exactly ONE entry for session 1 (the early floor was superseded, not double-folded).
    assert_eq!(text.matches("## session 1 (").count(), 1, "single entry per session, got:\n{text}");
    // the scratch note is cleaned up after folding.
    assert!(!dir.join("agg/state/memory/session-1.md").exists(), "scratch note deleted after fold");
}

#[test]
fn rollback_gate_unlands_a_regressing_merge() {
    // SAFETY-CRITICAL (§5.7): with session isolation + gate_regressions on (the default), a worker
    // change that makes a previously-met invariant judge REGRESS must be rolled back — base pristine.
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
    write_judge(dir, "build_ok", "#!/bin/sh\n[ -f .regressed ] && echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"broke the build\"}' || echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    // feature: never met (so the loop actually launches a worker rather than stopping at baseline).
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // done_if: feature; build_ok is the invariant that must STAY met.
    write_cfg(dir, "rbk", "feature", "  invariants: [build_ok]\n", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // `feature` never met → reaches the session cap.
    assert!(combined.contains("ROLLED BACK"), "the regressing merge must be rolled back:\n{combined}");
    // base must be pristine: the worker's "broke-it" line must NOT be on main.
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(!content.contains("broke-it"), "base must NOT contain the rolled-back change, got: {content:?}");
    assert!(content.contains("ok"), "base keeps its original content");
}

#[test]
fn rollback_gate_keeps_merge_when_a_judge_merely_flakes() {
    // SAFETY-CRITICAL companion: a previously-MET invariant whose judge FAILS transiently
    // (rate-limit/timeout/error → Verdict::failed, error set) must NOT trigger a rollback. A flake is
    // "judge couldn't run", not "the work regressed" — discarding a good session's merge because a
    // judge flaked is the bug. The good work must be KEPT.
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
    write_judge(dir, "build_ok", "#!/bin/sh\nif [ -f .flake ]; then echo 'transient judge failure' >&2; exit 3; fi\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    // feature: never met, so the loop actually runs a session (doesn't stop at baseline).
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "flake", "feature", "  invariants: [build_ok]\n", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // `feature` never met → reaches the session cap.
    // the flake must NOT have rolled anything back — the good work is KEPT on main.
    assert!(!combined.contains("ROLLED BACK"), "a transient judge flake must NOT trigger rollback:\n{combined}");
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(content.contains("good-work"), "the worker's good work must be KEPT despite the judge flake, got: {content:?}");
}

#[test]
fn rollback_gate_ignores_a_run_set_only_control_judge_flipping() {
    // SAFETY-CRITICAL (§5.7 / §5.3): the regression gate ranges over the DoD-set ONLY, exactly as
    // `any_regressed` does. A run-set-only control judge named solely in an `if` condition (here
    // `stalled`) is DESIGNED to flip met→unmet — that flip is the signal that fires `reconsider`. If
    // the gate treated the flip as a regression it would ROLL BACK the very work that escaped the
    // stall, and since rolled-back rows never land, every later step would keep "regressing" on the
    // stale window → livelock. The good work must be KEPT. (The buggy gate scanned ALL fresh_verdicts.)
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: does clean, GOOD work (adds a wanted line + commits) AND drops `.escaped`, which flips
    // the control judge `stalled` from met (at baseline) to not-met on this session's merged tree.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
printf 'good-work\n' >> tracked.txt
touch .escaped
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
    // stalled (run-set-only, named ONLY in the `if` condition → NOT in the DoD-set): met at baseline
    // (`.escaped` absent), then FLIPS to not-met once the worker drops `.escaped`. Its landed baseline
    // `met:true` is exactly what a naive gate would read as a regression when it goes false.
    write_judge(dir, "stalled", "#!/bin/sh\n[ -f .escaped ] && echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"escaped the stall\"}' || echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"stalled\"}'\n");
    // feature (the DoD judge, via done_if): never met → the loop runs a real session and reaches the
    // cap rather than stopping at baseline. false→false is not a regression, so it never rolls back.
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: stallflip\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n  reconsider: { skip_judges: true }\n\
         sequence:\n  steps:\n    - worker\n    - if stalled then reconsider\n  \
         done_if: \"feature\"\nsummary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write(dir, "agg/AGG_STATE.md", "do work\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // `feature` never met → reaches the session cap.
    // the control judge's flip must NOT have rolled anything back — the good work is KEPT on main.
    assert!(!combined.contains("ROLLED BACK"), "a run-set-only control judge flipping must NOT trigger rollback:\n{combined}");
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(content.contains("good-work"), "the worker's good work must be KEPT despite `stalled` flipping, got: {content:?}");
}

#[test]
fn a_broken_judge_does_not_abort_the_run_wearing_a_regressions_clothes() {
    // The HALF the flake test above does not cover: it proves the gate KEEPS the merge, but says
    // nothing about `abort_if`. This is the shipped bug end-to-end. A previously-MET invariant judge
    // that CRASHES has `met:false` on its `Verdict::failed` — which USED to fold into
    // `Lifecycle::Regressed`, which `any_regressed(invariants)` (the abort term `agg init` writes)
    // turned into an ABORT (exit 3). The run died blaming a regression that never happened. With the
    // fix, a broken judge leaves the judge's state intact, nothing regresses, the run does NOT abort,
    // and it simply reaches the session cap (exit 4) like any run that hasn't met its DoD.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: makes the invariant judge start CRASHING (drops `.flake`), does otherwise-clean work.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
printf 'work\n' >> tracked.txt
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
    // build_ok (invariant): met at baseline; once `.flake` exists the judge CRASHES — exits
    // non-zero with no verdict JSON → Verdict::failed (error set), NOT a clean not-met.
    write_judge(dir, "build_ok", "#!/bin/sh\nif [ -f .flake ]; then echo 'judge crashed' >&2; exit 1; fi\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    // feature: never met, so the loop runs sessions and does not stop at baseline.
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // abort_if carries `any_regressed(invariants)` — the exact term `agg init` ships that the bug
    // fired through. build_ok is the invariant it ranges over.
    write_cfg(dir, "brokenjudge", "feature", "  invariants: [build_ok]\n  abort_if: \"any_regressed(invariants)\"\n", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // exit 4 (session cap), NOT 3 (ABORT). A crashed judge must not masquerade as a regression.
    assert_exit(&out, 4, &combined);
    assert!(!combined.contains("ABORT"), "a crashed judge must NOT abort the run as a phantom regression:\n{combined}");
}

/// The four deterministic outer-loop stages must be OBSERVABLE, in order, in `agg/state/state.json`
/// while the loop runs — the TUI's `phase_color` and the web UI's `phaseStatus` key off these
/// exact strings, and nothing else asserts them.
///
/// We don't poll (a race); instead each stage records the phase the loop had published at the
/// moment that stage invoked it, through agg's OWN extension points:
///   `on_session_start` → INJECT · the worker → RUN · the judge → VERIFY · `on_session_end` → GATE
fn record_phase_stub(dir: &Path) -> String {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // append "<TAG>=<the phase the loop has published right now>" to trace.txt
    write(
        &bin,
        "rec",
        r#"#!/bin/sh
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' agg/state/state.json)" >> trace.txt
"#,
    );
    chmod_x(&bin.join("rec"));

    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
sh bin/rec RUN
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.0}'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));

    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())
}

#[test]
fn phase_names_the_four_outer_loop_stages() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = record_phase_stub(dir);

    write_judge(
        dir,
        "worked",
        r#"#!/bin/sh
sh bin/rec VERIFY
if [ -f did_work ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"did_work present"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
fi
"#,
    );
    write_cfg(
        dir,
        "phases",
        "worked",
        "",
        "hooks:\n  on_session_start: [\"sh bin/rec INJECT\"]\n  on_session_end: [\"sh bin/rec GATE\"]\n",
    );
    write(dir, "agg/AGG_STATE.md", "create the file did_work\n");
    git_init(dir); // mandatory session isolation needs a git base

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_exit(&out, 0, &combined);

    let trace = fs::read_to_string(dir.join("trace.txt")).expect("stages should have recorded");
    let seq: Vec<&str> = trace.lines().collect();

    // baseline VERIFY (judges run once before the first session) → one full cycle → stop.
    assert_eq!(
        seq,
        ["VERIFY=verify", "INJECT=inject", "RUN=run", "VERIFY=verify", "GATE=gate"],
        "each stage must publish its own name before handing control to user code:\n{trace}\n{combined}"
    );

    // and the run must settle on the terminal phase.
    let state = fs::read_to_string(dir.join("agg/state/state.json")).unwrap();
    assert!(state.contains(r#""phase":"done""#), "finished run should publish phase=done:\n{state}");
}

/// The mirror image: a run whose DoD is ALREADY met at launch does the baseline VERIFY and then
/// stops — it must never enter INJECT/RUN/GATE. The stage trace is the proof that no worker was
/// launched (an exit code alone can't distinguish "stopped at baseline" from "ran and succeeded").
#[test]
fn a_baseline_satisfied_run_enters_no_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = record_phase_stub(dir);

    write(dir, "did_work", ""); // pre-satisfy the DoD
    write_judge(
        dir,
        "worked",
        "#!/bin/sh\nsh bin/rec VERIFY\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n",
    );
    write_cfg(
        dir,
        "baseline",
        "worked",
        "",
        "hooks:\n  on_session_start: [\"sh bin/rec INJECT\"]\n  on_session_end: [\"sh bin/rec GATE\"]\n",
    );
    write(dir, "agg/AGG_STATE.md", "noop\n");
    git_init(dir); // mandatory session isolation needs a git base (did_work stays untracked → clean)

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_exit(&out, 0, &combined);

    let trace = fs::read_to_string(dir.join("trace.txt")).expect("the baseline judge should have run");
    let seq: Vec<&str> = trace.lines().collect();
    assert_eq!(
        seq,
        ["VERIFY=verify"],
        "a baseline-satisfied run must judge once and stop — no INJECT, no RUN, no GATE:\n{trace}\n{combined}"
    );
}

/// An interrupted session is never judged and never logs a session-exit line: the SIGINT check
/// sits between the worker returning and that log, so a killed session leaves no trace of having
/// "exited" normally. It still exits 0 (an operator stop is a clean end) through the Drop guards.
#[test]
fn interrupt_during_run_skips_verify_and_the_exit_log() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = record_phase_stub(dir);

    // a worker that records RUN and then hangs, giving us a window to signal the loop.
    write(
        &dir.join("bin"),
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
sh bin/rec RUN
sleep 30
"#,
    );
    chmod_x(&dir.join("bin/claude"));

    write_judge(
        dir,
        "worked",
        r#"#!/bin/sh
sh bin/rec VERIFY
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
"#,
    );
    write_cfg(
        dir,
        "intr",
        "worked",
        "",
        "hooks:\n  on_session_start: [\"sh bin/rec INJECT\"]\n  on_session_end: [\"sh bin/rec GATE\"]\n",
    );
    write(dir, "agg/AGG_STATE.md", "work\n");
    git_init(dir); // mandatory session isolation needs a git base

    let log = dir.join("run.log");
    let mut child = agg(dir, &path)
        .args(["run", "--max-sessions", "2"])
        .stdout(std::process::Stdio::from(fs::File::create(&log).unwrap()))
        .stderr(std::process::Stdio::from(fs::File::create(dir.join("run.err")).unwrap()))
        .spawn()
        .unwrap();

    // wait (bounded) until the worker has actually started, then Ctrl-C the loop.
    let trace_path = dir.join("trace.txt");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if fs::read_to_string(&trace_path).map(|t| t.contains("RUN=run")).unwrap_or(false) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "worker never reached RUN");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // no libc dep: this file is already unix-only and shells out freely.
    Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    let signalled_at = std::time::Instant::now();
    let status = child.wait().unwrap();
    let took = signalled_at.elapsed();

    let err = fs::read_to_string(dir.join("run.err")).unwrap_or_default();
    assert_eq!(status.code(), Some(0), "an operator interrupt is a clean stop:\n{err}");
    // The worker above sleeps 30s. The loop can only come back this fast if the signal path
    // actually SIGKILLed the worker's process GROUP — without that kill we'd simply wait the
    // sleep out and still exit 0, so every other assertion here would pass on a broken kill.
    assert!(
        took < std::time::Duration::from_secs(15),
        "interrupt took {took:?} — the worker group was not killed, we waited out its sleep:\n{err}"
    );

    let trace = fs::read_to_string(&trace_path).unwrap();
    let seq: Vec<&str> = trace.lines().collect();
    assert_eq!(
        seq,
        ["VERIFY=verify", "INJECT=inject", "RUN=run"],
        "an interrupted session must NOT be staged, judged or gated:\n{trace}\n{err}"
    );
    assert!(err.contains("interrupted (SIGINT/SIGTERM)"), "should report the interrupt:\n{err}");
    assert!(
        !err.contains("exited (code"),
        "an interrupted session must not log a normal session-exit line:\n{err}"
    );
    assert!(!dir.join("agg/state/run.pid").exists(), "the Drop guard must clear run.pid");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// NEW-behaviour integration tests (§4.1 / §5.3 / §5.4 / §5.7)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// §5.4: a sequence that references a step not defined in `steps:` is a HARD ERROR at STARTUP —
/// caught in the foreground (before any session), listing what IS defined. Never a runtime surprise.
#[test]
fn an_unknown_step_in_the_sequence_is_a_startup_error() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "worked", "#!/bin/sh\necho '{\"met\":true}'\n");
    // `ghost` is named in the sequence but is not a key in `steps:`.
    write(
        dir,
        "agg/agg.yaml",
        "project: ghosts\ndefaults: { model: fake }\nsteps:\n  worker: {}\n\
         sequence:\n  steps: [worker, ghost]\n  done_if: \"worked\"\nsummary: { enabled: false }\n",
    );
    write(dir, "agg/AGG_STATE.md", "do work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    assert!(!out.status.success(), "an unknown step must refuse to start");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown step `ghost`") && err.contains("worker"),
        "the error must name the miss and list the defined steps, got:\n{err}"
    );
}

/// §5.7: a sequence of ONLY `skip_judges` steps never merges, so `done_if` can never fire — this is
/// refused at startup rather than spinning forever staging work nothing ever gates.
#[test]
fn a_sequence_of_only_skip_judges_is_refused_at_startup() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "worked", "#!/bin/sh\necho '{\"met\":true}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: allskip\ndefaults: { model: fake }\nsteps:\n  stage: { skip_judges: true }\n\
         sequence:\n  steps: [stage]\n  done_if: \"worked\"\nsummary: { enabled: false }\n",
    );
    write(dir, "agg/AGG_STATE.md", "do work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    assert!(!out.status.success(), "an all-skip_judges sequence must refuse to start");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("skip_judges") && err.contains("judged step"),
        "the error must explain that at least one judged step is required, got:\n{err}"
    );
}

/// §5.7: a `skip_judges` step STAGES its work (nothing merges); the NEXT judged step gates the whole
/// span — pass ⇒ the entire span merges. Proof: the skip step's commit AND the judged step's commit
/// both land on `main` together, cut from the span tip, not from base.
#[test]
fn a_skip_judges_span_is_gated_and_merged_by_the_next_judged_step() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // fake claude: append `work-<n>` to tracked.txt where <n> is a per-session counter, and COMMIT
    // it on the session branch. Session 1 (stage, skipped) writes work-0; session 2 (worker, judged)
    // branches off the staged span tip → reads counter=1 → writes work-1, then the judge meets.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
n=$(cat .counter 2>/dev/null || echo 0)
printf 'work-%s\n' "$n" >> tracked.txt
echo $((n + 1)) > .counter
git add -A >/dev/null 2>&1
git commit -qm "worker work-$n" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.0}'
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
    // feature: met once `work-1` (the SECOND session's line) is present — so the span must run the
    // stage step then the judged step before it can pass.
    write_judge(dir, "feature", "#!/bin/sh\ngrep -q 'work-1' tracked.txt && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"two rounds done\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // stage (skip_judges) → worker (judged), in that order.
    write(
        dir,
        "agg/agg.yaml",
        "project: span\ndefaults: { model: fake }\nsteps:\n  stage: { skip_judges: true }\n  worker: {}\n\
         sequence:\n  steps: [stage, worker]\n  done_if: \"feature\"\nsummary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write(dir, "agg/AGG_STATE.md", "do work\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "4"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 0, &combined);
    assert!(combined.contains("done_if satisfied"), "the judged step gates the span to done:\n{combined}");

    // the WHOLE span merged: BOTH the skipped step's commit (work-0) and the judged step's (work-1)
    // are now on base. If the span logic branched the judged step off `base` instead of the span
    // tip, work-0 would be missing.
    let on_main = std::process::Command::new("git").args(["show", "main:tracked.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(
        content.contains("work-0") && content.contains("work-1"),
        "the skip_judges step's staged work must merge together with the judged step's, got: {content:?}"
    );
}

/// §5.3 end-to-end: the aggregates range over the DoD-set, NOT the run-set. A judge named only in an
/// `if` condition (`stalled`) is in the run-set but not the DoD-set, so `done_if: all_goals` can fire
/// while `stalled` is unmet. If aggregates ranged over the run-set, the run would never finish (the
/// success condition would become "we got stuck") and hit the session cap (exit 4) instead.
#[test]
fn done_if_all_goals_ignores_an_if_condition_judge() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // feature (the DoD judge, via `invariants` → it is in the DoD-set): met once did_work exists.
    write_judge(dir, "feature", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"done\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // stalled (a run-set-only judge, named ONLY in the `if` condition): NEVER met.
    write_judge(dir, "stalled", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"still churning\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: dodset\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n  reconsider: { skip_judges: true }\n\
         sequence:\n  steps:\n    - worker\n    - if stalled then reconsider\n  \
         done_if: \"all_goals\"\n  invariants: [feature]\nsummary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write(dir, "agg/AGG_STATE.md", "create the file did_work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // exit 0 — NOT 4. all_goals fired over the DoD-set {feature} despite stalled being unmet.
    assert_exit(&out, 0, &combined);
    assert!(
        combined.contains("done_if satisfied"),
        "all_goals must fire over the DoD-set even while the if-condition judge `stalled` is unmet:\n{combined}"
    );
}
