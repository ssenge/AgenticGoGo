//! The workflow building blocks (A-26 to A-31) against a real project: a git repo, real session branches, a
//! fake `claude` worker and a fake `codex` critic on PATH. Every assertion reads what the run produced (the
//! step history, the work on `main`, the critic's calls), never a flag the blocks set.
//!
//! Unix-only (the fake agents are shell scripts), like the rest of the suite.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use agg::prelude::*;
use agg::workflow::{Flow, Outcome, Role, Run, Task};

/// The one environment mutation in this binary: the fake agents' `bin/` on PATH and a throwaway HOME.
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("agg-workflow-env-{}", std::process::id()));
    let (bin, home) = (root.join("bin"), root.join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    // The worker: one line in work.log per session, unless the project says it is idle; a task force session
    // writes its proposal to the file its brief names; a voice of a discussion only writes its position, which
    // disagrees while .git/agg-disagreeing exists, and none while .git/agg-silent does.
    write_exe(&bin.join("claude"), r#"#!/bin/sh
worker=
for a in "$@"; do
  case "$a" in
    --version) echo "fake-claude 0.0.0"; exit 0 ;;
    *INSTRUCTIONS.md*) worker=1 ;;
  esac
done
position=$(grep -o 'Write your position to `[^`]*`' agg/private/INSTRUCTIONS.md 2>/dev/null | head -1 | cut -d'`' -f2)
if [ -n "$position" ]; then
  printf '%s\n' "$*" | grep -o -- '--model [^ ]*' >> .git/agg-voices.log
  cp agg/private/INSTRUCTIONS.md .git/agg-voices.brief
  if [ ! -f .git/agg-silent ]; then
    if [ -f .git/agg-disagreeing ]; then open=1; else open=0; fi
    printf '1. a point\n\nDISAGREE: %s\n' $open > "$position"
  fi
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"fake-'$$'","usage":{"output_tokens":1},"total_cost_usd":0.01}'
  exit 0
fi
if [ -n "$worker" ]; then cat agg/private/INSTRUCTIONS.md >> .git/agg-worker.args; { printf '%s' "$*" | tr '\n' ' '; echo; } >> .git/agg-worker.argv; fi
answer=$(grep -o '/[^ ]*task-force\.yaml' agg/private/INSTRUCTIONS.md 2>/dev/null | tail -1)
if [ -n "$answer" ] && [ -f task-force.answer ]; then cp task-force.answer "$answer"; rm task-force.answer; fi
if [ -n "$worker" ] && [ ! -f .git/agg-idle ]; then
  echo "session $$" >> work.log
  git add -A >/dev/null 2>&1
  git commit -qm "worker: did work" >/dev/null 2>&1
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"fake-'$$'","usage":{"output_tokens":1},"total_cost_usd":0.01}'
"#);
    // The critic: met when the project has `critic.ok`; every call's prompt goes to .git/critic.calls.
    write_exe(&bin.join("codex"), r#"#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && { echo "fake-codex 0.0.0"; exit 0; }; done
for a in "$@"; do echo "$a"; done >> .git/critic.calls
echo "---call" >> .git/critic.calls
if [ -f critic.ok ]; then v='{\"met\": true, \"rationale\": \"fine\"}'; else v='{\"met\": false, \"rationale\": \"fix A\"}'; fi
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"'"$v"'"}}'
"#);
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    std::env::set_var("HOME", &home);
    root
});

