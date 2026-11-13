//! `agg process` (A-37) against a real project: the example process (`examples/process/`), a git repo, a fake `claude`
//! worker and a fake `codex` critic on PATH. Each test runs the `agg` binary and reads what the run produced: the step
//! history, the work on `main`, the files the scripts and hooks wrote.
//!
//! Unix-only (the fake agents are shell scripts), like the rest of the suite.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::LazyLock;

/// The fake agents' `bin/` and a throwaway HOME, shared by the tests (each test's own project is apart).
static ENV: LazyLock<(PathBuf, PathBuf)> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-process-env-{}", std::process::id()));
    let (bin, home) = (root.join("bin"), root.join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    // The worker: one line in work.log per session, unless the project says it is idle; a task force session writes
    // its proposal to the file its brief names (from the project's task-force.answer).
    write_exe(&bin.join("claude"), r#"#!/bin/sh
worker=
for a in "$@"; do
  case "$a" in
    --version) echo "fake-claude 0.0.0"; exit 0 ;;
    *INSTRUCTIONS.md*) worker=1 ;;
  esac
done
if [ -n "$worker" ]; then cat agg/private/INSTRUCTIONS.md >> .git/agg-worker.args; fi
answer=$(grep -o '/[^ ]*task-force\.yaml' agg/private/INSTRUCTIONS.md 2>/dev/null | tail -1)
if [ -n "$answer" ] && [ -f task-force.answer ]; then cp task-force.answer "$answer"; [ -f .git/agg-keep-answer ] || rm task-force.answer; fi
# a task force session repairs the cause (the workers are no longer idle) unless the project says it is stubborn
if grep -q '^## The failure' agg/private/INSTRUCTIONS.md 2>/dev/null && [ ! -f .git/agg-stubborn ]; then rm -f .git/agg-idle; fi
# an exam's repairer disputes a finding when the project says so
dispute=$(grep -o 'The disputes file: `[^`]*`' agg/private/INSTRUCTIONS.md 2>/dev/null | tail -1 | cut -d'`' -f2)
if [ -n "$dispute" ] && [ -f .git/agg-dispute ]; then echo "t1 is wrong" > "$dispute"; fi
if [ -n "$worker" ] && [ ! -f .git/agg-idle ] && [ -z "$dispute" -o ! -f .git/agg-dispute ]; then
  echo "session $$" >> work.log
  git add -A >/dev/null 2>&1
  git commit -qm "worker: did work" >/dev/null 2>&1
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"fake-'$$'","usage":{"output_tokens":1},"total_cost_usd":0.01}'
"#);
    // The critic: met when the project has `critic.ok`; every call's prompt goes to .git/critic.calls.
    write_exe(&bin.join("codex"), r#"#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && { echo "fake-codex 0.0.0"; exit 0; }; done
cat >> .git/critic.calls
echo "---call" >> .git/critic.calls
if [ -f critic.ok ]; then v='{\"met\": true, \"rationale\": \"fine\"}'; else v='{\"met\": false, \"rationale\": \"fix A\"}'; fi
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"'"$v"'"}}'
"#);
    (bin, home)
});

fn write_exe(p: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The example process's file.
fn process_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/process/process.yaml")
}

/// A project: a git repo on `main` with its roles, its task, the project's settings (the checkpoints approved by the
/// run itself) and a critic that is met.
fn project(settings: &str) -> tempfile::TempDir {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    project_in(tmp.path(), settings);
    tmp
}

/// The project's files in `d`.
fn project_in(d: &Path, settings: &str) {
    std::fs::create_dir_all(d).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(d, args);
    }
    std::fs::write(d.join("tools.yaml"), "worker: { agent: claude, model: fake }\ncritic: { agent: codex }\n").unwrap();
    std::fs::write(d.join("task.md"), "Build the thing.\n").unwrap();
    std::fs::write(d.join("settings.yaml"), settings).unwrap();
    std::fs::write(d.join("critic.ok"), "").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "baseline"]);
}

/// `agg process <example> <project> <args>`.
fn run(dir: &Path, args: &[&str]) -> Output {
    let (bin, home) = &*ENV;
    Command::new(env!("CARGO_BIN_EXE_agg")).arg("process").arg(process_file()).arg(dir).args(args).current_dir(dir)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())).env("HOME", home)
        .output().unwrap()
}

