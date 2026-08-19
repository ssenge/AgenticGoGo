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

/// Write the worker's forward-advice file. It is WORKER-writable, so it stays in `agg/state/` under
/// the private-split — but it resolves through `agg::paths` so the next layout move lands in this
/// one helper instead of the ~15 call sites that used to spell the path out.
fn write_state_md(dir: &Path, content: &str) -> std::path::PathBuf {
    let p = agg::paths::agg_dir(dir).join("STATE.md");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    p
}

/// Write `agg/agg.yaml` (via [`cfg`]) + the forward state file (via [`write_state_md`]).
fn write_cfg(dir: &Path, project: &str, done_if: &str, seq_extra: &str, top_extra: &str) {
    write(dir, "agg/agg.yaml", &cfg(project, done_if, seq_extra, top_extra));
    write_state_md(dir, "do work\n");
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
    // init scaffolds agg.yaml + the committed AGG.md + the gitignored forward-state STATE.md + a
    // starter judge. No goals.yaml, no AGG_RESUME.md (§7.1).
    assert!(dir.join("agg/agg.yaml").exists(), "init should scaffold agg.yaml");
    assert!(dir.join("agg/AGG.md").exists(), "init should scaffold the committed AGG.md");
    assert!(agg::paths::agg_dir(dir).join("STATE.md").exists(), "init should scaffold the forward state file");
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

/// THE SPLIT, pinned end-to-end. `src/paths.rs` asserts the classification as a table and
/// `isolation` proves the kernel denies the writes; neither proves the RUNNING binary puts each
/// file on the side it claims. A real project driven by the real `agg` must end up with the gate
/// ledger under the AGG-OWNED `agg/private/` and the worker's forward advice under the
/// worker-writable `agg/state/` — and with BOTH roots ignored by git, since runtime churn in
/// either one would pollute the user's history (and `agg/private/` is the newer entry, so an
/// existing project's `.gitignore` has to gain it).
#[test]
fn runtime_state_splits_into_state_and_private_and_both_are_gitignored() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "did", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"ok\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "splitproj", "did", "", "");

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 0, &combined); // the fake worker commits did_work → the DoD is met.

    // ── 1) the gate ledger is AGG-OWNED. A confined worker can write everything in its cwd EXCEPT
    //    `private/`, so a ledger left in `state/` would let it forge the rows `stalled` reads.
    let ledger = agg::paths::verdicts_jsonl(dir);
    assert!(ledger.starts_with(dir.join("agg/private")), "the ledger must live in the carve-out: {}", ledger.display());
    assert!(!fs::read_to_string(&ledger).unwrap_or_default().trim().is_empty(), "the run must have written verdict rows:\n{combined}");
    assert!(!dir.join("agg/state/verdicts.jsonl").exists(), "no ledger may be left in the worker-writable root");

    // ── 2) the worker's own files stay worker-writable, or a confined worker cannot do its job.
    let state_md = agg::paths::agg_dir(dir).join("STATE.md");
    assert!(state_md.starts_with(dir.join("agg/state")), "STATE.md must stay worker-writable: {}", state_md.display());
    assert!(state_md.exists(), "the forward-advice file the brief points at must be there");

    // ── 3) both roots gitignored. Asked of GIT, not of the file's text, so any spelling that
    //    actually works counts — and it is the property the user cares about (a clean `git status`).
    for p in [&ledger, &state_md] {
        let ignored = std::process::Command::new("git")
            .args(["check-ignore", "-q", &p.strip_prefix(dir).unwrap().to_string_lossy()])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(ignored.success(), "{} must be gitignored — runtime state must never enter the user's history", p.display());
    }
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
    write_state_md(dir, "create the file did_work\n");

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
    write_state_md(dir, "create the file did_work\n");

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
    let snap = fs::read_to_string(agg::paths::state_json(dir)).expect("state.json published");
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

/// The dry-run commands inherit the `notify_if` validation for free, because `doctor` and `plan`
/// both go through `assemble()` (STUCK_NOTIFY §12.1). Proving it on `doctor` is enough — a second
/// test on `plan` would assert the same call, not a second code path.
///
/// The setup is otherwise perfect (fake agent on PATH, state file present, judge resolves), so
/// EXACTLY ONE check may fail: a typo'd detector name must be caught before an overnight run, not
/// mid-loop, and it must be attributed to the sequence check rather than to some incidental gap.
#[test]
fn doctor_inherits_the_notify_if_validation() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "worked", "#!/bin/sh\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"ok\"}'\n");
    write_cfg(
        dir,
        "docnotify",
        "worked",
        "  notify_if: \"ghost_detector\"\n  notify: { cmd: [\"true\"] }\n",
        "",
    );

    let out = agg(dir, &path).arg("doctor").output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "a notify_if naming an unresolvable judge must fail doctor:\n{combined}");
    assert!(
        combined.contains("✗ sequence + judges resolve"),
        "the failure must be attributed to the sequence/judge check, got:\n{combined}"
    );
    assert!(combined.contains("ghost_detector"), "doctor must NAME the unresolvable detector:\n{combined}");
    assert!(
        combined.contains("1 check(s) failed"),
        "only the notify_if check may fail — anything else means the fixture, not the feature, is broken:\n{combined}"
    );
}