fn write_exe(p: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A clean git repo on `main`, with the check (met after `n` worker sessions) and the critic's rubric.
fn project(sessions_needed: usize) -> tempfile::TempDir {
    LazyLock::force(&ENV);
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(d, args);
    }
    std::fs::create_dir_all(d.join("agg/judges")).unwrap();
    write_exe(&d.join("agg/judges/check.sh"), &format!(
        "#!/bin/sh\nn=$(cat work.log 2>/dev/null | wc -l | tr -d ' ')\n\
         if [ \"$n\" -ge {sessions_needed} ]; then m=true; else m=false; fi\n\
         printf '{{\"met\": %s, \"rationale\": \"%s sessions\"}}\\n' $m $n\n"));
    std::fs::write(d.join("agg/judges/review.md"), "Is the work right?\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "baseline"]);
    tmp
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn flow(dir: &Path, run: Run) -> Flow {
    Flow::new(Agg::open(dir).unwrap().memory(false), run)
}

/// An agent step named in the flow `name`, its worker step named after its role, as a driver names them.
fn task(name: &str) -> Task {
    Task::new(Step::new("builder").model("fake").prompt("Do the work.")).named(name)
        .check(Judge::script("check", "agg/judges/check.sh"))
}

fn critic() -> Judge {
    Judge::rubric("review", "agg/judges/review.md").agent(Agent::Codex)
}

/// The kinds the history recorded for step `name`, in order.
fn kinds(flow: &Flow, name: &str) -> Vec<String> {
    flow.steps().entries().iter().filter(|e| e["step"] == name)
        .map(|e| e["kind"].as_str().unwrap_or_default().to_string()).collect()
}

fn critic_calls(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(".git/critic.calls")).unwrap_or_default();
    text.split("---call\n").filter(|c| !c.trim().is_empty()).map(String::from).collect()
}

/// A-26: a first result that fails the check is repaired with the check's rationale and passes on the second
/// round; the critic runs only once the check is met.
#[test]
fn a_failed_check_is_repaired_and_the_critic_runs_only_after_it() {
    let tmp = project(2);
    std::fs::write(tmp.path().join("critic.ok"), "").unwrap();
    let f = flow(tmp.path(), Run::default());
    let outcome = f.task(&task("1 Build").critic(critic()).repairs(2)).unwrap();
    f.flush().unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(kinds(&f, "1 Build"), ["repair", "done"]);
    assert_eq!(critic_calls(tmp.path()).len(), 1, "the critic ran once, after the check was met");
    assert_eq!(git(tmp.path(), &["show", "main:work.log"]).lines().count(), 2, "both sessions' work landed");
    let briefs = std::fs::read_to_string(tmp.path().join(".git/agg-worker.args")).unwrap();
    assert!(briefs.contains("## How your result is reviewed") && briefs.contains("agg/judges/check.sh"));
    assert!(briefs.contains("## Review to address"), "the repair round got the check's rationale");
}

/// A-26: a critic still unmet after its round leaves the work kept and the point returned; its second call
/// is a re-review of its earlier points.
#[test]
fn a_critic_point_left_after_its_round_is_accepted_and_returned() {
    let tmp = project(1);
    let f = flow(tmp.path(), Run::default());
    let outcome = f.task(&task("1 Build").critic(critic()).repairs(2)).unwrap();
    f.flush().unwrap();
    assert!(matches!(outcome, Outcome::Accepted(ref p) if p.contains("fix A")), "{outcome:?}");
    assert_eq!(kinds(&f, "1 Build"), ["repair", "accepted", "done"]);
    let calls = critic_calls(tmp.path());
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains("This is a re-review") && calls[1].contains("fix A"));
    assert!(calls[0].contains("## The worker's task") && calls[0].contains("Do the work."));
    let brief = std::fs::read_to_string(tmp.path().join(".git/agg-worker.args")).unwrap();
    assert!(brief.contains("Is the work right?") && !brief.contains("## The worker's task"),
            "the worker gets the rubric without the task it would repeat\n{brief}");
}

/// A task that climbs and ends unmet returns why to its caller, its work kept, instead of failing into
/// the task force; the caller's next attempt continues from it.
#[test]
fn a_climbing_task_that_ends_unmet_returns_why_with_its_work_kept() {
    let tmp = project(3);
    let f = flow(tmp.path(), Run::default());
    let outcome = f.task(&task("1 Build").repairs(0).climbs()).unwrap();
    assert!(matches!(outcome, Outcome::Unmet(ref why) if why.contains("1 sessions")), "{outcome:?}");
    assert_eq!(git(tmp.path(), &["show", "main:work.log"]).lines().count(), 1, "the unmet attempt's work landed");
    let next = f.task(&task("1 Build").repairs(1).climbs()).unwrap();
    f.flush().unwrap();
    assert_eq!(next, Outcome::Done, "the next attempt continued from it: 3 sessions in all");
    assert_eq!(kinds(&f, "1 Build"), ["unmet", "repair", "done"]);
}