fn said(out: &Output) -> String {
    format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// The history's entries, oldest first.
fn history(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("reports/steps.jsonl")).unwrap_or_default().lines()
        .filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The (label, kind) of every entry that isn't a hook, in order.
fn steps(dir: &Path) -> Vec<(String, String)> {
    history(dir).iter().filter(|e| e["kind"] != "hook")
        .map(|e| (e["step"].as_str().unwrap_or_default().to_string(), e["kind"].as_str().unwrap_or_default().to_string())).collect()
}


const AUTO: &str = "auto_approve: [cp-survey, cp-build]\n";

/// The process runs in file order, its steps numbered from position: scripts, an agent judged by checks and a critic,
/// checkpoints approved by the project's setting, an `each` over the plan's units with its last-item check, a step whose
/// condition is unmet and one the settings skip; the phase hooks run around the phase.
#[test]
fn a_process_runs_in_file_order_with_numbers_from_position() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let out = run(d, &[]);
    assert!(out.status.success(), "{}", said(&out));
    let order: Vec<String> = steps(d).into_iter().map(|(s, k)| format!("{s}|{k}")).collect();
    let want = ["1.2 Sources|tool", "1.3 Survey|done", "1.4 CP1 Survey approved document|tool", "1.4 CP1 Survey approved|auto-approval",
                "2.1-U1 Construct|done", "2.2-U1 Accept|tool", "2.1-U2 Construct|done", "2.2-U2 Accept|tool",
                "2.3 Lint|skipped", "2.4 CP2 Build approved|auto-approval",
                "3.1 Package|done"];
    let at = |label: &str| order.iter().position(|e| e.starts_with(label)).unwrap_or_else(|| panic!("{label} not in {order:#?}"));
    for pair in want.windows(2) {
        assert!(at(pair[0]) < at(pair[1]), "{} before {}\n{order:#?}", pair[0], pair[1]);
    }
    assert!(order.contains(&"3.3 Extras|skipped".to_string()), "the settings skip it: {order:#?}");
    let ids: Vec<(String, String)> = history(d).iter().filter(|e| e["kind"] == "done")
        .map(|e| (e["step"].as_str().unwrap().to_string(), e["id"].as_str().unwrap().to_string())).collect();
    assert!(ids.contains(&("2.1-U2 Construct".into(), "construct-U2".into())), "an instance's id: {ids:?}");
    assert!(ids.contains(&("1.3 Survey".into(), "survey".into())));
    assert_eq!(std::fs::read_to_string(d.join("accepted.log")).unwrap(), "U1\nU2\n");
    let doc = read(d, "checkpoints/build.md");
    assert!(doc.contains("U1 since refs/agg/before/construct-U1") && doc.contains("U2 since refs/agg/before/construct-U2"),
            "a sibling step's ref is `{{{{ before.<id> }}}}`: {doc}");
    assert!(!steps(d).iter().any(|(s, _)| s == "2.4 CP2 Build approved document"), "a checkpoint without `prepare` has no document step");
    let phases = std::fs::read_to_string(d.join(".git/phase-hooks.log")).unwrap();
    assert!(phases.lines().next().unwrap().ends_with("before") && phases.lines().last().unwrap().ends_with("after"), "{phases}");
    assert!(!std::fs::read_to_string(d.join(".git/step-hooks.log")).unwrap().is_empty(), "the step hook ran");
    let declared: Vec<String> = history(d).iter().filter(|e| e["kind"] == "hook" && e["step"] == "1.3 Survey")
        .map(|e| e["rationale"].as_str().unwrap_or_default().to_string()).filter(|r| r.contains("worked")).collect();
    assert_eq!(declared.len(), 1, "a declared judge named in the hooks file ran after the step: {:?}", history(d));
    assert!(declared[0].starts_with("after worked"), "{declared:?}");
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

fn touch(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), "").unwrap();
}

fn labels(dir: &Path) -> Vec<String> {
    steps(dir).into_iter().map(|(s, _)| s).collect()
}