#[test]
fn judge_runs_one_name_and_prints_raw_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = std::env::var("PATH").unwrap_or_default(); // no worker → real PATH is fine
    // a judge is a FILE resolved by NAME now (§5.1) — no goals.yaml, no inline cmd.
    write_judge(dir, "ok", "#!/bin/sh\necho '{\"met\":true,\"rationale\":\"fine\"}'\n");
    write(dir, "agg/agg.yaml", "project: jt\nsteps: { worker: {} }\nsequence: { steps: [worker], done_if: ok }\n");
    write_state_md(dir, "noop\n");

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
    // limits.cost: 0 → any spend (the stub's $0.05) is over budget.
    write_cfg(dir, "itest", "impossible", "  abort_if: \"over_cost\"\n  limits: { cost: 0 }\n", "");

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
    write_cfg(dir, "jsonproj", "worked", "  limits: { cost: 5.0 }\n", "");
    write_state_md(dir, "create the file did_work\n");

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
    // LOG.md from mechanical facts — the worker is never trusted to persist.
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // a judge that never meets, so the loop runs the full max_sessions and folds memory each time.
    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "memproj", "impossible", "", "memory: { enabled: true, max_kb: 64, inject_kb: 8 }\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // DoD is `impossible` → reaches the session cap unmet.

    // the durable memory file must exist under agg/private/ (AGG-OWNED audit trail), with a folded mechanical entry.
    let mem = agg::core::memory::memory_file(dir);
    assert!(mem.exists(), "LOG.md must be written even when the worker writes no note");
    let text = fs::read_to_string(&mem).unwrap();
    assert!(text.contains("## session 1"), "session 1 folded into memory, got:\n{text}");
    assert!(text.contains("exited cleanly") || text.contains("Goals:"), "mechanical facts recorded:\n{text}");
    // the loop logs the fold.
    assert!(combined.contains("[memory] session #1 folded"), "fold should be logged:\n{combined}");
}

#[test]
fn worker_gets_a_pointer_and_agg_writes_the_full_brief_to_instructions_md() {
    // §2/§3: the worker's `-p` is a tiny FIXED pointer; the whole brief is composed into
    // agg/private/INSTRUCTIONS.md (regenerated every session), which the worker reads. This kills the
    // argv size ceiling + the dash-safe fragility, and points at STATE.md rather than inlining it.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // a stub that captures its `-p` value into pval.txt, then commits a marker + emits a clean result.
    let claude = bin.join("claude");
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
prev=""
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
  if [ "$prev" = "-p" ]; then printf '%s' "$a" > pval.txt; fi
  prev="$a"
done
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    );
    chmod_x(&claude);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    write_judge(dir, "did", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"ok\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "ptrproj", "did", "", "");
    // a recognisable STATE body, to prove agg POINTS at it (§3 refinement 1) rather than inlining it.
    write_state_md(dir, "STATE-BODY-MARKER: fix the widget\n");
    // a committed AGG.md so the brief also points at the standing project instructions.
    write(dir, "agg/AGG.md", "# Project\nThe widget factory.\n");
    git_init(dir);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 0, &combined);

    // 1) the worker's `-p` was the tiny pointer — never the full brief, never the STATE body.
    let pval = fs::read_to_string(dir.join("pval.txt")).expect("fake worker captured its -p");
    // the pointer must name the file agg ACTUALLY wrote, so derive the relative path from `paths`
    // rather than restating it — a pointer that drifts from the writer is a worker with no brief.
    let brief_rel = agg::paths::instructions_md(Path::new("")).to_string_lossy().into_owned();
    assert!(pval.contains(&brief_rel), "the -p must be the {brief_rel} pointer, got: {pval}");
    assert!(!pval.contains("STATE-BODY-MARKER"), "the STATE body must NOT be inlined into -p: {pval}");
    assert!(pval.len() < 200, "the -p must stay tiny (no argv ceiling), got {} bytes", pval.len());

    // 2) agg composed the FULL brief into INSTRUCTIONS.md, pointing at STATE.md (not inlining it).
    let instr = fs::read_to_string(agg::paths::instructions_md(dir)).expect("agg wrote INSTRUCTIONS.md");
    assert!(instr.contains("# Session 1"), "brief carries the session header:\n{instr}");
    assert!(instr.contains("agg/state/STATE.md"), "brief POINTS at STATE.md:\n{instr}");
    assert!(instr.contains("agg/AGG.md"), "brief POINTS at the standing AGG.md:\n{instr}");
    assert!(instr.contains("agg/state/wiki/"), "footer names the wiki (multi-session plans live there):\n{instr}");
    // the wiki guidance must be SELF-CONTAINED: the OKF rules + a concrete template to copy, so a
    // worker that has never heard of OKF can still produce a linked, graph-able wiki.
    assert!(instr.contains("OKF"), "footer names the OKF format:\n{instr}");
    assert!(instr.contains("type: decision"), "footer ships a concrete OKF page template to copy:\n{instr}");
    assert!(instr.contains("[dead-ends](dead-ends.md)"), "template shows a standard markdown cross-link:\n{instr}");
    assert!(instr.contains("Before you exit"), "brief carries the standing footer:\n{instr}");
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
        serde_json::from_str(&fs::read_to_string(agg::paths::state_json(dir)).unwrap()).unwrap();
    let lifetime = state["lifetime_session"].as_u64().unwrap_or(0);
    assert!(lifetime >= 2, "lifetime_session must be published (was the publish! bug); got {lifetime}\nstate: {state}");
}

#[test]
fn worker_written_memory_note_is_folded() {
    // Tier 3a: when the worker writes agg/state/sessions/session-<N>.md on a clean session, agg folds
    // that note (preferred over the mechanical fallback) into the durable LOG.md.
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
mkdir -p agg/state/sessions
printf 'GOTCHA: the frobnicator needs a warm cache before the second pass\n' > agg/state/sessions/session-1.md
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

    let text = fs::read_to_string(agg::core::memory::memory_file(dir)).unwrap();
    assert!(text.contains("GOTCHA: the frobnicator"), "worker note folded into memory, got:\n{text}");
    // the worker note is appended as a fenced, lower-trust hint after the mechanical fact —
    // never standing alone — so the fold source is 'mechanical+worker'.
    assert!(combined.contains("folded (mechanical+worker)"), "fold source should be 'mechanical+worker':\n{combined}");
    assert!(text.contains("UNTRUSTED hint"), "worker note flagged as untrusted hint:\n{text}");
    // exactly ONE entry for session 1 (the early floor was superseded, not double-folded).
    assert_eq!(text.matches("## session 1 (").count(), 1, "single entry per session, got:\n{text}");
    // the scratch note is cleaned up after folding.
    assert!(!agg::core::memory::scratch_path(dir, 1).exists(), "scratch note deleted after fold");
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
    // stalled (run-set-only, named ONLY in the `until:` condition → NOT in the DoD-set): met at baseline
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
         steps:\n  worker: {}\n\
         sequence:\n  steps:\n    - { step: worker, until: stalled, max: 4 }\n  \
         done_if: \"feature\"\nsummary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");
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

