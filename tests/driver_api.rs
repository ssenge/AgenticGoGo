//! THE RUST DRIVER API's load-bearing properties (BUILD.md §5).
//!
//! These drive the REAL facade against a REAL project — a git repo, the real hook pipeline, real
//! session branches — with a fake `claude` on PATH standing in for the model, exactly as
//! `tests/cli.rs` does for the YAML path. Nothing here asserts on a flag the facade set; every
//! assertion reads something the pipeline actually produced (the session counter, a marker file a
//! judge script wrote, `state.json`).
//!
//! # Why one shared PATH/HOME, set once
//!
//! Tests inside one binary run in parallel threads, and mutating the process environment while
//! another thread is spawning is a race. So the environment is mutated EXACTLY ONCE, inside a
//! `LazyLock` that every test touches before it does anything else — every thread therefore
//! synchronises on that initialisation before any of them can spawn a worker.
//!
//! Unix-only (the fake agent is a shell script), like the rest of the suite.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use agg::core::config::Limits;
use agg::prelude::*;

/// The one environment mutation in this binary: a shared `bin/` holding the fake `claude`, and a
/// throwaway `HOME` so `ensure_library` never writes the developer's real `~/.agg/judges`.
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-driver-env-{}", std::process::id()));
    let bin = root.join("bin");
    let home = root.join("home");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // The fake `claude`. `--version` answers preflight. A WORKER session is recognised by the
    // instructions pointer in its prompt — a ruler one-shot (summary / LLM judge) must not be
    // mistaken for one, or every summary would commit a phantom session.
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        r#"#!/bin/sh
worker=
for a in "$@"; do
  case "$a" in
    --version) echo "fake-claude 0.0.0"; exit 0 ;;
    *INSTRUCTIONS.md*) worker=1 ;;
  esac
done
if [ -n "$worker" ]; then
  echo "session $$" >> work.log
  git add -A >/dev/null 2>&1
  git commit -qm "worker: did work" >/dev/null 2>&1
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#,
    )
    .unwrap();
    chmod_x(&claude);

    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    std::env::set_var("HOME", &home);
    root
});

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(p, perms).unwrap();
}

/// A clean git repo on `main` with one empty commit — session isolation is MANDATORY, so every
/// driver run needs one. Touching [`ENV`] first is what serialises the environment setup.
fn project() -> tempfile::TempDir {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| Command::new("git").args(args).current_dir(tmp.path()).output().unwrap();
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    g(&["commit", "-q", "--allow-empty", "-m", "agg baseline"]);
    tmp
}

/// A driver step that costs one fake session.
fn work() -> Step {
    Step::new("implement").model("fake").prompt("do a chunk")
}

/// Write an executable script judge at `agg/judges/<name>.sh` that APPENDS one line to `<name>.runs`
/// every time it executes, then reports `met`. The marker file is how "did this judge actually run"
/// is observed — a count, not a flag agg set.
fn marker_judge(dir: &Path, name: &str, met: bool) -> Judge {
    let rel = format!("agg/judges/{name}.sh");
    let p = dir.join(&rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\necho ran >> \"$AGG_PROJECT_DIR/{name}.runs\"\nprintf '%s\\n' '{{\"met\":{met}}}'\n"
        ),
    )
    .unwrap();
    chmod_x(&p);
    Judge::script(name, rel)
}

/// How many times a marker judge has executed.
fn runs(dir: &Path, name: &str) -> usize {
    std::fs::read_to_string(dir.join(format!("{name}.runs"))).map(|t| t.lines().count()).unwrap_or(0)
}

// ── the gate's helpers ───────────────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Every `agg/*` branch that still exists — the span's refs, seen from outside agg.
fn agg_branches(dir: &Path) -> Vec<String> {
    let out = git(dir, &["branch", "--list", "agg/*", "--format=%(refname:short)"]);
    out.lines().map(str::to_string).collect()
}

/// How many worker sessions' work is reachable from `rev` — one line per session in `work.log`,
/// written by the fake agent. A commit count would also move for merge commits; this counts WORK.
fn work_on(dir: &Path, rev: &str) -> usize {
    let out =
        Command::new("git").args(["show", &format!("{rev}:work.log")]).current_dir(dir).output().unwrap();
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout).lines().count()
}

fn work_landed_on_main(dir: &Path) -> usize {
    work_on(dir, "main")
}