/// `--from` and `--until` take an id, a number, an instance key or an instance's number; a name no step has halts.
#[test]
fn from_and_until_take_an_id_a_number_or_an_instance_key() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let out = run(d, &["--until", "survey"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(labels(d).contains(&"1.3 Survey".to_string()) && !labels(d).iter().any(|l| l.contains("CP1")), "{:?}", labels(d));
    // U1 by its key's end, then U2 by its instance number: only that unit's steps run each time
    let out = run(d, &["--from", "2.1-U1", "--until", "accept-U1"]);
    assert!(out.status.success(), "{}", said(&out));
    let out = run(d, &["--from", "2.1-U2", "--until", "accept-U2"]);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(read(d, "accepted.log"), "U1\nU2\n");
    assert_eq!(labels(d).iter().filter(|l| l.starts_with("2.1-U1 Construct")).count(), 1, "U1 ran once, in the first run");
    assert!(labels(d).contains(&"2.1-U2 Construct".to_string()));
    let out = run(d, &["--from", "bogus"]);
    assert!(!out.status.success() && said(&out).contains("names no step"), "{}", said(&out));
    let out = run(d, &["--until", "1.3"]);
    assert!(out.status.success(), "a number names its step too: {}", said(&out));
}

/// The run's configuration switches off a step or a whole phase, with the reason in the record; a project's settings can
/// switch on a step the process skips; a step whose condition is met runs.
#[test]
fn skips_come_from_the_run_configuration_and_the_settings_and_a_condition_decides() {
    let tmp = project("auto_approve: [cp-survey, cp-build]\nskip: []\n");
    let d = tmp.path();
    std::fs::write(d.join("run.yaml"), "switched_off:\n  survey: built in the first increment\n  ship: the whole phase\n").unwrap();
    touch(d, "lint.enabled");
    let out = run(d, &[]);
    assert!(out.status.success(), "{}", said(&out));
    let h = history(d);
    let skipped = |label: &str| h.iter().find(|e| e["step"] == label && e["kind"] == "skipped").map(|e| e["rationale"].as_str().unwrap().to_string());
    assert_eq!(skipped("1.3 Survey").as_deref(), Some("switched off by the run's configuration (run.yaml): built in the first increment"));
    assert!(skipped("3.1 Package").is_some() && skipped("3.2 Docs exam").is_some(), "a phase is switched off as a whole");
    assert!(labels(d).contains(&"2.3 Lint".to_string()) && skipped("2.3 Lint").is_none(), "its condition is met");
    assert!(steps(d).contains(&("2.1-U1 Construct".to_string(), "done".to_string())));
}

/// A checkpoint asks a human; the decision they write in the yes goes back to the step that wrote the document, which
/// carries it; the approval then stands, the document being the same.
#[test]
fn a_checkpoint_asks_a_human_and_a_decision_in_the_yes_goes_back_to_its_owner() {
    let tmp = project("");
    let d = tmp.path();
    let dir = d.to_path_buf();
    let human = std::thread::spawn(move || {
        for _ in 0..2400 {
            if let Some(a) = agg::core::asks::open(&dir).into_iter().next() {
                std::fs::create_dir_all(dir.join("checkpoints")).unwrap();
                std::fs::write(dir.join("checkpoints/cp-survey-decisions.md"), "- keep the survey short\n").unwrap();
                agg::core::asks::answer(&dir, &a.id, "yes", "test", agg::util::now_epoch()).unwrap();
                return a.question;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no ask appeared");
    });
    let out = run(d, &["--until", "cp-survey"]);
    let question = human.join().unwrap_or_else(|_| panic!("no ask appeared; the run said:\n{}", said(&out)));
    assert!(out.status.success(), "{}", said(&out));
    assert!(question.starts_with("CP1: approve 1.3 Survey?"), "{question}");
    let h = history(d);
    assert!(h.iter().any(|e| e["kind"] == "reentry" && e["step"] == "1.3 Survey"
        && e["rationale"].as_str().unwrap().contains("keep the survey short")), "{h:#?}");
    let briefs = read(d, ".git/agg-worker.args");
    assert!(briefs.contains("## Since your last result") && briefs.contains("+- keep the survey short"),
            "the survey's refresh shows the decision in what it reads: {briefs}");
    assert_eq!(h.iter().filter(|e| e["kind"] == "approval" && e["step"] == "1.4 CP1 Survey approved").count(), 2,
               "asked once, then standing: {h:#?}");
}

/// A failed step goes to the task force, which repairs the cause in place; the step is judged again and met, and the
/// task force's brief names the step's rubric.
#[test]
fn the_task_force_repairs_a_failed_step_in_place() {
    let tmp = project(AUTO);
    let d = tmp.path();
    touch(d, ".git/agg-idle");
    std::fs::write(d.join("task-force.answer"), "outcome: fixed\ncause: the workers were idle\nnote: woke them\n").unwrap();
    let out = run(d, &["--until", "survey"]);
    assert!(out.status.success(), "{}", said(&out));
    let h = history(d);
    let fixed = h.iter().find(|e| e["kind"] == "task-force" && e["met"] == true).unwrap_or_else(|| panic!("{h:#?}"));
    assert!(fixed["rationale"].as_str().unwrap().starts_with("fixed in place"), "{fixed}");
    assert_eq!(fixed["id"], "survey");
    let briefs = read(d, ".git/agg-worker.args");
    assert!(briefs.contains("## What step 1.3 Survey must keep to") && briefs.contains("judges/survey-review.md"), "the rubric is in the task force's brief");
    assert!(briefs.contains("the app lives in `app/`"), "the process's own section is in it");
    assert!(steps(d).contains(&("1.3 Survey".to_string(), "done".to_string())));
}

/// A failure the task force can't mend uses up its repairs for that step, and the run exits 6: a supervisor can't take it
/// for a halt a restart mends.
#[test]
fn a_spent_limit_exits_6() {
    let tmp = project(AUTO);
    let d = tmp.path();
    touch(d, ".git/agg-idle");
    touch(d, ".git/agg-stubborn");
    touch(d, ".git/agg-keep-answer");
    std::fs::write(d.join("task-force.answer"), "outcome: fixed\ncause: guessed\nnote: changed x\n").unwrap();
    let out = run(d, &["--until", "survey"]);
    assert_eq!(out.status.code(), Some(6), "{}", said(&out));
    let h = history(d);
    let spent = h.iter().rfind(|e| e["kind"] == "task-force" && e["met"] == false).unwrap();
    assert!(spent["rationale"].as_str().unwrap().starts_with("spent:"), "{spent}");
    assert!(said(&out).contains("are spent"), "{}", said(&out));
}

/// A proposal the process trusts (`task_force.trust`: a failure in a phase sent back to a step) goes ahead without a
/// human; the process starts again at that step with the note, and the task force's brief carries the unit's constraints.
#[test]
fn a_trusted_proposal_re_enters_without_asking() {
    let tmp = project(AUTO);
    let d = tmp.path();
    std::fs::write(d.join("accepted.log"), "x\nx\nx\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "three units accepted already"]);
    std::fs::write(d.join("task-force.answer"), "reenter: survey\ncause: the survey missed a unit\nnote: list every unit\n").unwrap();
    let out = run(d, &["--until", "accept-U1"]);
    assert!(out.status.success(), "{}", said(&out));
    let h = history(d);
    let tf = h.iter().find(|e| e["kind"] == "task-force" && e["met"] == true).unwrap_or_else(|| panic!("{h:#?}"));
    assert!(tf["rationale"].as_str().unwrap().contains("a proposal the driver trusts, no ask"), "{tf}");
    assert_eq!(tf["id"], "construct-U1");
    let briefs = read(d, ".git/agg-worker.args");
    assert!(briefs.contains("## Note from a later step") && briefs.contains("list every unit"), "the survey got the note");
    assert!(briefs.contains("Only files matching app/a/**"), "the failed unit's constraints are in the task force's brief");
    assert!(steps(d).contains(&("2.2-U1 Accept".to_string(), "tool".to_string())), "the run went on after the re-entry");
}

/// A ladder's tool tier that applies no tool is recorded as `n/a`; the next tier does the step.
#[test]
fn a_tool_tier_that_applies_nothing_is_not_an_attempt() {
    let tmp = project("auto_approve: [cp-survey, cp-build]\nsteps:\n  construct: { ladder: [{ tool: codemod }, { mode: once }] }\n");
    let d = tmp.path();
    let out = run(d, &["--until", "accept-U1"]);
    assert!(out.status.success(), "{}", said(&out));
    let h = history(d);
    let of = |kind: &str| h.iter().filter(|e| e["step"] == "2.1-U1 Construct" && e["kind"] == kind).count();
    assert_eq!((of("n/a"), of("tier"), of("done")), (1, 1, 1), "{h:#?}");
    let na = h.iter().find(|e| e["kind"] == "n/a").unwrap();
    assert!(na["rationale"].as_str().unwrap().starts_with("tier 1 of 2 n/a: codemod"), "{na}");
}

/// The brief of an agent step in an `each` holds its item and the house rules, in the order the process says.
#[test]
fn a_units_brief_holds_its_item_and_the_house_rules() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let out = run(d, &["--until", "accept-U1"]);
    assert!(out.status.success(), "{}", said(&out));
    let briefs = read(d, ".git/agg-worker.args");
    let unit = briefs.rsplit("# Session ").next().unwrap();
    assert!(unit.contains("## Your unit") && unit.contains("id: U1") && unit.contains("note: first"), "{unit}");
    assert!(unit.contains("You are working on 2.1-U1 Construct (step id `construct`)"), "{unit}");
    assert!(unit.contains("## Your input files") && unit.contains("Build the thing."), "the brief's small inputs: {unit}");
    assert!(unit.contains("## Check your result"), "{unit}");
}

/// The exam: a repair round that meets the checks passes; a round that finds what the one before found fails the step.
#[test]
fn an_exam_passes_after_a_repair_and_fails_on_a_stall() {
    let exam = |work: usize| {
        let tmp = project(AUTO);
        let d = tmp.path();
        std::fs::create_dir_all(d.join("exam")).unwrap();
        std::fs::write(d.join("exam/exam.yaml"), "tasks:\n  - { id: t1, task: x }\n").unwrap();
        std::fs::write(d.join("work.log"), "w\n".repeat(work)).unwrap();
        git(d, &["add", "-A"]);
        git(d, &["commit", "-qm", "exam"]);
        let out = run(d, &["--from", "docs-exam", "--until", "docs-exam"]);
        (tmp, out)
    };
    let (tmp, out) = exam(2);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(kinds_in(tmp.path(), "3.2 Docs exam"), ["repair", "done"], "{}", said(&out));
    let (tmp, out) = exam(0);
    assert!(!out.status.success() && said(&out).contains("the repair didn't get past this (round 2)"), "{}", said(&out));
    assert!(kinds_in(tmp.path(), "3.2 Docs exam").contains(&"repair".to_string()));
}

fn kinds_in(dir: &Path, label: &str) -> Vec<String> {
    steps(dir).into_iter().filter(|(s, _)| s == label).map(|(_, k)| k).collect()
}

/// A repairer that disputes the exam sends the run back to the step that wrote it, once; a second dispute fails the step.
#[test]
fn a_dispute_re_enters_the_exams_writer_once() {
    let tmp = project(AUTO);
    let d = tmp.path();
    std::fs::create_dir_all(d.join("exam")).unwrap();
    std::fs::write(d.join("exam/exam.yaml"), "tasks:\n  - { id: t1, task: x }\n").unwrap();
    std::fs::write(d.join("work.log"), "w\n".repeat(3)).unwrap();
    touch(d, "exam.fails");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "exam"]);
    touch(d, ".git/agg-dispute");
    let out = run(d, &["--from", "docs-exam", "--until", "docs-exam"]);
    assert!(!out.status.success() && said(&out).contains("after `package` wrote the exam again"), "{}", said(&out));
    let h = history(d);
    assert!(h.iter().any(|e| e["kind"] == "reentry" && e["step"] == "3.1 Package"), "{h:#?}");
}

/// A step that ends the run unmet runs its `on_fail` command first.
#[test]
fn on_fail_runs_when_the_step_ends_the_run_unmet() {
    let tmp = project(AUTO);
    let d = tmp.path();
    touch(d, ".git/agg-idle");
    let out = run(d, &["--from", "package", "--until", "package"]);
    assert!(!out.status.success(), "{}", said(&out));
    assert_eq!(read(d, ".git/on-fail.log"), "package failed\n");
    assert!(said(&out).contains("on_fail: cleaned up after package"), "{}", said(&out));
}

/// `--check` prints a verdict JSON and exits 1 on a problem; a process with a problem isn't run; `--list` prints the
/// numbered steps.
#[test]
fn check_and_list_read_only_the_file_and_a_bad_process_is_not_run() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let bad = tempfile::tempdir().unwrap();
    std::fs::write(bad.path().join("process.yaml"), "version: 1\nphases:\n- id: p\n  name: P\n  steps:\n  - { id: s, name: S, kind: script, run: nowhere/x.sh }\n").unwrap();
    let agg = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_agg")).arg("process").args(args).current_dir(d).output().unwrap();
    let checked = agg(&[bad.path().join("process.yaml").to_str().unwrap(), "--check"]);
    assert_eq!(checked.status.code(), Some(1));
    let verdict: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&checked.stdout).lines().last().unwrap()).unwrap();
    assert_eq!(verdict["met"], false);
    assert!(verdict["rationale"].as_str().unwrap().contains("nowhere/x.sh"), "{verdict}");
    let ran = agg(&[bad.path().join("process.yaml").to_str().unwrap(), d.to_str().unwrap()]);
    assert!(!ran.status.success() && said(&ran).contains("1 problem(s)"), "{}", said(&ran));
    let listed = agg(&[process_file().to_str().unwrap(), "--list"]);
    let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let labels: Vec<&str> = list.as_array().unwrap().iter().filter_map(|e| e["label"].as_str()).collect();
    assert!(labels.contains(&"1.4 CP1 Survey approved") && labels.contains(&"2.4 CP2 Build approved") && labels.contains(&"3.4 Handover"), "{labels:?}");
    let ok = agg(&[process_file().to_str().unwrap(), "--check"]);
    assert!(ok.status.success(), "{}", said(&ok));
}

