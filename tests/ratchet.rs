//! The numeric RATCHET and the gate's own terms (`rollbacks`, `consecutive_rollbacks`,
//! `oscillating`, `ratchet_regressed`, `<judge>.ref`) — end to end, on both front ends.
//!
//! The YAML half drives the real `agg` binary with a fake `claude` whose sessions follow a PLAN: one
//! shell line per session, run in the project, then committed. The plan sits OUTSIDE the project so
//! a rollback cannot restore it (a rolled-back session must not replay its own line).
//!
//! The driver half drives the real facade, with a judge whose `value` is the number of sessions
//! whose work is in the tree — so "worse" and "better" are a direction, not a fixture file.
//!
//! Unix-only (the fake agent is a shell script), like the rest of the suite.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use agg::core::verdicts::{rows_for, Outcome, RatchetMark};
use agg::prelude::*;

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    p
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

fn write_judge(dir: &Path, name: &str, body: &str) {
    chmod_x(&write(dir, &format!("agg/judges/{name}.sh"), body));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ── the YAML path ──────────────────────────────────────────────────────────────────────────────

/// A project whose fake worker runs one line of `plan` per session. Returns (tempdir, project, PATH).
/// The caller writes judges + config, then calls [`commit_base`].
fn planned_project(plan: &[&str]) -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    let proj = tmp.path().join("proj");
    fs::create_dir_all(&proj).unwrap();
    let plan_file = write(&bin, "plan", &format!("{}\n", plan.join("\n")));
    let claude = write(
        &bin,
        "claude",
        &format!(
            r#"#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && {{ echo "fake 0.0.0"; exit 0; }}; done
line=$(head -n 1 '{plan}')
tail -n +2 '{plan}' > '{plan}.rest' && mv '{plan}.rest' '{plan}'
sh -c "$line"
git add -A >/dev/null 2>&1
git commit -qm "worker: $line" >/dev/null 2>&1
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{{"output_tokens":1}},"total_cost_usd":0.0}}'
exit 0
"#,
            plan = plan_file.display()
        ),
    );
    chmod_x(&claude);
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(&proj, args);
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (tmp, proj, path)
}

/// Commit the fixture as `main`. The `.gitignore` is committed WITH it: on a project whose base has
/// none, the first session branch is what commits it, so rolling that session back deletes it from
/// the work tree and the next worker's `git add -A` commits `agg/private/` (a pre-existing trap that
/// these tests, which roll back on session 1 by design, would otherwise walk into).
fn commit_base(dir: &Path) {
    write(dir, ".gitignore", "agg/state/\nagg/private/\n.obsidian/\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
}

/// `agg.yaml` with one judged `worker` step, a never-met `feature` DoD, and `seq_extra` under
/// `sequence:`.
fn write_cfg(dir: &Path, seq_extra: &str) {
    write(
        dir,
        "agg/agg.yaml",
        &format!(
            "project: ratchet\ndefaults: {{ model: fake }}\nsteps:\n  worker: {{}}\n\
             sequence:\n  steps: [worker]\n  done_if: \"feature\"\n{seq_extra}\
             summary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"
        ),
    );
    write_judge(dir, "feature", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1}'\n");
}

/// The metric judge: its `value` is whatever `metric.txt` says.
fn metric_judge(dir: &Path) {
    write_judge(dir, "cc", "#!/bin/sh\nprintf '{\"met\":true,\"value\":%s}\\n' \"$(cat metric.txt)\"\n");
}

fn run(dir: &Path, path: &str, sessions: &str) -> (std::process::Output, String) {
    let out = Command::cargo_bin("agg")
        .unwrap()
        .current_dir(dir)
        .env("PATH", path)
        .env("HOME", dir.join(".home"))
        .args(["run", "--max-sessions", sessions])
        .output()
        .unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out, combined)
}

/// Per session, the outcome the gate stamped on `cc`'s row and the ratchet mark it carries.
fn cc_rows(dir: &Path) -> Vec<(Option<u32>, Outcome, Option<RatchetMark>)> {
    rows_for(dir, "cc", false).into_iter().map(|r| (r.session, r.outcome, r.ratchet)).collect()
}