/// L8-P2: a ladder's next tier continues an attempt that ended unmet, so it still gets the note at `--from`; only a
/// call after a result is a new task.
#[test]
fn the_next_attempt_after_an_unmet_one_keeps_the_note() {
    let tmp = project(3);
    let d = tmp.path();
    let f = flow(d, Run { from: Some("1".into()), note: Some("the port is wrong".into()), ..Run::default() });
    assert!(matches!(f.task(&task("1 Build").repairs(0).climbs()).unwrap(), Outcome::Unmet(_)));
    assert_eq!(f.task(&task("1 Build").repairs(1).climbs()).unwrap(), Outcome::Done);
    let briefs = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    let last = briefs.rsplit("# Session ").next().unwrap();
    assert!(last.contains("## Note from a later step") && last.contains("the port is wrong"), "{last}");
}

/// A-27: a script step's changes land on `main`; a step whose condition is unmet is skipped with no session.
#[test]
fn a_script_step_commits_and_an_unmet_condition_skips_without_a_session() {
    let tmp = project(1);
    let d = tmp.path();
    write_exe(&d.join("agg/judges/write.sh"), "#!/bin/sh\necho data > out.txt\necho '{\"met\": true, \"rationale\": \"wrote\"}'\n");
    write_exe(&d.join("agg/judges/never.sh"), "#!/bin/sh\necho '{\"met\": false, \"rationale\": \"no database\"}'\n");
    let f = flow(d, Run::default());
    f.script_step("1 Write", &Judge::script("write", "agg/judges/write.sh")).unwrap();
    assert!(!f.applies("2 Migrate", &Judge::script("never", "agg/judges/never.sh")).unwrap());
    f.flush().unwrap();
    assert_eq!(git(d, &["show", "main:out.txt"]), "data");
    assert_eq!((kinds(&f, "1 Write"), kinds(&f, "2 Migrate")), (vec!["tool".to_string()], vec!["skipped".to_string()]));
    assert!(!d.join(".git/agg-worker.args").exists(), "no session ran");
}

/// A-28 and A-29: an approval commits the humans' decisions; re-entered at it with the document unchanged,
/// it stands without asking. Re-entered at a step, the steps before it don't run and it gets the note.
#[test]
fn an_unchanged_approval_stands_and_a_reentered_step_gets_its_note() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join("plan.md"), "the plan\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "plan"]);
    let answered = answer_yes(d);
    let f = flow(d, Run::default());
    std::fs::write(d.join("decisions.md"), "keep the port\n").unwrap();
    f.approval("2 CP", "plan.md", "Approve the plan?", "decisions.md", &[]).unwrap();
    answered.join().unwrap();
    assert_eq!(git(d, &["show", "main:decisions.md"]), "keep the port", "the decisions are committed");
    drop(f);
    let run = Run { from: Some("2".into()), ..Run::default() };
    let f = flow(d, run.clone());
    f.task(&task("1 Build")).unwrap();
    f.approval("2 CP", "plan.md", "Approve the plan?", "decisions.md", &[]).unwrap();
    f.flush().unwrap();
    assert!(kinds(&f, "2 CP").last().is_some_and(|k| k == "approval"));
    assert!(!d.join(".git/agg-worker.args").exists(), "the step before `from` didn't run");
    drop(f);
    let f = flow(d, Run { from: Some("3".into()), note: Some("use port 8080".into()), ..Run::default() });
    assert_eq!(f.task(&task("3 Docs")).unwrap(), Outcome::Done);
    assert!(std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap().contains("use port 8080"));
}

/// A-29: after a re-entry, a later step whose inputs didn't change since its result is unchanged, no session.
#[test]
fn a_later_step_with_unchanged_inputs_refreshes_without_a_session() {
    let tmp = project(1);
    let d = tmp.path();
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Build")).unwrap(), Outcome::Done);
    assert_eq!(f.task(&task("2 Docs").inputs(["docs/"])).unwrap(), Outcome::Done);
    f.flush().unwrap();
    drop(f);
    let sessions = std::fs::read_to_string(d.join("work.log")).unwrap().lines().count();
    let f = flow(d, Run { from: Some("1".into()), note: Some("again".into()), ..Run::default() });
    assert_eq!(f.task(&task("1 Build")).unwrap(), Outcome::Done);
    assert_eq!(f.task(&task("2 Docs").inputs(["docs/"])).unwrap(), Outcome::Unchanged);
    assert_eq!(std::fs::read_to_string(d.join("work.log")).unwrap().lines().count(), sessions + 1);
}