// ---- A-38: what every process run needs -----------------------------------------------------------------------

/// A process home with a script-only process and its scripts (a script step prints a verdict JSON, as a judge does): `write` makes `out/result.txt` (and a script under
/// `scripts-out/` when the project has `.bad-script`, or no output when it has `.no-output`), `drop` removes the
/// result when the project has `.drop`, `read` reads it.
fn mini_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(d.join("process.yaml"), "version: 1\nrun:\n  history: reports/steps.jsonl\n  declared: true\n  scripts_runnable: [scripts-out]\n  \
        commit_inputs: { branch: main, message: \"Inputs at run start\" }\nphases:\n- id: one\n  name: One\n  steps:\n  \
        - { id: write, name: Write, kind: script, run: scripts/write.sh, settings: { outputs: [out/result.txt] } }\n  \
        - { id: drop, name: Drop, kind: script, run: scripts/drop.sh }\n  \
        - { id: read, name: Read, kind: script, run: scripts/read.sh, settings: { inputs: [out/result.txt] } }\n").unwrap();
    for (name, body) in [
        ("write.sh", "mkdir -p out scripts-out\n[ -f .no-output ] || echo result > out/result.txt\n[ -f .bad-script ] && printf '#!/bin/sh\\necho x\\n' > scripts-out/bad.sh\necho '{\"met\": true, \"rationale\": \"written\"}'\n"),
        ("drop.sh", "[ -f .drop ] && rm -f out/result.txt\necho '{\"met\": true, \"rationale\": \"dropped\"}'\n"),
        ("read.sh", "cat out/result.txt > read.log\necho '{\"met\": true, \"rationale\": \"read\"}'\n"),
    ] {
        write_exe(&d.join("scripts").join(name), &format!("#!/bin/sh\n{body}"));
    }
    tmp
}

