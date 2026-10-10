//! Judge GROUPS (`sequence.groups`, k-of-n aggregates, the gate's net group rule) — end to end, on
//! both front ends.
//!
//! The YAML half drives the real `agg` binary with a fake `claude` whose sessions follow a PLAN: one
//! shell line per session, run in the project, then committed. The plan sits OUTSIDE the project so
//! a rollback cannot restore it. The driver half drives the real facade, with judges whose verdict
//! is a function of how many sessions' work is in the tree.
//!
//! Unix-only (the fake agent is a shell script), like the rest of the suite.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use agg::core::verdicts::{rows_for, Outcome};
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

fn commit_base(dir: &Path) {
    write(dir, ".gitignore", "agg/state/\nagg/private/\n.obsidian/\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
}

/// `agg.yaml` with one judged `worker` step and `seq` as the whole body under `sequence:`.
fn write_cfg(dir: &Path, seq: &str) {
    write(
        dir,
        "agg/agg.yaml",
        &format!(
            "project: groups\ndefaults: {{ model: fake }}\nsteps:\n  worker: {{}}\n\
             sequence:\n  steps: [worker]\n{seq}summary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"
        ),
    );
}

/// Judges `names`, each met iff its letter is in `state.txt`.
fn letter_judges(dir: &Path, names: &[&str]) {
    for j in names {
        write_judge(dir, j, &format!("#!/bin/sh\ngrep -q {j} state.txt && echo '{{\"met\":true}}' || echo '{{\"met\":false}}'\n"));
    }
}

fn run(dir: &Path, path: &str, sessions: &str) -> (Option<i32>, String) {
    let out = Command::cargo_bin("agg")
        .unwrap()
        .current_dir(dir)
        .env("PATH", path)
        .env("HOME", dir.join(".home"))
        .args(["run", "--max-sessions", sessions])
        .output()
        .unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code(), combined)
}

/// Per session, the outcome the gate stamped on `judge`'s row.
fn outcomes(dir: &Path, judge: &str) -> Vec<(Option<u32>, Outcome)> {
    rows_for(dir, judge, false).into_iter().map(|r| (r.session, r.outcome)).collect()
}

/// THE A-4 ACCEPTANCE ROW: `done_if: "count_met(qs) >= 2"` over three judges does not pass with one
/// met (session 1) and passes with two (session 2).
#[test]
fn k_of_n_passes_with_two_of_three_and_not_with_one() {
    let (_tmp, dir, path) = planned_project(&["echo a > state.txt", "echo ab > state.txt", "echo abc > state.txt"]);
    write(&dir, "state.txt", "\n");
    letter_judges(&dir, &["a", "b", "c"]);
    write_cfg(&dir, "  groups:\n    qs: [\"a\", \"b\", \"c\"]\n  done_if: \"count_met(qs) >= 2\"\n");
    commit_base(&dir);

    let (code, combined) = run(&dir, &path, "5");
    assert_eq!(code, Some(0), "done on two of three:\n{combined}");
    assert_eq!(
        outcomes(&dir, "c"),
        vec![(None, Outcome::Baseline), (Some(1), Outcome::Merged), (Some(2), Outcome::Merged)],
        "one of three did not end the run; two did — and every member ran each session:\n{combined}"
    );
}

/// The gate's GROUP rule: a member swap inside a `done_if` group (a lost, b gained) is not a
/// regression and is kept; a net loss is rolled back. The same plan with the members named one by
/// one rolls the swap back — that difference is the rule.
#[test]
fn a_swap_inside_a_group_is_kept_and_a_net_loss_is_rolled_back() {
    let plan = ["echo b > state.txt", "echo x > state.txt"];
    let (_tmp, dir, path) = planned_project(&plan);
    write(&dir, "state.txt", "a\n");
    letter_judges(&dir, &["a", "b", "c"]);
    write_cfg(&dir, "  groups:\n    qs: [\"a\", \"b\", \"c\"]\n  done_if: \"count_met(qs) >= 3\"\n");
    commit_base(&dir);

    let (code, combined) = run(&dir, &path, "2");
    assert_eq!(code, Some(4), "{combined}");
    assert_eq!(
        outcomes(&dir, "b"),
        vec![(None, Outcome::Baseline), (Some(1), Outcome::Merged), (Some(2), Outcome::RolledBack)],
        "{combined}"
    );
    assert_eq!(git(&dir, &["show", "main:state.txt"]), "b", "the swap landed, the loss did not");

    // the contrast: the same members, named one by one, are each held to the per-judge rule.
    let (_tmp2, dir2, path2) = planned_project(&plan);
    write(&dir2, "state.txt", "a\n");
    letter_judges(&dir2, &["a", "b", "c"]);
    write_cfg(&dir2, "  done_if: \"a AND b AND c\"\n");
    commit_base(&dir2);
    let (_, combined) = run(&dir2, &path2, "1");
    assert_eq!(outcomes(&dir2, "b")[1], (Some(1), Outcome::RolledBack), "a regressed on its own:\n{combined}");
}