/// A later step that names no inputs can't tell a refresh from a new task: after a re-entry it runs its task
/// whole, without being told to keep its result.
#[test]
fn a_later_step_without_inputs_runs_its_task_whole() {
    let tmp = project(1);
    let d = tmp.path();
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Build")).unwrap(), Outcome::Done);
    assert_eq!(f.task(&task("2 Docs")).unwrap(), Outcome::Done);
    f.flush().unwrap();
    drop(f);
    let f = flow(d, Run { from: Some("1".into()), note: Some("again".into()), ..Run::default() });
    assert_eq!(f.task(&task("1 Build")).unwrap(), Outcome::Done);
    let _ = f.task(&task("2 Docs"));
    let brief = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    assert!(!brief.contains("Since your last result"), "{brief}");
}

/// Every session starts fresh, a repair's and a re-entered step's too: what a step knows is in its files.
#[test]
fn every_session_starts_fresh() {
    let tmp = project(2);
    let d = tmp.path();
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Build").repairs(1)).unwrap(), Outcome::Done);
    drop(f);
    std::fs::write(d.join("work.log"), "").unwrap();
    git(d, &["commit", "-qam", "start over"]);
    let f = flow(d, Run { from: Some("1".into()), note: Some("fix x".into()), ..Run::default() });
    assert_eq!(f.task(&task("1 Build").repairs(1)).unwrap(), Outcome::Done);
    let argv = std::fs::read_to_string(d.join(".git/agg-worker.argv")).unwrap();
    assert_eq!(argv.lines().count(), 4, "first run: work, repair; re-entry: work, repair\n{argv}");
    assert!(!argv.contains("--resume"), "{argv}");
}

/// A flow's scratch folder goes when the flow ends, however it ended, read-only files and all.
#[test]
fn a_flows_scratch_folder_is_removed_when_it_ends() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = project(1);
    let f = flow(tmp.path(), Run::default());
    let file = f.scratch("7.1-cannot-fix.md").unwrap();
    let dir = file.parent().unwrap().to_path_buf();
    std::fs::write(&file, "an old claim").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert!(matches!(f.halt("stop").unwrap_err(), Fatal::Ended(RunOutcome::Halt)));
    drop(f);
    assert!(!dir.exists(), "{} is left", dir.display());
}

/// A-30: a failed step goes to the task force, which proposes an earlier step; a yes records the re-entry and ends
/// the run.
#[test]
fn the_task_force_proposes_an_earlier_step_and_a_yes_ends_the_run_for_the_reentry() {
    let tmp = project(9);
    let d = tmp.path();
    std::fs::write(d.join("task-force.answer"), "reenter: '1'\ncause: step 1 built the wrong thing\nnote: build x\n").unwrap();
    let answered = answer_yes(d);
    let force = Step::new("task-force").model("fake").prompt("Find the cause.");
    let f = flow(d, Run::default()).on_failure(force, 3);
    f.task(&Task::new(Step::new("1 Build").model("fake").prompt("Build."))).unwrap();
    let err = f.task(&task("2 Test")).unwrap_err();
    answered.join().unwrap();
    assert!(matches!(err, Fatal::Ended(RunOutcome::Halt)), "{err:?}");
    let last = f.steps().entries().last().cloned().unwrap();
    assert_eq!((last["kind"].as_str(), last["met"].as_bool()), (Some("task-force"), Some(true)));
    assert!(last["rationale"].as_str().unwrap().contains("re-enter at 1: build x"));
}

/// A proposal the driver trusts goes ahead without a human's yes: no ask, the re-entry recorded as trusted.
#[test]
fn a_trusted_task_force_proposal_goes_ahead_without_asking() {
    let tmp = project(9);
    let d = tmp.path();
    std::fs::write(d.join("task-force.answer"), "reenter: '1'\ncause: step 1's script hid a failure\nnote: fix it\n").unwrap();
    let force = Step::new("task-force").model("fake").prompt("Find the cause.");
    let f = flow(d, Run::default()).on_failure(force, 3).trust(|failed, at| failed == "2 Test" && at == "1");
    f.task(&Task::new(Step::new("1 Build").model("fake").prompt("Build."))).unwrap();
    let err = f.task(&task("2 Test")).unwrap_err();
    assert!(matches!(err, Fatal::Ended(RunOutcome::Halt)), "{err:?}");
    assert!(agg::core::asks::open(d).is_empty(), "no ask for a trusted proposal");
    let last = f.steps().entries().last().cloned().unwrap();
    assert_eq!((last["kind"].as_str(), last["met"].as_bool()), (Some("task-force"), Some(true)));
    assert!(last["rationale"].as_str().unwrap().contains("re-enter at 1 (a proposal the driver trusts, no ask): fix it"));
}