/// Seed `verdicts.jsonl` with a LANDED, met row for `name` — the state a previous gate would have
/// left. `landed_met` is what the regression rule compares against, so without this a first-ever
/// failing judge is not a regression, it is just news.
fn seed_landed_met(dir: &Path, name: &str) {
    agg::core::verdicts::append(
        dir,
        Some(0),
        "baseline",
        &[(name.to_string(), Verdict::binary(true))],
        agg::core::verdicts::Outcome::Merged,
    )
    .unwrap();
}

// ---------------------------------------------------------------------------------------------

/// THE PIPELINE IS SHARED. A driver step runs the same hook dispatch a YAML lap does, so agg's own
/// artefacts appear without the facade writing any of them itself.
///
/// ⚠ `verdicts.jsonl` is asserted NOWHERE here: with `step()` always staging, `verdicts::append` is
/// not called by a step — it is called by a gate.
#[test]
fn a_driver_step_runs_the_real_pipeline() {
    let tmp = project();
    let dir = tmp.path();

    let agg = Agg::open(dir).unwrap();
    let out = agg.step(&work()).unwrap();

    assert_eq!(out.session, 1);
    assert_eq!(out.step, "implement");
    assert_eq!(out.landed, Landing::Span, "step() STAGES; nothing merges until gate()");
    assert!(out.tokens >= 1, "the worker's reported tokens reached the run counters: {out:?}");
    assert!(out.verdicts.is_empty(), "the run-set is EMPTY on the driver path — judges are lazy");

    assert!(agg::paths::state_json(dir).exists(), "the dashboard snapshot is published");
    let status = agg::ui::status::render(dir);
    assert!(status.contains("session"), "`agg status` renders this run:\n{status}");
    assert!(agg::paths::run_pid(dir).exists(), "the double-run guard is armed while the run is live");

    drop(agg);
    assert!(!agg::paths::run_pid(dir).exists(), "…and released when the Agg is dropped");
}

/// THE LATCH. A driver that ignores every `Err` and keeps calling `step()` after a ceiling fired
/// launches **zero** further workers.
///
/// Asserted on the SESSION COUNTER, not on the error: an `Err` that is returned while the pipeline
/// still runs a worker is exactly the failure this test exists to catch.
#[test]
fn after_a_ceiling_every_further_step_launches_no_worker() {
    let tmp = project();
    let agg = Agg::open(tmp.path()).unwrap().limits(Limits { sessions: Some(1), ..Limits::default() });

    agg.step(&work()).expect("the first step is under the ceiling");
    assert_eq!(agg.sessions(), 1);

    let breach = agg.check_limits().expect_err("session 1 of 1 is the ceiling");
    assert!(matches!(breach, Fatal::Ended(RunOutcome::MaxSessions)), "got {breach:?}");

    // the driver ignores it and ploughs on, exactly as a careless one would.
    for _ in 0..5 {
        let _ = agg.step(&work());
    }
    assert_eq!(agg.sessions(), 1, "the latch must make every later call a no-op that spends nothing");
    assert_eq!(agg.ended(), Some(RunOutcome::MaxSessions));

    // …and a judge asked after the latch reports NOT MET with no number — never a fabricated pass.
    let j = marker_judge(tmp.path(), "always", true);
    let v = agg.judge(&j);
    assert!(!v.met() && v.value().is_none(), "a latched judge is not-met and numberless: {v:?}");
    assert_eq!(runs(tmp.path(), "always"), 0, "…and it did not RUN");
}

/// JUDGES ARE LAZY AND MEMOIZED PER STEP. Asking twice inside one step runs the judge once; the next
/// step is a fresh world and it runs again.
#[test]
fn a_judge_asked_twice_in_one_step_runs_once_and_again_next_step() {
    let tmp = project();
    let dir = tmp.path();
    let tests = marker_judge(dir, "tests_pass", true);

    let agg = Agg::open(dir).unwrap();
    agg.step(&work()).unwrap();

    assert!(agg.judge(&tests).met());
    assert!(agg.judge(&tests).met());
    assert_eq!(runs(dir, "tests_pass"), 1, "memoized for the step — two asks, one execution");

    agg.step(&work()).unwrap();
    assert!(agg.judge(&tests).met());
    assert_eq!(runs(dir, "tests_pass"), 2, "a new step clears the cache — the judge runs again");
}