/// The four deterministic outer-loop stages must be OBSERVABLE, in order, in `agg/private/state.json`
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
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' agg/private/state.json)" >> trace.txt
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
    write_state_md(dir, "create the file did_work\n");
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
    let state = fs::read_to_string(agg::paths::state_json(dir)).unwrap();
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
    write_state_md(dir, "noop\n");
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
    write_state_md(dir, "work\n");
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
    // ⚠ 5, not 0. An operator interrupt is a CLEAN stop but it is NOT a met goal, and it used to
    // share exit 0 with `GoalsMet` — so `if agg run; then ship; fi` shipped on Ctrl-C. The codes are
    // 0 = goals met · 3 = abort_if · 4 = max-sessions · 5 = stopped (`RunOutcome::exit_code`).
    assert_eq!(status.code(), Some(5), "an interrupt is a STOP, distinguishable from success:\n{err}");
    // The worker above sleeps 30s. The loop can only come back this fast if the signal path
    // actually SIGKILLed the worker's process GROUP — without that kill we'd simply wait the
    // sleep out and still exit cleanly, so every other assertion here would pass on a broken kill.
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
    assert!(!agg::paths::run_pid(dir).exists(), "the Drop guard must clear run.pid");
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
    write_state_md(dir, "do work\n");

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
    write_state_md(dir, "do work\n");

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
    write_state_md(dir, "do work\n");
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
/// `until:` condition (`stalled`) is in the run-set but not the DoD-set, so `done_if: all_goals` can
/// fire while `stalled` is unmet. If aggregates ranged over the run-set, the run would never finish
/// (the success condition would become "we got stuck") and hit the session cap (exit 4) instead.
#[test]
fn done_if_all_goals_ignores_an_until_condition_judge() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // feature (the DoD judge, via `invariants` → it is in the DoD-set): met once did_work exists.
    write_judge(dir, "feature", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"done\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // stalled (a run-set-only judge, named ONLY in the `until:` condition): NEVER met.
    write_judge(dir, "stalled", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"still churning\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: dodset\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n\
         sequence:\n  steps:\n    - { step: worker, until: stalled, max: 3 }\n  \
         done_if: \"all_goals\"\n  invariants: [feature]\nsummary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "create the file did_work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // exit 0 — NOT 4. all_goals fired over the DoD-set {feature} despite stalled being unmet.
    assert_exit(&out, 0, &combined);
    assert!(
        combined.contains("done_if satisfied"),
        "all_goals must fire over the DoD-set even while the until-condition judge `stalled` is unmet:\n{combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// BACKFILL — runtime regressions that executed with no checked-in test (audit gap). Each drives
// the real loop through the fake-claude shim and would go RED if the behaviour regressed.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// #1 — JUDGE-TOKEN BUDGETING (a money path, previously untested end-to-end). The LLM judge's OWN
/// output tokens count against `limits.tokens` (§5.6). The worker reports 5 tok/session — 100 over
/// the whole 20-session cap, well UNDER the 1000 ceiling — so worker spend ALONE can never trip
/// `over_budget`. The `.md` judge reports 600 tok per run; once worker+judge crosses 1000, the guard
/// must ABORT (exit 3). If judge spend were uncounted, only the worker's ~100 tok would accrue and the
/// run would sail to the session cap (exit 4). That exit-3-vs-4 split is the whole test.
#[test]
fn judge_tokens_count_toward_the_token_ceiling() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // ONE binary, BOTH roles: `--output-format json` is the ruler one-shot (the LLM judge), which
    // reports 600 output_tokens on a `result`-shaped envelope so `tally_one_shot` sums them (§5.6) and
    // whose verdict (in `.result`) is NEVER met; `stream-json` is the worker, reporting 5 tok.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
fmt=""; prev=""
for a in "$@"; do [ "$prev" = "--output-format" ] && fmt="$a"; prev="$a"; done
if [ "$fmt" = "json" ]; then
  printf '{"type":"result","result":"{\\"met\\":false,\\"value\\":0,\\"max\\":1,\\"target\\":1,\\"rationale\\":\\"never\\"}","usage":{"output_tokens":600},"total_cost_usd":0}\n'
  exit 0
fi
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":5},"total_cost_usd":0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    // the `.md` judge IS the DoD (an LLM judge), never met — so ONLY over_budget can end the loop.
    write(dir, "agg/judges/reviewed.md", "---\ninputs: []\n---\nDecide if the work is done. Output the verdict JSON on the last line.\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: judgetok\ndefaults: { model: fake }\njudge: { agent: claude, model: fake }\n\
         steps:\n  worker: {}\n\
         sequence:\n  steps: [worker]\n  done_if: \"reviewed\"\n  abort_if: \"over_budget\"\n  limits: { tokens: 1000 }\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");
    git_init(dir);

    let out = agg(dir, &path).args(["run", "--max-sessions", "20"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // over_budget aborts (exit 3) — NOT the session cap (exit 4). The ONLY way tokens reach 1000 is by
    // counting the judge's 600/run, so this asserts exactly the money path.
    assert_exit(&out, 3, &combined);
    assert!(
        combined.contains("ABORT") && combined.contains("over_budget"),
        "the judge's own tokens must push worker+judge over limits.tokens and trip over_budget:\n{combined}"
    );
    assert!(
        !combined.contains("reached max_sessions"),
        "over_budget (judge tokens counted), not the session cap, must end the run:\n{combined}"
    );
}

/// #2 — CEILINGS ARE CHECKED AFTER A `skip_judges` STEP TOO. A skip step runs NO judges, but the
/// run-level ceilings must still be evaluated after it — else a run of skip steps sails past
/// `limits.sessions`/`limits.tokens`. Sequence `[worker, stage]`: `stage` (skip_judges) is session 2,
/// which is exactly when `over_iterations` (limits.sessions: 2) first trips, so the abort (exit 3)
/// lands ON the skip step. If the skip path stopped checking ceilings, the loop would slip to the
/// loop's own exit-4 session-cap precheck instead.
#[test]
fn ceilings_are_checked_after_a_skip_judges_step_too() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: skipceil\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n  stage: { skip_judges: true }\n\
         sequence:\n  steps: [worker, stage]\n  done_if: \"feature\"\n  abort_if: \"over_iterations\"\n  limits: { sessions: 2 }\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");

    // No --max-sessions flag on purpose: the ceiling under test is `limits.sessions`, and a non-zero
    // flag would OVERRIDE it (§4.1). limits.sessions=2 ALSO backs the loop's own exit-4 precheck, so
    // if the skip-path ceiling check regressed the run still terminates (exit 4) rather than hanging.
    let out = agg(dir, &path).arg("run").output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 3, &combined);
    assert!(combined.contains("over_iterations"), "the ceiling must abort ON the skip step (exit 3):\n{combined}");
    assert!(
        !combined.contains("reached max_sessions"),
        "over_iterations (checked after the skip step), not the exit-4 cap, must end the run:\n{combined}"
    );
}

/// #3 — a CRASHING judge with `abort_if: any_judge_error` ABORTS the run (exit 3). Distinct from a
/// regression: a crashed judge's `Verdict::failed` never changes lifecycle (model.rs), so
/// `any_regressed` stays false — only `any_judge_error` surfaces it. (The companion below proves the
/// negative half: a clean not-met does NOT trip it.)
#[test]
fn a_crashing_judge_aborts_the_run_via_any_judge_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: makes the invariant judge start CRASHING (drops `.flake`), then commits clean work.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
printf 'work\n' >> tracked.txt
touch .flake
git add -A >/dev/null 2>&1
git commit -qm "worker change" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let g = |args: &[&str]| { Command::new("git").args(args).current_dir(dir).output().unwrap(); };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    write(dir, "tracked.txt", "ok\n");
    // build_ok (invariant): met at baseline; once `.flake` exists it CRASHES (exit non-zero, no JSON →
    // Verdict::failed, error set), NOT a clean not-met.
    write_judge(dir, "build_ok", "#!/bin/sh\nif [ -f .flake ]; then echo 'judge crashed' >&2; exit 1; fi\necho '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"build ok\"}'\n");
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "crashjudge", "feature", "  invariants: [build_ok]\n  abort_if: \"any_judge_error\"\n", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 3, &combined);
    assert!(
        combined.contains("ABORT") && combined.contains("any_judge_error"),
        "a crashing judge under `abort_if: any_judge_error` must abort (exit 3):\n{combined}"
    );
}

