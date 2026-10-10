//! `prompt_file` / `role_prompt_file` end to end: the real `agg` binary, a fake `claude` that saves
//! every brief it is handed, and a prompt file OUTSIDE the repo that the fake edits mid-run.
//!
//! The acceptance criteria, each its own test: a step using the file keys gets the same brief as the
//! inline form; an edited file applies from the next session and shows a new hash in the ledger;
//! naming both forms, or a file that does not exist, is refused at startup at its config path.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The fake worker: save the brief it was handed as `$BRIEFS/brief-<n>`, and, when `$EDIT` names a
/// file, rewrite it — the edit the NEXT session's brief must show.
const FAKE: &str = r#"#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && { echo fake-claude 0.0.0; exit 0; }; done
n=$(ls "$BRIEFS" | wc -l | tr -d ' ')
cp agg/private/INSTRUCTIONS.md "$BRIEFS/brief-$((n + 1))"
[ -n "$EDIT" ] && echo "the second version of the task" > "$EDIT"
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"output_tokens":1},"total_cost_usd":0.01}'
"#;

/// A git project whose `agg.yaml` has `steps_yaml` as its `steps:` body, a judge that is never met
/// and a two-session cap; plus an outside dir (`briefs/`, prompt files) the fake may write.
fn project(steps_yaml: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::create_dir_all(out.path().join("briefs")).unwrap();
    write(&dir.join("bin/claude"), FAKE);
    chmod_x(&dir.join("bin/claude"));
    write(&dir.join("agg/judges/never.sh"), "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1}'\n");
    chmod_x(&dir.join("agg/judges/never.sh"));
    for (rel, text) in files {
        write(&dir.join("agg").join(rel), text);
    }
    write(
        &dir.join("agg/agg.yaml"),
        &format!(
            "project: \"pf\"\ndefaults: {{ model: \"fake\" }}\nsteps:\n{steps_yaml}\nsequence:\n  steps: [\"w\"]\n  \
             done_if: \"never\"\n  limits: {{ sessions: 2 }}\nsummary: {{ enabled: false }}\nmemory: {{ enabled: false }}\n"
        ),
    );
    write(&dir.join(".gitignore"), "bin/\n");
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "base"]);
    (tmp, out)
}

fn agg(dir: &Path, out: &Path, edit: Option<&Path>, args: &[&str]) -> Output {
    let path = format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::cargo_bin("agg").unwrap();
    c.current_dir(dir).env("PATH", path).env("HOME", out.join("home")).env("BRIEFS", out.join("briefs"));
    match edit {
        Some(p) => c.env("EDIT", p),
        None => c.env_remove("EDIT"),
    };
    c.args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn brief(out: &Path, n: u32) -> String {
    fs::read_to_string(out.join(format!("briefs/brief-{n}"))).unwrap()
}

/// The file keys produce byte-for-byte the brief the inline keys do.
#[test]
fn a_step_using_prompt_files_gets_the_same_brief_as_the_inline_form() {
    let role = "You are the tester.\nBreak things.\n";
    let task = "Write one failing test.\n";
    let inline = format!("  w: {{ role_prompt: {role:?}, prompt: {task:?} }}");
    let (a, a_out) = project(&inline, &[]);
    let filed = "  w: { role_prompt_file: \"roles/tester.md\", prompt_file: \"steps/w.md\" }";
    let (b, b_out) = project(filed, &[("roles/tester.md", role), ("steps/w.md", task)]);
    for (d, o) in [(&a, &a_out), (&b, &b_out)] {
        let r = agg(d.path(), o.path(), None, &["run"]);
        assert_eq!(r.status.code(), Some(4), "{}", text(&r));
    }
    let (ia, ib) = (brief(a_out.path(), 1), brief(b_out.path(), 1));
    assert!(ia.contains("Break things.") && ia.contains("Write one failing test."), "{ia}");
    assert_eq!(ia, ib, "the inline and the file-backed brief differ");
    assert!(!agg::paths::prompts_jsonl(a.path()).exists(), "an inline-only run records no prompt files");
}

/// An ABSOLUTE prompt file, edited by session 1: session 2's brief carries the new text, and the
/// ledger has one row per session with a different hash.
#[test]
fn an_edited_prompt_file_applies_next_session_with_a_new_hash() {
    let shared = tempfile::tempdir().unwrap();
    let file = shared.path().join("task.md");
    fs::write(&file, "the first version of the task\n").unwrap();
    let (p, out) = project(&format!("  w: {{ prompt_file: {:?} }}", file.display().to_string()), &[]);
    let r = agg(p.path(), out.path(), Some(&file), &["run"]);
    assert_eq!(r.status.code(), Some(4), "{}", text(&r));
    assert!(brief(out.path(), 1).contains("the first version"));
    assert!(brief(out.path(), 2).contains("the second version"), "the edit applies from the next session");
    let rows = agg::core::prompts::read_ledger(p.path());
    assert_eq!(rows.iter().map(|r| r.session).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(rows[0].files[0].key, "prompt_file");
    assert_eq!(rows[0].files[0].path, file.display().to_string());
    assert_ne!(rows[0].files[0].sha256, rows[1].files[0].sha256, "a changed file shows a new hash");
}

/// Both forms, and a missing file: `agg check` lists each at its own config path, and `agg run`
/// refuses to start (no worker ever runs).
#[test]
fn both_forms_or_a_missing_file_are_refused_at_startup_with_their_path() {
    let steps = "  w: { prompt: \"x\", prompt_file: \"steps/w.md\" }\n  v: { role_prompt_file: \"roles/gone.md\" }";
    let (p, out) = project(steps, &[("steps/w.md", "x\n")]);
    let c = agg(p.path(), out.path(), None, &["check", "--json"]);
    assert_eq!(c.status.code(), Some(1), "{}", text(&c));
    let v: serde_json::Value = serde_json::from_slice(&c.stdout).unwrap();
    let paths: Vec<&str> = v["problems"].as_array().unwrap().iter().filter_map(|p| p["path"].as_str()).collect();
    assert!(paths.contains(&"steps.w.prompt_file"), "{paths:?}");
    assert!(paths.contains(&"steps.v.role_prompt_file"), "{paths:?}");
    let r = agg(p.path(), out.path(), None, &["run"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r));
    assert!(fs::read_dir(out.path().join("briefs")).unwrap().next().is_none(), "no worker ran");
}