/// THE THREE ACCEPTANCE ROWS in one run, against a lower-is-better metric with tolerance 0.5 and no
/// seed (so the baseline pass measures the reference, 10):
///   session 1 → 10.4  worse WITHIN tolerance   → merged,      reference 10.4
///   session 2 → 11.5  worse BEYOND tolerance   → rolled back, recorded as 10.4 → 11.5
///   session 3 → 9     better                   → merged,      reference moves to 9
#[test]
fn the_ratchet_rolls_back_beyond_tolerance_keeps_within_it_and_moves_on_improvement() {
    let (_tmp, dir, path) =
        planned_project(&["echo 10.4 > metric.txt", "echo 11.5 > metric.txt", "echo 9 > metric.txt"]);
    write(&dir, "metric.txt", "10\n");
    metric_judge(&dir);
    write_cfg(&dir, "  ratchet:\n    cc: { direction: \"down\", tolerance: 0.5 }\n");
    commit_base(&dir);

    let (out, combined) = run(&dir, &path, "3");
    assert_eq!(out.status.code(), Some(4), "the never-met DoD runs to the cap:\n{combined}");

    let mark = |from: f64, to: f64| Some(RatchetMark { from: Some(from), to: Some(to) });
    assert_eq!(
        cc_rows(&dir),
        vec![
            (None, Outcome::Baseline, None),
            (Some(1), Outcome::Merged, None),
            (Some(2), Outcome::RolledBack, mark(10.4, 11.5)),
            (Some(3), Outcome::Merged, None),
        ],
        "{combined}"
    );
    assert!(combined.contains("ratchet `cc`: 10.4 → 11.5"), "the operator is told which judge, old and new:\n{combined}");
    assert_eq!(git(&dir, &["show", "main:metric.txt"]), "9", "the improvement landed, the regression did not");

    let state = agg::core::ratchet::load(&dir).expect("the gate state is persisted under agg/private/");
    assert_eq!(state.reference("cc"), Some(9.0), "an improvement moves the reference");
    assert_eq!((state.rollbacks, state.consecutive_rollbacks), (1, 0));
}

/// The SEED is respected on session 1: the measured baseline (60) would accept 65, but the seed
/// (71, higher is better) does not — and the gap between seed and measurement is reported as
/// drift. Two such sessions in a row trip `consecutive_rollbacks >= 2` in `abort_if`.
#[test]
fn the_seeded_baseline_holds_session_one_and_consecutive_rollbacks_abort() {
    let (_tmp, dir, path) = planned_project(&["echo 65 > metric.txt", "echo 66 > metric.txt", "echo 99 > metric.txt"]);
    write(&dir, "metric.txt", "60\n");
    metric_judge(&dir);
    write_cfg(
        &dir,
        "  ratchet:\n    cc: { direction: \"up\", tolerance: 1.0, baseline: 71.0 }\n  abort_if: \"consecutive_rollbacks >= 2\"\n",
    );
    commit_base(&dir);

    let (out, combined) = run(&dir, &path, "5");
    assert_eq!(out.status.code(), Some(3), "two rollbacks in a row abort:\n{combined}");
    assert!(combined.contains("drift"), "seed 71 vs measured 60 is reported:\n{combined}");
    assert!(combined.contains("ABORT") && combined.contains("consecutive_rollbacks >= 2"), "{combined}");
    let rows = cc_rows(&dir);
    assert_eq!(rows[1], (Some(1), Outcome::RolledBack, Some(RatchetMark { from: Some(71.0), to: Some(65.0) })));
    assert_eq!(rows.len(), 3, "the run stopped after session 2 — session 3's 99 never ran:\n{combined}");
    assert_eq!(git(&dir, &["show", "main:metric.txt"]), "60", "nothing landed");
}

/// A ratcheted judge that emits no `value` REFUSES the run before session 1 — a ratchet with nothing
/// to compare would silently pass everything.
#[test]
fn a_ratcheted_judge_without_a_value_refuses_at_startup() {
    let (_tmp, dir, path) = planned_project(&["echo nope > touched.txt"]);
    write_judge(&dir, "cc", "#!/bin/sh\necho '{\"met\":true}'\n");
    write_cfg(&dir, "  ratchet:\n    cc: { direction: \"down\" }\n");
    commit_base(&dir);

    let (out, combined) = run(&dir, &path, "3");
    assert_eq!(out.status.code(), Some(1), "a hard startup error:\n{combined}");
    assert!(combined.contains("emitted no `value`"), "{combined}");
    assert!(!dir.join("touched.txt").exists(), "no worker session ran");
}