/// #3 companion — the DISCRIMINATOR for the test above. A judge that runs cleanly and returns
/// `met:false` (NO error) must NOT populate `any_judge_error`. With `abort_if: any_judge_error` and a
/// DoD that is simply never met, the run reaches the session cap (exit 4), never aborting (exit 3).
#[test]
fn a_clean_not_met_judge_does_not_trip_any_judge_error() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"cleanly not yet\"}'\n");
    write_cfg(dir, "cleanjudge", "feature", "  abort_if: \"any_judge_error\"\n", "memory: { enabled: false }\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined);
    assert!(!combined.contains("ABORT"), "a cleanly not-met judge must NOT trip any_judge_error:\n{combined}");
}

/// Build a two-step project (`plan` then `build`) whose fake worker APPENDS its full argv to
/// `sessions.txt` (one block per session, separated by `=== SESSION ===`), never meets the DoD, runs
/// exactly two sessions (plan then build) and exits 4 at the cap. Returns the two recorded argv blocks
/// `(plan_argv, build_argv)`. The worker echoing its own args is the only honest observation channel
/// for per-step overrides — agg logs none of `--model`/`--effort`/`worker_args`. `defaults_body` /
/// `build_body` are the YAML flow-mapping bodies for `defaults:` and the `build` step.
fn two_step_argv(defaults_body: &str, build_body: &str) -> (String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
{ echo "=== SESSION ==="; for a in "$@"; do printf '%s\n' "$a"; done; } >> sessions.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    write_judge(dir, "nope", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        &format!(
            "project: argv\ndefaults: {defaults_body}\n\
             steps:\n  plan: {{}}\n  build: {build_body}\n\
             sequence:\n  steps: [plan, build]\n  done_if: \"nope\"\n\
             summary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"
        ),
    );
    write_state_md(dir, "do work\n");
    git_init(dir);
    let out = agg(dir, &path).args(["run", "--max-sessions", "2"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // never-met DoD → the 2-session cap ends it (plan, then build)
    let sessions = fs::read_to_string(dir.join("sessions.txt")).expect("the worker should have recorded its argv");
    let blocks: Vec<String> = sessions.split("=== SESSION ===").skip(1).map(|b| b.trim().to_string()).collect();
    assert_eq!(blocks.len(), 2, "exactly two sessions (plan, build) must have run:\n{sessions}");
    (blocks[0].clone(), blocks[1].clone())
}

/// The value following `flag` in a newline-separated argv block (the worker records one arg per line).
fn arg_value(argv: &str, flag: &str) -> Option<String> {
    let lines: Vec<&str> = argv.lines().collect();
    lines.iter().position(|l| *l == flag).and_then(|i| lines.get(i + 1)).map(|s| s.to_string())
}

/// #4 — a per-step MODEL override reaches the worker's `--model` argv. `build` overrides
/// `defaults.model`; the two sessions must carry DIFFERENT `--model` values. If the per-step override
/// regressed (build fell back to defaults.model), `model-bravo` would never reach argv.
#[test]
fn per_step_model_override_reaches_the_worker_argv() {
    let (plan, build) = two_step_argv("{ model: model-alpha }", "{ model: model-bravo }");
    assert_eq!(arg_value(&plan, "--model").as_deref(), Some("model-alpha"), "plan uses the default model:\n{plan}");
    assert_eq!(
        arg_value(&build, "--model").as_deref(),
        Some("model-bravo"),
        "build's per-step model override must reach --model for THAT step's worker:\n{build}"
    );
}

/// #5 — a per-step EFFORT OVERRIDE (not merely the inherited default) reaches `--effort`. `plan`
/// inherits `defaults.effort: low`; `build` overrides to `high`. If the per-step override regressed,
/// build would inherit `low` and `high` would never reach argv.
#[test]
fn per_step_effort_override_reaches_the_worker_argv() {
    let (plan, build) = two_step_argv("{ model: fake, effort: low }", "{ effort: high }");
    assert_eq!(arg_value(&plan, "--effort").as_deref(), Some("low"), "plan inherits defaults.effort:\n{plan}");
    assert_eq!(
        arg_value(&build, "--effort").as_deref(),
        Some("high"),
        "build's per-step effort override must reach --effort:\n{build}"
    );
}

/// #6 — an EMPTIED `worker_args: []` in a step overrides the inherited defaults list. `plan` inherits
/// the sentinel flag; `build` empties the list and must carry NONE of it. If `Some([])` were treated
/// like `None` (unset → inherit), build would carry the sentinel too.
#[test]
fn an_emptied_worker_args_list_overrides_the_inherited_defaults() {
    let (plan, build) = two_step_argv(
        "{ model: fake, worker_args: [\"--SENTINEL-ARG\", \"sentinel-val\"] }",
        "{ worker_args: [] }",
    );
    assert!(plan.contains("--SENTINEL-ARG"), "plan inherits defaults.worker_args:\n{plan}");
    assert!(
        !build.contains("--SENTINEL-ARG"),
        "an emptied worker_args: [] must DROP the inherited flag, not inherit it:\n{build}"
    );
}

/// #7 — a non-empty `inputs:` in an `.md` judge's frontmatter is resolved and its content REACHES the
/// judge prompt. `evidence.txt` carries a sentinel; the judge declares `inputs: [evidence.txt]`; the
/// ruler one-shot records the prompt it was handed. Both the sentinel and the input's label must be in
/// it. If frontmatter parsing regressed to empty, the judge would see "(no inputs specified)".
#[test]
fn an_md_judge_resolves_its_frontmatter_inputs_into_the_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // ONE binary: the `--output-format json` one-shot records the judge prompt and reports a verdict
    // (met once `did_work` exists); the stream-json worker creates + commits `did_work`.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
fmt=""; prompt=""; prev=""
for a in "$@"; do
  [ "$prev" = "--output-format" ] && fmt="$a"
  [ "$prev" = "-p" ] && prompt="$a"
  prev="$a"
done
if [ "$fmt" = "json" ]; then
  printf '%s' "$prompt" > judge_prompt.txt
  if [ -f did_work ]; then
    printf '{"result":"{\\"met\\":true,\\"value\\":1,\\"max\\":1,\\"target\\":1,\\"rationale\\":\\"ok\\"}"}\n'
  else
    printf '{"result":"{\\"met\\":false,\\"value\\":0,\\"max\\":1,\\"target\\":1,\\"rationale\\":\\"not yet\\"}"}\n'
  fi
  exit 0
fi
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    write(dir, "evidence.txt", "INPUT_SENTINEL_ABC\n");
    write(dir, "agg/judges/reviewed.md", "---\ninputs: [evidence.txt]\n---\nApply the rubric. Output the verdict JSON on the last line.\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: mdinputs\ndefaults: { model: fake }\njudge: { agent: claude, model: fake }\n\
         steps:\n  worker: {}\n\
         sequence:\n  steps: [worker]\n  done_if: \"reviewed\"\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "create the file did_work\n");
    git_init(dir);

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the llm-judged loop should reach done:\n{combined}");
    let jp = fs::read_to_string(dir.join("judge_prompt.txt")).expect("the ruler one-shot should have recorded its prompt");
    assert!(jp.contains("INPUT_SENTINEL_ABC"), "the declared input's CONTENT must reach the judge prompt:\n{jp}");
    assert!(jp.contains("evidence.txt"), "…under its input label:\n{jp}");
}

/// #8 — a run-set-only `until:`-condition judge is EVALUATED and its verdict ends the repetition
/// (the execution half; the DoD-EXCLUSION half is already covered by `done_if_all_goals_ignores_…`).
/// `stalled` is named ONLY in the `until:` of the `worker` entry, so it is in the run-set but not the
/// DoD-set. It must actually RUN each judged step (a filesystem side-effect proves it) AND, once met,
/// end `worker`'s repetition early so the walk reaches `reconsider` (whose per-step prompt marker then
/// appears) INSIDE the 3-session cap — `max: 3` alone would spend the whole cap on `worker`. If
/// `until:` judges were left out of the run-set, `stalled` would never run and the walk would never
/// converge.
#[test]
fn a_run_set_only_until_condition_judge_is_evaluated_and_ends_its_repetition() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // the worker records the brief it actually reads (agg/private/INSTRUCTIONS.md — the `-p` is now
    // just a tiny pointer at it) so the reconsider marker is observable, and on its first run COMMITS
    // `.signal` — which flips `stalled` to met on the next evaluation.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do [ "$prev" = "-p" ] && { cat agg/private/INSTRUCTIONS.md >> prompts.txt 2>/dev/null; printf '\n===8<===\n' >> prompts.txt; }; prev="$a"; done
if [ ! -f .signal ]; then : > .signal; git add .signal >/dev/null 2>&1; git commit -qm signal >/dev/null 2>&1; fi
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    // `stalled` leaves a side-effect each time it RUNS, and reports met once `.signal` is present.
    write_judge(dir, "stalled", "#!/bin/sh\necho STALLED_RAN >> stalled_ran.txt\n[ -f .signal ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"stalled\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    // `feature` is the DoD and is NEVER met → the loop runs to the cap, giving the branch time to fire.
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: ifcond\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n  reconsider: { skip_judges: true, prompt: \"RECONSIDER_MARKER_888\" }\n\
         sequence:\n  steps:\n    - { step: worker, until: stalled, max: 3 }\n    - reconsider\n  done_if: \"feature\"\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");
    git_init(dir);

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // feature never met → the session cap ends it
    // (a) `stalled` actually EXECUTED — the run-set really contains it.
    let ran = fs::read_to_string(dir.join("stalled_ran.txt")).expect("`stalled` should have run at least once");
    assert!(ran.contains("STALLED_RAN"), "the if-condition judge `stalled` must be EVALUATED (run):\n{ran}");
    // (b) its evaluated verdict ended the repetition — the walk moved on and reconsider fired,
    // injecting its per-step prompt.
    let prompts = fs::read_to_string(dir.join("prompts.txt")).unwrap_or_default();
    assert!(
        prompts.contains("RECONSIDER_MARKER_888"),
        "`until: stalled` must end `worker`'s repetition once stalled evaluates met:\n{combined}"
    );
}

/// FLAGSHIP CIRCUIT-BREAKER, end-to-end: the BUILTIN `stalled` judge — the one that ships inside the
/// binary and reads `agg/private/verdicts.jsonl` (met when, across the last K=3 MERGED steps, no binary
/// judge changed `met` and no numeric judge changed `value`) — must actually flip to met on a
/// no-progress run and end `worker`'s repetition so `reconsider` runs. Unlike `…ends_its_repetition`
/// above (which
/// FAKES `stalled` with a `.signal` file), this writes NO `agg/judges/stalled.sh`: it exercises the
/// real library judge resolved from `~/.agg/judges/` (installed by `ensure_library`), driven purely
/// by the verdict history the loop records.
///
/// Setup: `done_if: feature`, and `feature` is NEVER met and constant → flat across every merged
/// step. The fake worker COMMITS a unique no-op line each session (so worker steps MERGE — stalled
/// counts only merged rows) while touching nothing any judge reads, and records every `-p` prompt to
/// an UNTRACKED file (survives all branch churn). After 3 flat merged worker steps the builtin
/// `stalled` flips to met, so `until: stalled` ends the `worker` entry and the walk dispatches
/// `reconsider`, whose injected marker prompt reaches the worker — that marker's presence PROVES the
/// mechanism fired.
#[test]
fn the_builtin_stalled_judge_fires_reconsider_on_a_no_progress_run() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // The fake worker: (1) records the brief it actually reads (agg/private/INSTRUCTIONS.md — the `-p`
    // is now a tiny pointer at it) to prompts.txt — left UNTRACKED so it survives session branch
    // resets and accumulates the reconsider marker; (2) commits a UNIQUE no-op line per session to a
    // TRACKED file so the worker step MERGES (the builtin stalled counts only `merged` verdict rows)
    // while moving NO judge's met/value. `agg/private/` is auto-gitignored by agg, so the
    // verdicts.jsonl stalled reads is never committed or clobbered.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do [ "$prev" = "-p" ] && { cat agg/private/INSTRUCTIONS.md >> prompts.txt 2>/dev/null; printf '\n===8<===\n' >> prompts.txt; }; prev="$a"; done
n=$(cat noop.txt 2>/dev/null | wc -l | tr -d ' ')
printf 'noop %s\n' "$n" >> noop.txt
git add noop.txt >/dev/null 2>&1
git commit -qm "worker noop-$n" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0}\n'
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    // `feature` (the DoD) is NEVER met and CONSTANT — flat across every merged step, so the builtin
    // stalled sees no movement. It is the ONLY non-`stalled` judge in the run-set (stalled ignores
    // its own rows), so nothing masks the stall.
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    // NB: deliberately NO agg/judges/stalled.sh — `stalled` must resolve to the shipped library judge.
    write(
        dir,
        "agg/agg.yaml",
        "project: stallfire\ndefaults: { model: fake }\n\
         steps:\n  worker: {}\n  reconsider: { skip_judges: true, prompt: \"RECONSIDER_FIRED_MARKER\" }\n\
         sequence:\n  steps:\n    - { step: worker, until: stalled, max: 8 }\n    - reconsider\n  done_if: \"feature\"\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");
    git_init(dir);

    // feature never met + no abort_if → the ONLY terminator is the session cap (exit 4). 8 is ample:
    // stalled fires during session 4's verify (it then sees 3 flat merged steps), so reconsider is
    // dispatched at session 5.
    let out = agg(dir, &path).args(["run", "--max-sessions", "8"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined);

    // guard the SETUP: no project shadow may exist — the BUILTIN is the judge under test.
    assert!(
        !dir.join("agg/judges/stalled.sh").exists(),
        "this test must exercise the builtin stalled judge, not a project shadow"
    );
    // THE proof: the builtin stalled→reconsider circuit-breaker fired. reconsider's injected marker
    // reached the worker, which only happens if `until: stalled` ended the worker entry early.
    let prompts = fs::read_to_string(dir.join("prompts.txt")).unwrap_or_default();
    assert!(
        prompts.contains("RECONSIDER_FIRED_MARKER"),
        "the builtin `stalled` must flip to met after K=3 merged no-progress steps and end the \
         `worker` entry — reconsider's marker prompt never reached the worker:\n{combined}"
    );
    // …and the loop actually dispatched the reconsider STEP (the session banner names it).
    assert!(
        combined.contains("`reconsider`"),
        "the walk must dispatch the reconsider step, not merely evaluate the condition:\n{combined}"
    );
}

/// R5a (HOOK_STAGE_PLAN): a rate-limited worker session is INCOMPLETE — the loop backs off and goes
/// round again WITHOUT judging, staging/gating, or folding the post-judge memory refinement. This is
/// outcome-invisible (the run still reaches the cap and exits 4), so pin it here where a botched
/// verify/gate conversion would otherwise stay green while dropping the skip.
#[test]
fn a_rate_limited_session_skips_the_judged_gate_and_the_refine_fold() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // fake claude: report a rate-limit in the result line (matched by `looks_rate_limited`) and do
    // NO work — a rate-limited turn never reached the model.
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
# detection is exit-code AND terminal-event gated ("a clean exit 0 is never a rate-limit"), so the
# stub reports the rate-limit in its result AND exits non-zero.
printf '%s\n' '{"type":"result","subtype":"error","is_error":true,"result":"rate_limit_error: slow down","usage":{"output_tokens":0},"total_cost_usd":0}'
exit 1
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    // a judge that never meets (absent the rate-limit the loop would judge every session); backoff 0
    // so the test never sleeps; memory on so a would-be refine fold would show up in the log.
    write_judge(dir, "impossible", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"nope\"}'\n");
    write_cfg(dir, "rl", "impossible", "", "ratelimit_backoff_secs: 0\nmemory: { enabled: true, max_kb: 64, inject_kb: 8 }\n");
    git_init(dir);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // one rate-limited session, then the cap → MaxSessions.

    assert!(combined.contains("rate limit detected"), "the rate-limit path must be taken:\n{combined}");
    // the judged path (verify) is skipped: only the BASELINE runs judges ("running judges once before
    // the first session…"); the rate-limited session must NOT add a second judge run.
    assert_eq!(
        combined.matches("running judges").count(),
        1,
        "a rate-limited session must NOT judge (only the baseline should):\n{combined}"
    );
    // the post-judge memory REFINE fold (in the gate) is skipped for the incomplete session.
    assert!(
        !combined.contains("[memory] session #1 folded"),
        "a rate-limited session must NOT fold the post-judge refinement:\n{combined}"
    );
}