/// A driver re-enters at a step the run has seen, recorded with its note, and ends the run for it; a step
/// the run hasn't seen is refused without ending it.
#[test]
fn a_driver_reenters_at_a_step_it_ran_and_not_at_one_it_didnt() {
    let tmp = project(1);
    let f = flow(tmp.path(), Run::default());
    f.task(&Task::new(Step::new("1 Design").model("fake").prompt("Design."))).unwrap();
    assert!(matches!(f.reenter("9", "x").unwrap_err(), Fatal::Other(_)), "no step 9 in this run");
    assert!(matches!(f.reenter("1", "carry CP3's decision").unwrap_err(), Fatal::Ended(RunOutcome::Halt)));
    let last = f.steps().entries().last().cloned().unwrap();
    assert_eq!((last["kind"].as_str(), last["rationale"].as_str()), (Some("reentry"), Some("carry CP3's decision")));
}

/// Answers the first open yes/no ask with yes, from another thread, as an operator would.
fn answer_yes(dir: &Path) -> std::thread::JoinHandle<()> {
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        for _ in 0..1200 {
            if let Some(a) = agg::core::asks::open(&dir).into_iter().next() {
                agg::core::asks::answer(&dir, &a.id, "yes", "test", agg::util::now_epoch()).unwrap();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no ask appeared");
    })
}

/// A condition flips with the world, not with the work: met when one step's work landed and unmet before the
/// next, it is no regression, and the next step's work lands too (the gate held it once and rolled back).
#[test]
fn a_condition_that_flips_between_steps_is_no_regression() {
    let tmp = project(1);
    let d = tmp.path();
    write_exe(&d.join("agg/judges/cond.sh"), "#!/bin/sh\nif [ -f cond.on ]; then m=true; else m=false; fi\n\
                                             printf '{\"met\": %s, \"rationale\": \"cond\"}\\n' $m\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "the condition"]);
    std::fs::write(d.join("cond.on"), "").unwrap();
    let cond = Judge::script("cond", "agg/judges/cond.sh");
    let f = flow(d, Run::default());
    assert!(f.applies("1 Prepare", &cond).unwrap());
    assert_eq!(f.task(&task("2 Build")).unwrap(), Outcome::Done);
    std::fs::remove_file(d.join("cond.on")).unwrap();
    assert!(!f.applies("3 Optional", &cond).unwrap(), "the condition no longer holds");
    assert_eq!(f.task(&task("4 Build more")).unwrap(), Outcome::Done);
    f.flush().unwrap();
    assert_eq!(git(d, &["show", "main:work.log"]).lines().count(), 2, "both steps' work landed");
}


/// A-35: re-entered at a step with a decision, a step whose result it doesn't concern changes nothing and is
/// unchanged, never a note it failed to carry out; it saw the decision as a change to what it reads.
#[test]
fn a_decision_a_step_doesnt_need_leaves_it_unchanged() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join("decisions.md"), "- keep the port\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "decisions"]);
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Design").inputs(["decisions.md"])).unwrap(), Outcome::Done);
    f.flush().unwrap();
    drop(f);
    std::fs::write(d.join("decisions.md"), "- keep the port\n- test as the clerk\n").unwrap();
    git(d, &["commit", "-qam", "CP decided"]);
    std::fs::write(d.join(".git/agg-idle"), "").unwrap();
    let run = Run { from: Some("1".into()), note: Some("CP decided: test as the clerk".into()), decided: true,
                    ..Run::default() };
    let f = flow(d, run);
    assert_eq!(f.task(&task("1 Design").inputs(["decisions.md"])).unwrap(), Outcome::Unchanged);
    let brief = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    assert!(brief.contains("Since your last result") && brief.contains("test as the clerk"), "{brief}");
}