/// `&&` IS THE GATE. The expensive judge is never reached on a cycle where the cheap one failed —
/// which is the entire replacement for the deleted per-judge `gate:` field.
#[test]
fn a_failed_first_judge_short_circuits_the_expensive_one() {
    let tmp = project();
    let dir = tmp.path();
    let build = marker_judge(dir, "builds", false);
    let load = marker_judge(dir, "load_test", true);

    let agg = Agg::open(dir).unwrap();
    agg.step(&work()).unwrap();

    assert!(!(agg.judge(&build).met() && agg.judge(&load).met()));
    assert_eq!(runs(dir, "builds"), 1);
    assert_eq!(runs(dir, "load_test"), 0, "Rust's `&&` never reached the 40-minute judge");

    // …and it is genuinely reachable — the same expression with a passing first judge runs both.
    let ok = marker_judge(dir, "lint_clean", true);
    assert!(agg.judge(&ok).met() && agg.judge(&load).met());
    assert_eq!(runs(dir, "load_test"), 1);
}

/// `check_limits()` IS OPT-IN, IN BOTH DIRECTIONS. This is the whole ruling: all four ceilings are
/// opt-in TOGETHER, so `limits.sessions` must NOT be enforced behind the driver's back the way the
/// YAML path's `over_max_sessions` enforces it at the top of every lap.
#[test]
fn check_limits_is_opt_in_in_both_directions() {
    // (a) never called ⇒ no ceiling: the run sails past `limits.sessions`.
    let unchecked = project();
    let agg = Agg::open(unchecked.path())
        .unwrap()
        .limits(Limits { sessions: Some(2), ..Limits::default() });
    for _ in 0..4 {
        agg.step(&work()).expect("a driver that never checks has no ceilings");
    }
    assert_eq!(agg.sessions(), 4, "limits.sessions must not be enforced from the step path");
    assert_eq!(agg.ended(), None);

    // (b) called ⇒ it stops at exactly the ceiling.
    let checked = project();
    let agg = Agg::open(checked.path())
        .unwrap()
        .limits(Limits { sessions: Some(2), ..Limits::default() });
    let mut done = 0;
    for _ in 0..4 {
        if agg.check_limits().is_err() {
            break;
        }
        agg.step(&work()).unwrap();
        done += 1;
    }
    assert_eq!(done, 2, "the same limits, checked, stop the run at the ceiling");
    assert_eq!(agg.ended(), Some(RunOutcome::MaxSessions));
}

/// ⛔ A STRAY `agg.yaml` IS IGNORED — not merged, not a fallback.
///
/// The project below declares a token ceiling of 1 in YAML; the driver declares a far larger one in
/// Rust and runs past the YAML number. The second half is the sharper one: the `agg.yaml` is
/// **malformed**, and the run does not care — a file that is never parsed cannot be malformed.
///
/// The policy half of the rule (`on_regression`) is asserted with the `gate()` tests, since a policy
/// about what happens to a span is only observable once a span is closed.
#[test]
fn a_stray_and_even_malformed_agg_yaml_changes_nothing() {
    let tmp = project();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("agg")).unwrap();
    std::fs::write(
        dir.join("agg/agg.yaml"),
        "project: [this is not\n  valid: yaml at all\nsequence: { limits: { tokens: 1 } }\n",
    )
    .unwrap();

    let agg = Agg::open(dir)
        .unwrap()
        .limits(Limits { tokens: Some(1_000_000), ..Limits::default() })
        .on_regression(OnRegression::Annotate);

    let mut last = None;
    for _ in 0..3 {
        agg.check_limits().expect("the RUST ceiling is 1,000,000 tokens — nowhere near");
        last = Some(agg.step(&work()).unwrap());
    }
    let last = last.unwrap();
    assert_eq!(agg.sessions(), 3, "the YAML `limits.tokens: 1` never applied");
    assert!(last.tokens > 1, "…and the run really did spend past it: {} tokens", last.tokens);
    assert_eq!(agg.ended(), None, "a malformed agg.yaml cannot fail a run that never reads it");
}

// ── the gate ─────────────────────────────────────────────────────────────────────────────────

/// `gate()` SEES A VERDICT ASKED FOR THE FIRST TIME AFTER EVERY STEP — under both policies.
///
/// This is the test the pre-D2 design (a gate inside `step()`) passes neither half of: the judge is
/// never consulted while a step is running, so a gate that closed inside `step()` would have had
/// nothing to weigh. Both halves run the SAME script judge against the same seeded history and
/// differ only in `on_regression`, so the policy is the only variable.
#[test]
fn gate_weighs_a_lazily_asked_verdict_under_both_policies() {
    for (policy, expected, landed) in [
        (OnRegression::Rollback, GateOutcome::RolledBack, 0),
        (OnRegression::Annotate, GateOutcome::Kept, 2),
    ] {
        let tmp = project();
        let dir = tmp.path();
        let build = marker_judge(dir, "builds", false);
        seed_landed_met(dir, "builds"); // it WAS met as of the last gate — so this is a regression.

        let agg = Agg::open(dir).unwrap().on_regression(policy);
        agg.step(&work()).unwrap();
        agg.step(&work()).unwrap();
        assert_eq!(runs(dir, "builds"), 0, "{policy:?}: nothing asked yet — judges are LAZY");

        assert!(!agg.judge(&build).met(), "{policy:?}: the judge is red");
        assert_eq!(agg.gate().unwrap(), expected, "{policy:?}");
        assert_eq!(work_landed_on_main(dir), landed, "{policy:?}: sessions on `main`");
    }
}