/// `agg process <home>/process.yaml <project>`.
fn run_mini(home: &Path, dir: &Path) -> Output {
    let (bin, h) = &*ENV;
    Command::new(env!("CARGO_BIN_EXE_agg")).arg("process").arg(home.join("process.yaml")).arg(dir).current_dir(dir)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())).env("HOME", h).output().unwrap()
}

/// The rationale of the history's entries of `kind` for the step labelled `label`.
fn rationales(dir: &Path, kind: &str, label: &str) -> Vec<String> {
    history(dir).iter().filter(|e| e["kind"] == kind && e["step"] == label)
        .map(|e| e["rationale"].as_str().unwrap_or_default().to_string()).collect()
}

/// `run.commit_inputs` commits what a human filled in before the run, on `main`; the run's first entry says which
/// process ran; `run.declared` checks the declared output after `write` and the input before `read`.
#[test]
fn the_inputs_are_committed_and_declared_files_are_checked() {
    let (home, tmp) = (mini_home(), project(""));
    let d = tmp.path();
    git(d, &["checkout", "-q", "-b", "agg/session-left-over"]);
    std::fs::write(d.join("filled-in.md"), "an input").unwrap();
    let out = run_mini(home.path(), d);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(git(d, &["log", "--format=%s", "--", "filled-in.md"]), "Inputs at run start",
               "the file a human filled in came in with the inputs commit: {}", git(d, &["log", "--oneline"]));
    assert_eq!(git(d, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the left-over session branch was left");
    assert!(said(&out).contains("process home not in git"), "{}", said(&out));
    let first = &history(d)[0];
    assert_eq!((first["kind"].as_str(), first["rationale"].as_str()), (Some("start"), Some("process home not in git")));
    assert_eq!(rationales(d, "hook", "1.1 Write").iter().filter(|r| r.contains("every declared output exists")).count(), 1);
    assert_eq!(rationales(d, "hook", "1.3 Read").iter().filter(|r| r.contains("every input an earlier step wrote exists")).count(), 1);
    assert!(std::fs::read_to_string(d.join("read.log")).unwrap().contains("result"));
}

/// A step that doesn't write what it declares fails after it, naming the file and the advice.
#[test]
fn a_declared_output_that_is_missing_fails_the_step() {
    let (home, tmp) = (mini_home(), project(""));
    let d = tmp.path();
    std::fs::write(d.join(".no-output"), "").unwrap();
    let out = run_mini(home.path(), d);
    assert!(!out.status.success(), "{}", said(&out));
    let why = rationales(d, "hook", "1.1 Write").join("\n");
    assert!(why.contains("declared outputs missing: out/result.txt; write them"), "{why}");
}

/// An input an earlier step of this run declares as its output must still exist when a later step starts.
#[test]
fn an_input_an_earlier_step_wrote_that_is_gone_fails_the_step_before_it_runs() {
    let (home, tmp) = (mini_home(), project(""));
    let d = tmp.path();
    std::fs::write(d.join(".drop"), "").unwrap();
    let out = run_mini(home.path(), d);
    assert!(!out.status.success(), "{}", said(&out));
    let why = rationales(d, "hook", "1.3 Read").join("\n");
    assert!(why.contains("inputs an earlier step wrote are missing: out/result.txt"), "{why}");
    assert!(!d.join("read.log").exists(), "the step never started");
}

/// A script a step wrote under `run.scripts_runnable` must be able to run: a `#!` file executable.
#[test]
fn a_script_that_cannot_run_fails_the_step_that_wrote_it() {
    let (home, tmp) = (mini_home(), project(""));
    let d = tmp.path();
    std::fs::write(d.join(".bad-script"), "").unwrap();
    let out = run_mini(home.path(), d);
    assert!(!out.status.success(), "{}", said(&out));
    let why = rationales(d, "hook", "1.1 Write").join("\n");
    assert!(why.contains("scripts-out/bad.sh: starts with #! but isn't executable (chmod +x)"), "{why}");
}

// ---- A-38: scenario mode ---------------------------------------------------------------------------------------

/// A projects folder with the example's project `app`, and the scenario files.
fn scenario_folder(scenarios: &[(&str, &str)]) -> tempfile::TempDir {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    project_in(&tmp.path().join("projects/app"), "");
    for (name, text) in scenarios {
        std::fs::write(tmp.path().join(name), text).unwrap();
    }
    tmp
}

/// `agg process <example> --scenario <files> --projects <dir>/projects`.
fn play(dir: &Path, files: &[&str]) -> Output {
    let (_, home) = &*ENV;
    Command::new(env!("CARGO_BIN_EXE_agg")).arg("process").arg(process_file()).arg("--scenario")
        .args(files.iter().map(|f| dir.join(f))).arg("--projects").arg(dir.join("projects")).current_dir(dir)
        .env("HOME", home).output().unwrap()
}

/// A scenario plays the example's first steps on a copy of a project with scripted agents: the worker writes what its rule
/// says, the critic's judges are met by default, and the record and the calls are checked against what it expects.
const PASSES: &str = "project: app\nargs: [--until, survey]\nagents:\n  - { when: 'Survey', write: { work.log: \"session\\n\", survey.md: surveyed } }\n\
    expect:\n  record: ['^survey done met']\n  sessions: ['^claude worker', '^codex judge']\n  never: ['task-force']\n  files: { survey.md: 'surveyed' }\n  exit: 0\n";
/// The same run, expected to show what it doesn't, and a checkpoint's ask nobody foresaw.
const FAILS: &str = "project: app\nargs: [--until, cp-survey]\nagents:\n  - { when: 'Survey', write: { work.log: \"session\\n\" } }\n\
    expect:\n  record: ['^survey done met', '^never-happens']\n  exit: 0\n";

#[test]
fn a_scenario_plays_a_process_with_scripted_agents_and_checks_what_it_shows() {
    let tmp = scenario_folder(&[("passes.yaml", PASSES), ("fails.yaml", FAILS)]);
    let ok = play(tmp.path(), &["passes.yaml"]);
    assert!(ok.status.success(), "{}", said(&ok));
    let out = said(&ok);
    assert!(out.contains("PASS passes") && out.contains("\"met\":true"), "{out}");
    let bad = play(tmp.path(), &["passes.yaml", "fails.yaml"]);
    assert_eq!(bad.status.code(), Some(1), "{}", said(&bad));
    let out = said(&bad);
    assert!(out.contains("PASS passes") && out.contains("FAIL fails: unforeseen ask: "), "{out}");
    assert!(out.contains("nothing in order matches \"^never-happens\"") && out.contains("exit: "), "{out}");
    assert!(out.contains("== fails: exit") && out.contains("record:") && out.contains("agents:"), "a failing scenario shows its run: {out}");
}

/// A scenario file with a key the format doesn't have is refused, naming the file.
#[test]
fn a_scenario_with_an_unknown_key_is_refused() {
    let tmp = scenario_folder(&[("typo.yaml", "project: app\nagnets: []\n")]);
    let out = play(tmp.path(), &["typo.yaml"]);
    assert!(!out.status.success() && said(&out).contains("typo.yaml"), "{}", said(&out));
}

// ---- A-38: the run report -----------------------------------------------------------------------------------------

/// `agg process <file> <project> --report <kind> --json`, parsed.
fn report(dir: &Path, kind: &str) -> serde_json::Value {
    let out = run(dir, &["--report", kind, "--json"]);
    assert!(out.status.success(), "{}", said(&out));
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().last().unwrap()).unwrap()
}

/// The report of a finished run: the map has each step in its phase with a state (an `each`'s steps once per item, a
/// skipped step skipped, a checkpoint answered `auto`), the timeline a row per result with its totals, the profile
/// the steps with their substeps; `agg serve`'s endpoint isn't needed to read them.
#[test]
fn the_run_report_has_a_map_a_timeline_and_a_profile() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let out = run(d, &[]);
    assert!(out.status.success(), "{}", said(&out));
    let map = report(d, "map");
    let state = |id: &str| map["phases"].as_array().unwrap().iter().flat_map(|p| p["steps"].as_array().unwrap().iter())
        .find(|s| s["id"] == id).unwrap_or_else(|| panic!("{id} not on the map: {map}")).clone();
    assert_eq!(state("survey")["state"], "done");
    assert_eq!(state("construct-U2")["state"], "done");
    assert_eq!(state("construct-U2")["item"], "U2");
    assert_eq!(state("extras")["state"], "skipped");
    assert_eq!(state("cp-survey")["answer"], "auto");
    assert_eq!(map["ended"], true);
    let timeline = report(d, "timeline");
    assert!(timeline["steps"].as_array().unwrap().iter().any(|r| r["step"] == "1.3 Survey"), "{timeline}");
    assert_eq!(timeline["checkpoints_asked"], 0, "an auto-approval is no human's answer");
    let profile = report(d, "profile");
    assert!(profile["steps"].as_array().unwrap().len() > 5 && profile["map"]["phases"].is_array(), "{profile}");
    let text = run(d, &["--report", "timeline"]);
    assert!(said(&text).contains("wall ") && said(&text).contains("repairs 0"), "{}", said(&text));
    assert!(!run(d, &["--report", "nonsense"]).status.success());
    // what `agg serve` and `agg status` read: the project noted its process file
    assert_eq!(agg::workflow::process::report::of(d, "map").unwrap()["ended"], true);
    assert!(agg::workflow::process::report::of(d, "nonsense").is_none());
    assert!(agg::workflow::process::report::of(tmp.path().join("nowhere").as_path(), "map").is_none());
}