/// L8-P2: a step a re-entered run calls a second time (a loop's fix step, once per failure) is a new task, not a
/// refresh of the result it gave earlier in the same run.
#[test]
fn a_step_called_again_in_a_reentered_run_is_a_new_task() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join("decisions.md"), "- keep the port\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "decisions"]);
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Fix").inputs(["decisions.md"])).unwrap(), Outcome::Done);
    f.flush().unwrap();
    drop(f);
    std::fs::write(d.join("decisions.md"), "- keep the port\n- the run failed\n").unwrap();
    git(d, &["commit", "-qam", "failed"]);
    std::fs::write(d.join(".git/agg-idle"), "").unwrap();
    let f = flow(d, Run { from: Some("1".into()), note: Some("failed".into()), decided: true, ..Run::default() });
    assert_eq!(f.task(&task("1 Fix").inputs(["decisions.md"])).unwrap(), Outcome::Unchanged);
    let brief = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    assert!(brief.contains("Since your last result"), "the first call refreshes: {brief}");
    std::fs::remove_file(d.join(".git/agg-idle")).unwrap();
    assert_eq!(f.task(&task("1 Fix").inputs(["decisions.md"])).unwrap(), Outcome::Done);
    let briefs = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    let brief = briefs.rsplit("# Session ").next().unwrap();
    assert!(!brief.contains("since your") && !brief.contains("Since your"), "the second call is a new task: {brief}");
}

/// A-35: a decision with none of the step's inputs changed needs no session at all.
#[test]
fn a_decision_outside_a_steps_inputs_needs_no_session() {
    let tmp = project(1);
    let d = tmp.path();
    let f = flow(d, Run::default());
    assert_eq!(f.task(&task("1 Design").inputs(["design.md"])).unwrap(), Outcome::Done);
    f.flush().unwrap();
    drop(f);
    let sessions = std::fs::read_to_string(d.join("work.log")).unwrap().lines().count();
    let f = flow(d, Run { from: Some("1".into()), note: Some("decided".into()), decided: true, ..Run::default() });
    assert_eq!(f.task(&task("1 Design").inputs(["design.md"])).unwrap(), Outcome::Unchanged);
    assert_eq!(std::fs::read_to_string(d.join("work.log")).unwrap().lines().count(), sessions);
}

/// A-35: the task force repairs a failed step's cause in place; judged again and met, the step is done and the run
/// goes on, recorded with what the task force changed.
#[test]
fn the_task_force_repairs_in_place_and_the_step_judged_again_goes_on() {
    let tmp = project(2);
    let d = tmp.path();
    std::fs::write(d.join("task-force.answer"), "outcome: fixed\ncause: a tool was missing\nnote: installed it\n").unwrap();
    let force = Step::new("task-force").model("fake").prompt("Find the cause.");
    let f = flow(d, Run::default()).on_failure(force, 3);
    assert_eq!(f.task(&task("1 Build")).unwrap(), Outcome::Done, "its check met after the task force's session");
    assert_eq!(f.task(&task("2 Test")).unwrap(), Outcome::Done, "the run went on");
    f.flush().unwrap();
    let fixed = f.steps().entries().into_iter().find(|e| e["kind"] == "task-force").unwrap();
    assert_eq!(fixed["met"].as_bool(), Some(true));
    let why = fixed["rationale"].as_str().unwrap();
    assert!(why.starts_with("fixed in place (") && why.contains("work.log") && why.ends_with("a tool was missing: installed it"),
            "{fixed}");
    assert!(agg::core::asks::open(d).is_empty(), "a repair in place asks no one");
}

/// A-35: a repair in place the step's judges still don't meet ends the run for a human.
#[test]
fn a_repair_in_place_still_unmet_ends_the_run() {
    let tmp = project(9);
    let d = tmp.path();
    std::fs::write(d.join("task-force.answer"), "outcome: fixed\ncause: guessed\nnote: changed x\n").unwrap();
    let force = Step::new("task-force").model("fake").prompt("Find the cause.");
    let f = flow(d, Run::default()).on_failure(force, 3);
    assert!(matches!(f.task(&task("1 Build")).unwrap_err(), Fatal::Ended(RunOutcome::Halt)));
    let last = f.steps().entries().last().cloned().unwrap();
    assert_eq!((last["kind"].as_str(), last["met"].as_bool()), (Some("task-force"), Some(false)));
    assert!(last["rationale"].as_str().unwrap().contains("judged again: 2 sessions"), "{last}");
}

/// A-35: a script step that the task force repaired runs its script again, and lands when it is met.
#[test]
fn a_script_step_repaired_in_place_runs_its_script_again() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join("task-force.answer"), "outcome: fixed\ncause: no log\nnote: wrote one\n").unwrap();
    let force = Step::new("task-force").model("fake").prompt("Find the cause.");
    let f = flow(d, Run::default()).on_failure(force, 3);
    f.script_step("1 Count", &Judge::script("check", "agg/judges/check.sh")).unwrap();
    f.flush().unwrap();
    assert_eq!(kinds(&f, "1 Count"), ["tool", "task-force", "tool"]);
}