/// `gate()` LANDS THE WHOLE SPAN, not the last session. Three steps stage on one span; one gate puts
/// all three on `main` and leaves NO span branch behind — every one of them, not just the tip.
///
/// The rollback twin is the other half: `main` untouched, the intermediate branches gone, and the
/// span closed. The tip survives on purpose — `finalize_session` keeps it for inspection, and a
/// rollback that also destroyed the evidence would be a worse failure than the regression.
#[test]
fn gate_lands_the_whole_span_and_the_rollback_twin_clears_every_branch() {
    // (a) keep.
    let tmp = project();
    let dir = tmp.path();
    let agg = Agg::open(dir).unwrap();
    for _ in 0..3 {
        agg.step(&work()).unwrap();
    }
    assert_eq!(work_landed_on_main(dir), 0, "nothing lands until the gate says so");

    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);
    assert_eq!(work_landed_on_main(dir), 3, "ALL THREE sessions merged, not just the tip's");
    assert!(agg_branches(dir).is_empty(), "every span branch is gone: {:?}", agg_branches(dir));
    drop(agg);

    // (b) roll back.
    let tmp = project();
    let dir = tmp.path();
    let build = marker_judge(dir, "builds", false);
    seed_landed_met(dir, "builds");
    let agg = Agg::open(dir).unwrap().on_regression(OnRegression::Rollback);
    agg.step(&work()).unwrap();
    agg.step(&work()).unwrap();
    assert!(!agg.judge(&build).met());

    assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack);
    assert_eq!(work_landed_on_main(dir), 0, "`main` is untouched");
    let left = agg_branches(dir);
    assert_eq!(left.len(), 1, "only the tip survives, for inspection: {left:?}");
    assert!(left[0].ends_with("session-2"), "…and it is the TIP, holding the whole span: {left:?}");
    assert_eq!(agg.gate().unwrap(), GateOutcome::Nothing, "the span is CLOSED — nothing is left open");
}

/// `gate()` WITH NOTHING STAGED returns `Nothing` and touches no ref. A driver may call it at the
/// bottom of every `for` body without first asking whether a step ran.
#[test]
fn gate_with_nothing_staged_touches_no_ref() {
    let tmp = project();
    let dir = tmp.path();
    let agg = Agg::open(dir).unwrap();
    let before = git(dir, &["rev-parse", "main"]);

    assert_eq!(agg.gate().unwrap(), GateOutcome::Nothing);
    assert_eq!(agg.gate().unwrap(), GateOutcome::Nothing, "…and it is idempotent");
    assert_eq!(git(dir, &["rev-parse", "main"]), before, "no ref moved");
    assert!(agg_branches(dir).is_empty());
}

/// A CONFLICTED SPAN IS `Failed(Conflict)`, NEVER `RolledBack` — and the span SURVIVES.
///
/// A merge conflict is not a policy decision and not a discard: git aborted, base is untouched and
/// the tip still holds the work, so the span is still gateable once the operator resolves it.
/// Asserted by gating AGAIN and getting the same answer — `Nothing` there would mean `gate()` had
/// quietly thrown the span away.
#[test]
fn a_conflicted_span_fails_without_discarding_it() {
    let tmp = project();
    let dir = tmp.path();
    let agg = Agg::open(dir).unwrap();
    agg.step(&work()).unwrap();

    // Move base under the span, in a SEPARATE worktree — the primary tree must stay where the run
    // left it (on the span tip), or the test would be measuring its own `git checkout`.
    let wt = tempfile::tempdir().unwrap();
    let base = wt.path().join("base");
    let base_str = base.to_str().unwrap();
    git(dir, &["worktree", "add", "-f", base_str, "main"]);
    std::fs::write(base.join("work.log"), "the operator edited base\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-qm", "base moved under the span"]);
    git(dir, &["worktree", "remove", "--force", base_str]);

    let before = git(dir, &["rev-parse", "main"]);
    assert_eq!(agg.gate().unwrap(), GateOutcome::Failed(GateFailure::Conflict));
    assert_eq!(git(dir, &["rev-parse", "main"]), before, "a conflict leaves base exactly as it was");
    let left = agg_branches(dir);
    assert_eq!(left.len(), 1, "the span tip is KEPT — the work is still there: {left:?}");
    assert_eq!(
        agg.gate().unwrap(),
        GateOutcome::Failed(GateFailure::Conflict),
        "the span is still OPEN and still gateable"
    );
}