// ---- A-38: standing texts ---------------------------------------------------------------------------------------------

/// `--brief <step>` prints the process's parts of a worker's brief without running anything: agg's standing texts
/// (the system prompt, the shared machine, the notes with the project's file), the step's prompt, and the critic's scope
/// with the file's own text after agg's; `--brief task-force` the task force's prompt (agg's, then the file's).
#[test]
fn a_brief_shows_the_standing_texts_and_the_files_that_follow_them() {
    let tmp = project(AUTO);
    let d = tmp.path();
    let out = run(d, &["--brief", "survey"]);
    assert!(out.status.success(), "{}", said(&out));
    let brief = said(&out);
    let at = |needle: &str| brief.find(needle).unwrap_or_else(|| panic!("{needle} not in the brief:\n{brief}"));
    assert!(at("Your brief is agg/private/INSTRUCTIONS.md") < at("the worker's prompt (prompts/survey.md)"), "{brief}");
    assert!(at("the worker's prompt") < at("## Working on a shared machine"), "{brief}");
    assert!(brief.contains("`notes.md` holds what earlier steps learned") && brief.contains("## survey"), "{brief}");
    assert!(at("## What you review, and what you never raise") < at("the critic's rubric"), "agg's scope, then the file's: {brief}");
    let force = said(&run(d, &["--brief", "task-force"]));
    assert!(force.find("## Goal").unwrap() < force.find("A step failed. Find its cause").unwrap(), "agg's text, then the file's: {force}");
    assert!(force.contains("**`fixed`**"), "{force}");
    let script = run(d, &["--brief", "sources"]);
    assert!(!script.status.success() && said(&script).contains("only an agent step"), "{}", said(&script));
}