/// A ratcheted judge that ERRORS on a session never passes: the session is rolled back and the row
/// says the judge measured nothing — unlike the boolean rule, where a flake keeps the merge.
#[test]
fn a_ratcheted_judge_that_errors_never_passes() {
    let (_tmp, dir, path) = planned_project(&["echo broken > metric.txt"]);
    write(&dir, "metric.txt", "10\n");
    write_judge(
        &dir,
        "cc",
        "#!/bin/sh\nv=$(cat metric.txt)\n[ \"$v\" = broken ] && { echo crashed >&2; exit 2; }\nprintf '{\"met\":true,\"value\":%s}\\n' \"$v\"\n",
    );
    write_cfg(&dir, "  ratchet:\n    cc: { direction: \"down\" }\n");
    commit_base(&dir);

    let (_, combined) = run(&dir, &path, "1");
    assert_eq!(cc_rows(&dir)[1], (Some(1), Outcome::RolledBack, Some(RatchetMark { from: Some(10.0), to: None })), "{combined}");
}

/// Two goals, a worker that fixes one and breaks the other: the unmet sets go {b} → {a} → {b} —
/// A→B→A — and `oscillating` in `abort_if` stops the run on session 3.
#[test]
fn oscillating_stops_an_a_b_a_run() {
    let (_tmp, dir, path) =
        planned_project(&["echo a > state.txt", "echo b > state.txt", "echo a > state.txt", "echo ab > state.txt"]);
    write(&dir, "state.txt", "\n");
    for j in ["a", "b"] {
        write_judge(
            &dir,
            j,
            &format!("#!/bin/sh\ngrep -q {j} state.txt && echo '{{\"met\":true}}' || echo '{{\"met\":false}}'\n"),
        );
    }
    write_cfg(&dir, "  invariants: [a, b]\n  abort_if: \"oscillating\"\n");
    commit_base(&dir);

    let (out, combined) = run(&dir, &path, "6");
    assert_eq!(out.status.code(), Some(3), "{combined}");
    assert!(combined.contains("ABORT") && combined.contains("oscillating"), "{combined}");
    let state = agg::core::ratchet::load(&dir).unwrap();
    assert_eq!(state.signatures.len(), 3, "it fired on the third judged session, not later");
    assert!(state.oscillating(4));
}

/// The discriminator: a run that only ever gets BETTER trips neither detector and reaches its DoD.
#[test]
fn a_monotonically_improving_run_trips_neither_detector() {
    let (_tmp, dir, path) =
        planned_project(&["echo a > state.txt", "echo ab > state.txt", "echo abc > state.txt"]);
    write(&dir, "state.txt", "\n");
    for j in ["a", "b", "c"] {
        write_judge(
            &dir,
            j,
            &format!("#!/bin/sh\ngrep -q {j} state.txt && echo '{{\"met\":true}}' || echo '{{\"met\":false}}'\n"),
        );
    }
    write(
        &dir,
        "agg/agg.yaml",
        "project: mono\ndefaults: { model: fake }\nsteps:\n  worker: {}\n\
         sequence:\n  steps: [worker]\n  done_if: \"a AND b AND c\"\n  abort_if: \"oscillating OR rollbacks >= 1\"\n\
         summary: { enabled: false }\nmemory: { enabled: false }\n",
    );
    commit_base(&dir);

    let (out, combined) = run(&dir, &path, "6");
    assert_eq!(out.status.code(), Some(0), "done, never aborted:\n{combined}");
    let state = agg::core::ratchet::load(&dir).unwrap();
    assert_eq!(state.rollbacks, 0);
    assert!(!state.oscillating(4));
}

// ── the Rust driver ────────────────────────────────────────────────────────────────────────────

/// A fake `claude` that appends one line to `work.log` per worker session and commits it — set on
/// the process PATH exactly once (tests run in parallel threads).
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-ratchet-env-{}", std::process::id()));
    let bin = root.join("bin");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let claude = write(
        &bin,
        "claude",
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
    );
    chmod_x(&claude);
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    std::env::set_var("HOME", &home);
    root
});

fn driver_project() -> (tempfile::TempDir, Judge) {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(tmp.path(), args);
    }
    git(tmp.path(), &["commit", "-q", "--allow-empty", "-m", "agg baseline"]);
    write_judge(
        tmp.path(),
        "size",
        "#!/bin/sh\nn=$(cat work.log 2>/dev/null | wc -l | tr -d ' ')\nprintf '{\"met\":true,\"value\":%s}\\n' \"$n\"\n",
    );
    let j = Judge::script("size", "agg/judges/size.sh");
    (tmp, j)
}