// ── git normalization at open + the run end (BUILD.md §3.9 / §3.10) ──────────────────────────

/// A STALE SPAN SURVIVES A FRESH RUN. Run 1 stages two sessions and never gates; run 2 starts, and
/// run 1's branches are still there with their commits.
///
/// Without §3.9 rule 3 this is a silent data-loss bug and not a small one: session numbering restarts
/// per run and `create_branch` opens with `git branch -D`, so run 2's session-1 deletes run 1's
/// session-1, session-2 deletes session-2, and the very branch run 1's exit warning told the operator
/// to `git merge` is gone by the time they read it.
#[test]
fn a_previous_runs_stranded_span_is_parked_aside_not_deleted() {
    let tmp = project();
    let dir = tmp.path();

    // run 1: two sessions, never gated. The `Agg` is dropped, which is the whole of "run 1 ended".
    {
        let agg = Agg::open(dir).unwrap();
        agg.step(&work()).unwrap();
        agg.step(&work()).unwrap();
    }
    let after_run_1 = agg_branches(dir);
    assert_eq!(after_run_1.len(), 2, "run 1 left its span: {after_run_1:?}");
    let tip = after_run_1.iter().find(|b| b.ends_with("session-2")).unwrap().clone();
    assert_eq!(work_on(dir, &tip), 2, "both of run 1's sessions are on its tip");

    // run 2 starts in exactly the state run 1 left behind.
    let agg = Agg::open(dir).unwrap();
    agg.step(&work()).unwrap();

    let parked: Vec<String> = agg_branches(dir).into_iter().filter(|b| b.contains("/orphaned-")).collect();
    assert_eq!(parked.len(), 2, "run 1's whole span was parked, not eaten: {parked:?}");
    let parked_tip = parked.iter().find(|b| b.ends_with("session-2")).expect("the tip keeps its number");
    assert_eq!(work_on(dir, parked_tip), 2, "…and it still carries run 1's two sessions' commits");

    // and run 2 really did reuse the numbering the parking freed up.
    assert!(
        agg_branches(dir).iter().any(|b| !b.contains("/orphaned-") && b.ends_with("session-1")),
        "run 2 cut its own session-1: {:?}",
        agg_branches(dir)
    );
}

/// A CRASHED HEAD DOES NOT BECOME THE BASE. A run that never gates leaves HEAD on its session branch,
/// which is exactly the state a mid-span crash leaves — so the next run must recover the RECORDED
/// base rather than resolve one from the dead branch it happens to be standing on.
///
/// Without §3.9 rule 1 every gate in run 2 merges into the dead branch, returns `Kept`, writes
/// `Merged` to `verdicts.jsonl`, and `main` never moves — with nothing warning, because the span
/// genuinely *was* gated. `work_landed_on_main` is what separates the two worlds.
#[test]
fn a_crashed_head_on_a_session_branch_never_becomes_the_base() {
    let tmp = project();
    let dir = tmp.path();
    {
        let agg = Agg::open(dir).unwrap();
        agg.step(&work()).unwrap();
    }
    let head = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(head.starts_with("agg/"), "the span left HEAD on its session branch: {head}");

    let agg = Agg::open(dir).unwrap();
    agg.step(&work()).unwrap();
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);

    assert_eq!(
        agg::state::DashboardState::read(dir).unwrap().iso_base,
        "main",
        "the base is the RECORDED one, never the dead session branch"
    );
    assert_eq!(work_landed_on_main(dir), 1, "run 2's session landed on `main` — the gate merged there");
    let parked: Vec<String> = agg_branches(dir).into_iter().filter(|b| b.contains("/orphaned-")).collect();
    assert_eq!(parked.len(), 1, "run 1's dead span is parked, not merged: {parked:?}");
}