/// A-35: a check blocked by another step's result gets no repair round: the step fails at once.
#[test]
fn a_blocked_check_fails_without_a_repair_round() {
    let tmp = project(1);
    let d = tmp.path();
    write_exe(&d.join("agg/judges/blocked.sh"),
              "#!/bin/sh\necho '{\"met\": false, \"blocked_by\": \"bindings.lock.yaml\", \"rationale\": \"no tool\"}'\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "a blocked check"]);
    let f = flow(d, Run::default());
    let t = Task::new(Step::new("builder").model("fake").prompt("Do the work.")).named("1 Rules")
        .check(Judge::script("blocked", "agg/judges/blocked.sh")).repairs(2);
    assert!(matches!(f.task(&t).unwrap_err(), Fatal::Ended(RunOutcome::Halt)));
    assert_eq!(std::fs::read_to_string(d.join("work.log")).unwrap().lines().count(), 1, "no repair session");
}

/// A voice of a discussion on the fake claude with `model`.
fn voice(model: &str) -> Role {
    Role { agent: Agent::Claude, model: Some(model.into()), effort: Effort::Default }
}

/// The models of the voices' sessions, in order.
fn voices(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(".git/agg-voices.log")).unwrap_or_default().lines().map(String::from).collect()
}

/// A-36: two voices take positions on their own, revise them once and agree; the worker then carries out the
/// task with their last positions in its brief, in one session.
#[test]
fn voices_discuss_the_task_until_they_agree() {
    let tmp = project(1);
    let d = tmp.path();
    let f = flow(d, Run::default());
    let t = task("1 Build").section("## Inputs\n\nthe plan").voices([voice("fake-a"), voice("fake-b")]);
    assert_eq!(f.task(&t).unwrap(), Outcome::Done);
    f.flush().unwrap();
    assert_eq!(voices(d), ["--model fake-a", "--model fake-b", "--model fake-a", "--model fake-b"]);
    let heard = std::fs::read_to_string(d.join(".git/agg-voices.brief")).unwrap();
    assert!(heard.contains("Do the work.") && heard.contains("## Inputs"), "the task with its sections: {heard}");
    assert!(!heard.contains("Check your result"), "not how it is reviewed: {heard}");
    assert!(heard.contains("you are voice 2. You wrote") && heard.contains("1-position-2-0.md"), "its own: {heard}");
    assert!(heard.contains("1-position-1-1.md"), "voice 1's position of this round, not the last: {heard}");
    let brief = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    assert!(brief.contains("The discussion before you") && brief.contains("until they agreed"), "{brief}");
    assert!(brief.contains("1-position-2-1.md"), "the last round's positions: {brief}");
    assert_eq!(std::fs::read_to_string(d.join("work.log")).unwrap().lines().count(), 1, "one worker session");
    assert_eq!(kinds(&f, "1 Build"), ["discussion", "done"]);
}

/// A-36: voices that still disagree stop after the last round, and the worker is told so.
#[test]
fn a_discussion_that_doesnt_settle_ends_after_its_rounds() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join(".git/agg-disagreeing"), "").unwrap();
    let f = flow(d, Run::default());
    f.task(&task("1 Build").voices([voice("fake-a"), voice("fake-b")])).unwrap();
    f.flush().unwrap();
    assert_eq!(voices(d).len(), 8, "the positions on their own and three rounds, for each voice");
    let brief = std::fs::read_to_string(d.join(".git/agg-worker.args")).unwrap();
    assert!(brief.contains("still disagree on some points"), "{brief}");
    let discussion = f.steps().entries().into_iter().find(|e| e["kind"] == "discussion").unwrap();
    assert_eq!(discussion["met"], false);
}

/// A-36: a voice that writes no position fails the step, before any worker session.
#[test]
fn a_voice_without_a_position_fails_the_step() {
    let tmp = project(1);
    let d = tmp.path();
    std::fs::write(d.join(".git/agg-silent"), "").unwrap();
    let f = flow(d, Run::default());
    assert!(matches!(f.task(&task("1 Build").voices([voice("fake-a")])).unwrap_err(), Fatal::Ended(_)));
    assert!(!d.join("work.log").exists(), "no worker session");
}