fn work() -> Step {
    Step::new("implement").model("fake").prompt("do a chunk")
}

/// The driver's ratchet: the value asked BEFORE the first step is the baseline, a kept improvement
/// moves the reference, and a regression beyond tolerance rolls the span back — recorded with the
/// judge, its old and its new value.
#[test]
fn a_driver_ratchet_keeps_improvements_and_rolls_back_regressions() {
    // higher is better: every session's work is an improvement.
    let (tmp, size) = driver_project();
    let agg = Agg::open(tmp.path()).unwrap().ratchet("size", Ratchet::up(0.0));
    assert_eq!(agg.judge(&size).value(), Some(0.0), "the pre-step ask IS the baseline");
    assert_eq!(agg.gate_state().reference("size"), Some(0.0));
    agg.step(&work()).unwrap();
    agg.judge(&size);
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);
    assert_eq!(agg.gate_state().reference("size"), Some(1.0), "the improvement is the new reference");
    drop(agg);

    // lower is better: the same work is a regression beyond tolerance 0 …
    let (tmp, size) = driver_project();
    let agg = Agg::open(tmp.path()).unwrap().ratchet("size", Ratchet::down(0.0));
    agg.judge(&size);
    agg.step(&work()).unwrap();
    agg.judge(&size);
    assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack);
    let st = agg.gate_state();
    assert_eq!((st.rollbacks, st.consecutive_rollbacks, st.reference("size")), (1, 1, Some(0.0)));
    assert!(st.ratchet_regressed());
    let row = &rows_for(tmp.path(), "size", false)[0];
    assert_eq!(row.ratchet, Some(RatchetMark { from: Some(0.0), to: Some(1.0) }));
    drop(agg);

    // … and within tolerance 1 it is kept.
    let (tmp, size) = driver_project();
    let agg = Agg::open(tmp.path()).unwrap().ratchet("size", Ratchet::down(1.0));
    agg.judge(&size);
    agg.step(&work()).unwrap();
    agg.judge(&size);
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);
}

/// A SEED holds the first gate even when the driver never asked before its first step.
#[test]
fn a_driver_seed_holds_the_first_gate() {
    let (tmp, size) = driver_project();
    let agg = Agg::open(tmp.path()).unwrap().ratchet("size", Ratchet::up(0.0).baseline(5.0));
    assert_eq!(agg.gate_state().reference("size"), Some(5.0), "seeded before the run even started");
    agg.step(&work()).unwrap();
    assert_eq!(agg.judge(&size).value(), Some(1.0));
    assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack, "1 is below the seeded 5");
}

/// RESUME restores the gate state AS OF THE LAST KEPT GATE — the reference it set, and NOT the
/// rollback that came after it (that gate was truncated and re-executes, so counting it from the
/// file would count it twice).
#[test]
fn a_resumed_driver_restores_the_reference_as_of_the_last_kept_gate() {
    let (tmp, size) = driver_project();
    let dir = tmp.path();
    let open = |resume: bool| Agg::open_with(dir, Opts { resume }).unwrap().ratchet("size", Ratchet::down(1.0));
    let flow_to_kept = |agg: &Agg| {
        agg.judge(&size);
        agg.step(&work()).unwrap();
        agg.judge(&size);
        assert_eq!(agg.gate().unwrap(), GateOutcome::Kept, "1 is within tolerance 1 of 0");
    };
    let regress = |agg: &Agg| {
        agg.step(&work()).unwrap();
        agg.step(&work()).unwrap();
        agg.judge(&size);
        assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack, "3 is 2 worse than 1");
    };

    {
        let agg = open(false);
        flow_to_kept(&agg);
        regress(&agg);
        assert_eq!(agg.gate_state().rollbacks, 1);
    }
    assert_eq!(agg::core::ratchet::load(dir).unwrap().rollbacks, 1, "the file is AHEAD of the truncation point");

    let agg = open(true);
    flow_to_kept(&agg); // fast-forwarded
    let st = agg.gate_state();
    assert_eq!(st.reference("size"), Some(1.0), "the reference survives the restart");
    assert_eq!(st.rollbacks, 0, "the dropped gate's rollback is not carried over");
    regress(&agg); // re-executes for real
    assert_eq!(agg.gate_state().rollbacks, 1, "counted once, not twice");
}