/// R2 (HOOK_STAGE_PLAN): the session that MEETS `done_if` is NOT special-cased out of session-end
/// work — it still fires the on_session_end hook and folds the post-judge memory refinement BEFORE
/// the run stops. (The run-stop check is the LAST on_session_end handler, not part of the gate; a
/// gate-placed stop would skip the winning session's fold + hook.) Outcome-invisible → pinned here.
#[test]
fn a_winning_session_still_folds_memory_and_fires_on_session_end() {
    let (tmp, path) = project_with_fake_claude(); // the worker creates + commits `did_work`
    let dir = tmp.path();
    write_judge(dir, "won", "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"done\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "win", "won", "", "memory: { enabled: true, max_kb: 64, inject_kb: 8 }\nhooks:\n  on_session_end: [\"touch session_end_ran\"]\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 0, &combined); // done_if fires on session 1 → GoalsMet.

    // the WINNING session still fired its on_session_end hook…
    assert!(dir.join("session_end_ran").exists(), "on_session_end must fire on the winning session:\n{combined}");
    // …and still folded session 1 into LOG.md (the fold precedes the run-stop decision).
    let mem = fs::read_to_string(agg::core::memory::memory_file(dir)).unwrap_or_default();
    assert!(mem.contains("## session 1"), "the winning session must still fold into LOG.md:\n{mem}\n{combined}");
    assert!(combined.contains("[memory] session #1 folded"), "the winning session's fold must be logged:\n{combined}");
}

#[test]
fn agg_auto_commits_a_worker_that_forgets_to_commit() {
    // GIT_REDESIGN §2: the worker EDITS a tracked file but never runs `git commit`. agg must
    // auto-commit the edit on the session branch (not discard it), so it merges to base and the goal
    // is met. Before GIT_REDESIGN, discard_uncommitted_tracked threw the edit away → never converged.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // fake worker: create answer.txt and DO NOT commit (no git at all).
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
printf 'made-it\n' > answer.txt
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
    write_judge(dir, "answered", "#!/bin/sh\n[ -f answer.txt ] && grep -q made-it answer.txt && echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"answered\"}' || echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "autocommit", "answered", "", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "3"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 0, &combined); // agg committed the edit → merged → answered met → done.
    // the worker never ran git, yet its edit is on main (agg auto-committed it).
    let on_main = std::process::Command::new("git").args(["show", "main:answer.txt"]).current_dir(dir).output().unwrap();
    let content = String::from_utf8_lossy(&on_main.stdout);
    assert!(content.contains("made-it"), "agg must auto-commit the worker's uncommitted edit onto main, got: {content:?}\n{combined}");
    assert!(combined.contains("agg committed the worker's edits"), "the auto-commit must be logged:\n{combined}");
}

#[test]
fn a_worker_tampered_judge_is_rolled_back_and_base_judge_restored() {
    // GIT_REDESIGN §6 (the single most important moat test): a session whose worker TAMPERS a
    // committed judge AND regresses a DoD-set invariant must ROLL BACK — and because judges are
    // committed and rollback restores base via `:(exclude)agg/{state,private}/**`, base's judge is the
    // ORIGINAL, not the worker's tampered copy. agg (not the worker) makes the commit now, so this
    // proves the moat still holds under GIT_REDESIGN's auto-commit.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // worker: regress build_ok (drop .regressed) AND tamper its judge script (append a marker), and
    // DO NOT commit — agg auto-commits. The tamper is a harmless trailing comment, so build_ok still
    // runs and still reports the regression (a self-hiding tamper is a deeper concern, out of scope).
    write(
        &bin,
        "claude",
        r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
touch .regressed
printf '\n# TAMPERED-BY-WORKER\n' >> agg/judges/build_ok.sh
exit 0
"#,
    );
    chmod_x(&bin.join("claude"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    let g = |args: &[&str]| { std::process::Command::new("git").args(args).current_dir(dir).output().unwrap(); };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    // build_ok (invariant): met at baseline, REGRESSES once .regressed exists.
    write_judge(dir, "build_ok", "#!/bin/sh\n[ -f .regressed ] && echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"regressed\"}' || echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"ok\"}'\n");
    // feature: never met (so the loop runs a session rather than stopping at baseline).
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"not yet\"}'\n");
    write_cfg(dir, "tamper", "feature", "  invariants: [build_ok]\n", "memory: { enabled: false }\n");
    g(&["add", "-A"]);
    g(&["commit", "-qm", "base"]);

    let out = agg(dir, &path).args(["run", "--max-sessions", "1"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 4, &combined); // feature never met → cap; the regressing session rolled back.
    assert!(combined.contains("ROLLED BACK"), "the judge-tampering, regressing session must roll back:\n{combined}");
    // THE MOAT: base's committed judge is restored to the ORIGINAL — the worker's tamper is gone.
    let judge_on_main = std::process::Command::new("git").args(["show", "main:agg/judges/build_ok.sh"]).current_dir(dir).output().unwrap();
    let judge = String::from_utf8_lossy(&judge_on_main.stdout);
    assert!(!judge.contains("TAMPERED"), "rollback must restore the committed judge — no worker tamper on base, got:\n{judge}");
    assert!(judge.contains(".regressed"), "base keeps the ORIGINAL judge logic");
    // base is pristine: the worker's .regressed marker is NOT tracked on base.
    let ls = std::process::Command::new("git").args(["ls-files", ".regressed"]).current_dir(dir).output().unwrap();
    assert!(String::from_utf8_lossy(&ls.stdout).trim().is_empty(), "the regressing session's marker must not be on base");
}

/// ⛔ `max` BOUNDS an `until`; it does not SATISFY it. An entry that spends its whole budget with the
/// condition still false has BROKEN A CONTRACT, and the run aborts naming the bound and the
/// condition — it does not advance to the next entry as though it had converged.
///
/// This is the engine-level half of `core::walk`'s unit test: it proves the walk's error survives the
/// handler pipeline and ends the process, rather than being swallowed on the way. Exit 3 (abort), not
/// 4 (max-sessions), is the whole assertion — the run stops at the BOUND, with sessions to spare.
///
/// The 2026-08-05 real run is why this exists: `survey` burned its `max: 3` against a judge that was
/// timing out, the walk moved on silently, and the worker went on to shrink the artefact by 61% so
/// the broken grader could finish inside its timeout.
#[test]
fn a_sequence_entry_that_exhausts_max_without_converging_aborts_the_run() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // never met, however many times it runs — so `until: worked` can never hold.
    write_judge(dir, "worked", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: bounded\n\
         defaults: { model: fake }\n\
         steps:\n  worker: {}\n\
         sequence:\n  steps: [{ step: worker, until: worked, max: 2 }]\n  done_if: \"worked\"\n\
         summary: { enabled: false }\n",
    );
    write_state_md(dir, "do work\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "9"]).output().unwrap();
    let combined =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_exit(&out, 3, &combined);
    assert!(combined.contains("exhausted `max: 2`"), "the abort names the bound:\n{combined}");
    assert!(combined.contains("until: worked"), "…and the condition that never held:\n{combined}");
}

/// A step whose whole OUTPUT is gitignored worker-state must still be able to satisfy its `until:`.
///
/// `agg/state/` is gitignored BY DESIGN — the OKF wiki and STATE.md are durable, multi-session and
/// first-class — so a `survey`/`spec`/`plan` step commits NOTHING every time and resolves as
/// `NoChanges`. The gate's `_` arm used to `restore_goal_state` on that, throwing away the verdict
/// the judge had just measured against the real filesystem and putting the pre-step value back.
///
/// The consequence was invisible until `max:` became a contract: a real `examples/workflow.yaml` run
/// had `spec` score 100/100, get restored to 75, re-dispatch, score 100/100 again, and then abort
/// with "exhausted `max: 2` without `until: spec_sound` ever holding" — about a condition that had
/// held twice. This asserts the loop CONVERGES instead: exit 0, not exit 3.
#[test]
fn a_step_that_commits_nothing_can_still_satisfy_its_until() {
    let (tmp, path) = project_with_fake_claude();
    let dir = tmp.path();
    // gitignore the worker-state dir, exactly as a real project does — so the worker's output is
    // real, durable, and invisible to git. agg's auto-commit then finds nothing: `NoChanges`.
    // `bin/` too, or agg's auto-commit sweeps the fake worker's own binary onto the session branch —
    // the session then resolves as `Staged`, not `NoChanges`, and the bug under test cannot occur.
    write(dir, ".gitignore", "agg/\nbin/\n.home/\n");
    Command::new("git").args(["add", ".gitignore"]).current_dir(dir).output().unwrap();
    Command::new("git").args(["commit", "-qm", "gitignore agg/"]).current_dir(dir).output().unwrap();

    // A worker that writes ONLY into the gitignored state dir, so its sessions commit nothing.
    //
    // ⚠ It writes a COUNTER, not a marker, and the judge needs TWO. That is not padding: agg appends
    // its own entries to `.gitignore` on the first run, so session #1 always has something to commit
    // and always resolves as `Staged`. A judge that flips on session #1 therefore converges through
    // the merge path and never touches the arm under test. The judge must become met on a LATER,
    // commit-free session — which is exactly the shape of the real failure, where `spec` scored
    // 100/100 on a `NoChanges` session.
    write(
        &dir.join("bin"),
        "claude",
        "#!/bin/sh\n\
         for a in \"$@\"; do if [ \"$a\" = \"--version\" ]; then echo 'fake 0.0.0'; exit 0; fi; done\n\
         mkdir -p agg/state/wiki && echo x >> agg/state/wiki/spec.md\n\
         printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"output_tokens\":1}}'\n",
    );
    chmod_x(&dir.join("bin/claude"));
    write_judge(
        dir,
        "spec_ok",
        "#!/bin/sh\nn=$(wc -l < agg/state/wiki/spec.md 2>/dev/null || echo 0)\n\
         if [ \"$n\" -ge 2 ]; then echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1}'; \
         else echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'; fi\n",
    );
    // never met, so `done_if` cannot end the run before the walk re-visits the entry — which is the
    // only moment `until:` is evaluated, and therefore the only moment the bug is observable.
    write_judge(dir, "never", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n");
    write(
        dir,
        "agg/agg.yaml",
        "project: statework\n\
         defaults: { model: fake }\n\
         steps:\n  worker: {}\n  second: {}\n\
         sequence:\n  steps: [{ step: worker, until: spec_ok, max: 3 }, { step: second }]\n  done_if: \"never\"\n\
         summary: { enabled: false }\n",
    );
    write_state_md(dir, "write the spec\n");

    let out = agg(dir, &path).args(["run", "--max-sessions", "4"]).output().unwrap();
    let combined =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(
        !combined.contains("exhausted `max: 3`"),
        "the judge measured the real filesystem and said MET — the bound must not be spent:\n{combined}"
    );
    assert!(
        combined.contains("step `second`"),
        "converging on the first entry must ADVANCE the walk, not re-dispatch it:\n{combined}"
    );
    assert_exit(&out, 4, &combined); // `never` never fires, so the session cap is the exit
}