/// THE UNGATED-SPAN WARNING. A driver that never gates ends with `main` unchanged and every session's
/// work on the span branch — so the run's last word must name that branch and the command that lands
/// it.
///
/// The commit count is asserted alongside the wording on purpose: a warning that fires while the work
/// is actually gone would be worse than no warning at all.
#[test]
fn a_run_that_never_gates_names_the_branch_and_the_merge_that_lands_it() {
    let tmp = project();
    let dir = tmp.path();
    let agg = Agg::open(dir).unwrap();
    for _ in 0..3 {
        agg.step(&work()).unwrap();
    }
    let tip = agg_branches(dir).into_iter().find(|b| b.ends_with("session-3")).unwrap();

    // the sentence agg is about to print, taken from the run's OWN git state.
    assert_eq!(
        agg.ungated_span().expect("three sessions staged, no gate"),
        format!("⚠ 3 session(s) staged on {tip}, never gated — main is unchanged.\n  Merge with: git merge {tip}")
    );
    drop(agg);

    assert_eq!(work_landed_on_main(dir), 0, "`main` really is unchanged");
    assert_eq!(work_on(dir, &tip), 3, "…and all three sessions really are on the branch it named");
}

/// ⛔ THE RESUME PATH DISCARDS A DIRTY TREE; A FRESH START STILL REFUSES.
///
/// A power cut mid-worker leaves uncommitted tracked changes by construction — agg commits a session's
/// work only after it ends — so a resume that refused to start would make the flagship resume scenario
/// the one scenario that cannot start. Both directions, because the refusal is the moat for everyone
/// who is NOT resuming: a dirty tree there means the operator's own uncommitted work.
#[test]
fn a_dirty_tree_refuses_a_fresh_start_and_is_discarded_on_a_resume() {
    let tmp = project();
    let dir = tmp.path();
    std::fs::write(dir.join("seed.txt"), "committed\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "seed"]);

    let dirty = || std::fs::write(dir.join("seed.txt"), "a crashed worker's half-finished edit\n").unwrap();

    dirty();
    let refused = Agg::open(dir).unwrap().step(&work()).expect_err("a fresh start must refuse a dirty tree");
    assert!(format!("{refused}").contains("uncommitted tracked changes"), "got {refused:?}");
    assert_eq!(std::fs::read_to_string(dir.join("seed.txt")).unwrap(), "a crashed worker's half-finished edit\n");

    dirty();
    let agg = Agg::open_with(dir, Opts { resume: true }).unwrap();
    agg.step(&work()).expect("a resume starts, and says loudly what it discarded");
    assert_eq!(
        std::fs::read_to_string(dir.join("seed.txt")).unwrap(),
        "committed\n",
        "the dead session's edit was discarded, not carried into the new run's baseline"
    );
}

// ── the sample driver, actually RUN (BUILD.md §5 — "the samples work as expected") ────────────

/// A script judge that reports `met` and writes NOTHING.
///
/// ⚠ Deliberately NOT [`marker_judge`], and the reason is a real edge this suite found the first
/// time the sample ran for more than one cycle: its `<name>.runs` marker lands in the project dir,
/// the worker's `git add -A` commits it on the NEXT session, and the judge's next append then leaves
/// a DIRTY TRACKED file — which is enough to make `gate()`'s `git checkout <base>` refuse, so the
/// gate returns `Failed(CheckoutFailed)` and the span stays open. agg is being loud and correct
/// there (it will not clobber a local change it did not make), but the consequence is worth knowing:
/// **a judge that scribbles into a tracked path can wedge a span.** Real projects gitignore judge
/// scratch output; this one simply produces none.
fn quiet_judge(dir: &Path, name: &str, met: bool) -> Judge {
    let rel = format!("agg/judges/{name}.sh");
    let p = dir.join(&rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, format!("#!/bin/sh\nprintf '%s\\n' '{{\"met\":{met}}}'\n")).unwrap();
    chmod_x(&p);
    Judge::script(name, rel)
}

/// `examples/workflow.rs`, cut to its skeleton: one cycle is
/// **survey (bounded retry) → implement → `&&`-gated lazy judges → `gate()`**.
///
/// This is a REAL driver, not a test body: `&Agg` in, `Result<_, Fatal>` out, `?` on every spending
/// call — the shape a compiled driver's `main` has. `check` and `gate` are the two lines whose
/// ABSENCE is the interesting case (`check_limits()` is opt-in; a driver that never gates must be
/// told its work is stranded), so the three tests below differ by those flags and nothing else.
///
/// Three things are cut from the sample, each because the fake agent cannot honour it:
/// the two RUBRIC judges (they cost a ruler call the fake agent answers with prose, not a verdict),
/// `block()` (it waits for an operator who is not there), and `Isolation::Sandbox` (a real kernel
/// jail — the `#[ignore]`d isolation tests own that). Everything else is the sample's own flow.
///
/// Returns how many cycles closed a span.
fn sample_driver(agg: &Agg, dir: &Path, cycles: u64, check: bool, gate: bool) -> Result<usize, Fatal> {
    // Two scripts and one native cover every judge dispatch arm reachable without a model.
    let builds = quiet_judge(dir, "builds", true);
    let tests = quiet_judge(dir, "tests_pass", true);
    let load = quiet_judge(dir, "load_ok", true);
    let worked = Judge::native("worked", |c| {
        // a pure function of committed repo state — no clock, no env, exactly as the sample's is.
        Verdict::binary(c.read("work.log").map(|t| !t.trim().is_empty()).unwrap_or(false))
    });

    // The sample's template family, minus the tier. ⚠ NO `.readonly()` either: it binds to nothing
    // under `Isolation::None`, and a sample that relied on that would be teaching the lie.
    let harness = Step::template().agent(Agent::Claude).model("fake").effort(Effort::High);
    let survey = harness.create("survey").prompt("Survey the approaches. Write agg/state/wiki/survey.md.");
    let implement = harness.create("implement").prompt("Implement the spec. Add tests alongside.");

    let cycle = agg.pos("cycle", cycles);
    let mut gated = 0usize;
    for c in 0..cycles {
        cycle.update(c);
        // THE CEILINGS, ENFORCED WHERE THIS DRIVER SAYS. Never called ⇒ never enforced.
        if check {
            agg.check_limits()?;
        }

        // ── DISCOVER — the sample's bounded attempt loop, and its nested `pos` frame ──
        let attempt = agg.pos("attempt", 2);
        for a in 0..2 {
            attempt.update(a);
            agg.step(&survey)?;
            assert_eq!(agg.label_path(), format!("cycle {c}/{cycles} › attempt {a}/2"));
            if agg.judge(&builds).met() {
                break;
            }
            agg.info("survey still thin — trying a different angle");
        }
        drop(attempt);

        // ── BUILD ──
        let r = agg.step(&implement)?;
        agg.log(&format!("implement: {} tokens, landed={:?}", r.tokens, r.landed));
        assert_eq!(r.landed, Landing::Span, "step() STAGES; only gate() lands anything");
        assert_eq!(
            work_landed_on_main(dir),
            gated * 2,
            "`main` is exactly where the last gate left it — this cycle's sessions are only STAGED"
        );

        // ── HARDEN — `&&` short-circuits, and it is the only judge gating in the design ──
        if !(agg.judge(&tests).met() && agg.judge(&load).met() && agg.judge(&worked).met()) {
            agg.ask("performance gate failed — ship anyway, or keep tuning?");
            continue;
        }

        // ── SHIP ──
        if gate {
            assert_eq!(agg.gate()?, GateOutcome::Kept);
            gated += 1;
        }
    }
    Ok(gated)
}

/// THE SAMPLE DRIVER, RUN. `examples/workflow.rs` compiles, which proves the SURFACE; this proves
/// the LOOP — two cycles of the sample's own shape against the fake agent, with every artefact the
/// pipeline is supposed to produce read back from OUTSIDE the facade (git, `state.json`,
/// `verdicts.jsonl`), never from a flag the facade set.
#[test]
fn the_sample_drivers_cycle_runs_end_to_end_and_lands_every_span() {
    let tmp = project();
    let dir = tmp.path();
    // the `agg/` scaffold a real driver project has. `instructions()` only POINTS at this file —
    // agg never reads its bytes — so its content is irrelevant and its existence is not.
    std::fs::create_dir_all(dir.join("agg")).unwrap();
    std::fs::write(dir.join("agg/AGG.md"), "# house rules\n").unwrap();

    let agg = Agg::open(dir)
        .unwrap()
        .limits(Limits { tokens: Some(40_000_000), cost: None, sessions: Some(400), wall_hours: Some(12.0) })
        .on_regression(OnRegression::Annotate)
        .instructions("agg/AGG.md");

    assert_eq!(sample_driver(&agg, dir, 2, true, true).unwrap(), 2, "both cycles closed their span");

    // sessions really ran, and the loop published itself as it went.
    assert_eq!(agg.sessions(), 4, "two cycles × (one survey + one implement)");
    assert!(agg::paths::state_json(dir).exists(), "the dashboard snapshot is published");
    assert!(agg.summary().contains("session"), "`agg status` renders this run:\n{}", agg.summary());
    assert!(agg::paths::run_pid(dir).exists(), "the double-run guard is armed while the run is live");

    // the work LANDED — all of it — and no span is left open.
    assert_eq!(work_landed_on_main(dir), 4, "every gated session is on `main`");
    assert!(agg_branches(dir).is_empty(), "every span branch is gone: {:?}", agg_branches(dir));
    assert!(agg.ungated_span().is_none(), "nothing is stranded — both spans were gated");

    // ⚠ ONE `verdicts.jsonl` ROW PER GATE, never per step — `verdicts::append` is the gate's call.
    // Both gates kept, so both rows say `merged`, which is what `landed_met` reads back next run.
    let rows = agg::core::verdicts::rows_for(dir, "tests_pass", false);
    assert_eq!(rows.len(), 2, "one row per gate, not one per step: {rows:?}");
    assert!(rows.iter().all(|r| r.met && r.outcome == agg::core::verdicts::Outcome::Merged), "{rows:?}");
    // the NATIVE judge is in there too: a closure in the driver's own binary reaches the same
    // durable ledger a script judge does.
    assert_eq!(agg::core::verdicts::rows_for(dir, "worked", true).len(), 2);
    // …and a judge the driver never asked for appears in NO row. Laziness is visible from the ledger.
    assert!(agg::core::verdicts::rows_for(dir, "spec_sound", false).is_empty());

    drop(agg);
    assert!(!agg::paths::run_pid(dir).exists(), "…and released when the run ends");
}

/// THE SAME DRIVER WITH ITS `gate()` REMOVED. Nothing is lost and nothing moves — and the run's last
/// word names the branch that holds the work plus the one command that lands it.
///
/// The commit count is asserted next to the wording deliberately: a warning that fires while the
/// work is actually gone would be worse than no warning at all.
#[test]
fn the_sample_driver_without_its_gate_strands_the_span_and_says_so() {
    let tmp = project();
    let dir = tmp.path();
    let agg = Agg::open(dir).unwrap();

    assert_eq!(sample_driver(&agg, dir, 2, true, false).unwrap(), 0, "no cycle closed a span");

    let tip = agg_branches(dir).into_iter().find(|b| b.ends_with("session-4")).expect("the span tip");
    assert_eq!(
        agg.ungated_span().expect("four sessions staged, never gated"),
        format!("⚠ 4 session(s) staged on {tip}, never gated — main is unchanged.\n  Merge with: git merge {tip}")
    );
    assert_eq!(work_landed_on_main(dir), 0, "`main` really is unchanged");
    assert_eq!(work_on(dir, &tip), 4, "…and all four sessions really are on the branch it named");
}

/// `check_limits()` IS OPT-IN, ON THE SAMPLE'S OWN SHAPE — where the call sits at the top of the
/// cycle and `?` carries the breach out of the driver. Same driver, same ceiling, one line different.
#[test]
fn the_sample_drivers_ceiling_binds_only_because_the_cycle_checks_it() {
    // (a) the line is there: `limits.sessions: 2` ends the run at the top of cycle 2.
    let tmp = project();
    let agg = Agg::open(tmp.path()).unwrap().limits(Limits { sessions: Some(2), ..Limits::default() });
    let stopped = sample_driver(&agg, tmp.path(), 2, true, true).expect_err("cycle 2 is over the ceiling");
    assert!(matches!(stopped, Fatal::Ended(RunOutcome::MaxSessions)), "got {stopped:?}");
    assert_eq!(agg.sessions(), 2, "it landed BEFORE cycle 2 spent anything");
    assert_eq!(work_landed_on_main(tmp.path()), 2, "…and cycle 1's gated work is still on `main`");

    // (b) the line is gone: the identical ceiling never binds.
    let tmp = project();
    let agg = Agg::open(tmp.path()).unwrap().limits(Limits { sessions: Some(2), ..Limits::default() });
    assert_eq!(sample_driver(&agg, tmp.path(), 2, false, true).unwrap(), 2, "both cycles ran to their gate");
    assert_eq!(agg.sessions(), 4, "a driver that never checks has no ceilings");
    assert_eq!(agg.ended(), None);
}

/// `pos` is RAII and removes ITS OWN frame — never the top one. A non-LIFO drop is legal Rust
/// (`drop(outer)` before an inner guard) and must not corrupt the label path.
#[test]
fn a_pos_frame_is_removed_by_id_not_by_popping() {
    let tmp = project();
    let agg = Agg::open(tmp.path()).unwrap();

    let outer = agg.pos("cycle", 20);
    let inner = agg.pos("attempt", 3);
    outer.update(7);
    inner.update(2);
    assert_eq!(agg.label_path(), "cycle 7/20 › attempt 2/3");

    drop(outer); // NOT the top of the stack
    assert_eq!(agg.label_path(), "attempt 2/3", "the wrong frame would have been popped");
    drop(inner);
    assert_eq!(agg.label_path(), "");
}
