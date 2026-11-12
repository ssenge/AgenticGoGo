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
    let d = tmp.path();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
        git(d, args);
    }
    std::fs::write(d.join("tools.yaml"), "worker: { agent: claude, model: fake }\ncritic: { agent: codex }\n").unwrap();
    std::fs::write(d.join("task.md"), "Build the thing.\n").unwrap();
    std::fs::write(d.join("settings.yaml"), settings).unwrap();
    std::fs::write(d.join("critic.ok"), "").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "baseline"]);
    tmp
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
                "2.3 Lint|skipped", "2.4 CP2 Build approved document|tool", "2.4 CP2 Build approved|auto-approval",
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
    let phases = std::fs::read_to_string(d.join(".git/phase-hooks.log")).unwrap();
    assert!(phases.lines().next().unwrap().ends_with("before") && phases.lines().last().unwrap().ends_with("after"), "{phases}");
    assert!(!std::fs::read_to_string(d.join(".git/step-hooks.log")).unwrap().is_empty(), "the step hook ran");
}