/// A member named ONLY in a group still runs, and an `abort_if` over a group reads it.
#[test]
fn a_group_in_abort_if_aborts_on_its_count() {
    let (_tmp, dir, path) = planned_project(&["echo c > state.txt", "echo cd > state.txt"]);
    write(&dir, "state.txt", "\n");
    letter_judges(&dir, &["goal", "c", "d"]);
    write_cfg(&dir, "  groups:\n    alarms: [\"c\", \"d\"]\n  done_if: \"goal\"\n  abort_if: \"all_goals(alarms)\"\n");
    commit_base(&dir);

    let (code, combined) = run(&dir, &path, "5");
    assert_eq!(code, Some(3), "abort once both alarms are met:\n{combined}");
    assert_eq!(outcomes(&dir, "d").len(), 3, "baseline + two sessions:\n{combined}");
}

/// An unknown member and an unknown group are startup errors that LIST the valid names.
#[test]
fn an_unknown_member_or_group_refuses_at_startup_listing_the_valid_names() {
    let (_tmp, dir, path) = planned_project(&["echo nope > touched.txt"]);
    letter_judges(&dir, &["a", "b"]);
    write_cfg(&dir, "  groups:\n    qs: [\"a\", \"ghost\"]\n  done_if: \"count_met(qs) >= 1\"\n");
    commit_base(&dir);
    let (code, combined) = run(&dir, &path, "1");
    assert_eq!(code, Some(1), "{combined}");
    assert!(combined.contains("ghost") && combined.contains("available judges") && combined.contains("a, b"), "{combined}");

    write_cfg(&dir, "  groups:\n    qs: [\"a\", \"b\"]\n  done_if: \"count_met(quality) >= 1\"\n");
    let (code, combined) = run(&dir, &path, "1");
    assert_eq!(code, Some(1), "{combined}");
    assert!(combined.contains("unknown group `quality`") && combined.contains("defined groups: qs"), "{combined}");

    write_cfg(&dir, "  groups:\n    qs: [\"a\", \"b\"]\n  done_if: \"count_met(qs) >= 3\"\n");
    let (code, combined) = run(&dir, &path, "1");
    assert_eq!(code, Some(1), "{combined}");
    assert!(combined.contains("can never hold"), "k > n: {combined}");
    assert!(!dir.join("touched.txt").exists(), "no worker session ran");
}

// ── the Rust driver ────────────────────────────────────────────────────────────────────────────

/// A fake `claude` that appends one line to `work.log` per worker session and commits it.
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-groups-env-{}", std::process::id()));
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

/// A driver project with three judges over `n` = the number of sessions in the tree: `even` (met
/// iff n is even), `odd` (met iff n is odd) and `any` (met iff n >= 1).
fn driver_project() -> (tempfile::TempDir, Judge, Judge, Judge) {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(tmp.path(), args);
    }
    git(tmp.path(), &["commit", "-q", "--allow-empty", "-m", "agg baseline"]);
    let n = "n=$(cat work.log 2>/dev/null | wc -l | tr -d ' ')";
    for (name, test) in [("even", "[ $((n % 2)) -eq 0 ]"), ("odd", "[ $((n % 2)) -eq 1 ]"), ("any", "[ \"$n\" -ge 1 ]")] {
        write_judge(
            tmp.path(),
            name,
            &format!("#!/bin/sh\n{n}\n{test} && printf '{{\"met\":true,\"value\":%s}}\\n' \"$n\" || printf '{{\"met\":false,\"value\":%s}}\\n' \"$n\"\n"),
        );
    }
    let j = |n: &str| Judge::script(n, format!("agg/judges/{n}.sh"));
    (tmp, j("even"), j("odd"), j("any"))
}

fn work() -> Step {
    Step::new("implement").model("fake").prompt("do a chunk")
}

/// `agg.group()` counts k-of-n, and at the gate a member asked ONLY through the group is judged with
/// it, net: the even/odd swap of the second session is kept. Asked by name, the same swap is a
/// per-judge regression and is rolled back.
#[test]
fn a_driver_group_counts_k_of_n_and_its_gate_judges_the_group_net() {
    let (tmp, even, odd, any) = driver_project();
    let q = Group::new("q", [even.clone(), odd.clone(), any.clone()]);
    let agg = Agg::open(tmp.path()).unwrap();
    let before = agg.group(&q);
    assert_eq!((before.count_met(), before.total()), (1, 3), "n=0: only `even`");
    assert!(!before.at_least(2) && before.any() && !before.all());
    agg.step(&work()).unwrap();
    assert!(agg.group(&q).at_least(2), "n=1: odd + any");
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);
    agg.step(&work()).unwrap();
    let after = agg.group(&q);
    assert_eq!(after.count_met(), 2, "n=2: even + any — odd lost, even gained");
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept, "a swap inside a group is not a regression");
    drop(agg);

    let (tmp, even, odd, _) = driver_project();
    let agg = Agg::open(tmp.path()).unwrap();
    agg.step(&work()).unwrap();
    agg.judge(&even);
    agg.judge(&odd);
    assert_eq!(agg.gate().unwrap(), GateOutcome::Kept);
    agg.step(&work()).unwrap();
    agg.judge(&even);
    agg.judge(&odd);
    assert_eq!(agg.gate().unwrap(), GateOutcome::RolledBack, "asked by name, `odd` regressed on its own");
}
