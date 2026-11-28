//! `kind: parallel` through the real binary: `examples/parallel-steps` copied into a temp project, a writer that sleeps,
//! different steps at once in lanes of their own, joined in the order they are listed.

mod common;

use common::Bounded;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-C", &dir.display().to_string()]).args(args).output_bounded().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    process: PathBuf,
    project: PathBuf,
}

/// `examples/parallel-steps` as a git project in a temp folder; `patches` are `(file, from, to)` edits of the example's files
/// (paths relative to the process's folder, or `demo/…` for the project's).
fn fixture(patches: &[(&str, &str, &str)]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/parallel-steps"), &home);
    let project = tmp.path().join("demo");
    fs::rename(home.join("projects/demo"), &project).unwrap();
    for (file, from, to) in patches {
        let path = match file.strip_prefix("demo/") {
            Some(rest) => project.join(rest),
            None => home.join(file),
        };
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "`{from}` is not in {}", path.display());
        fs::write(&path, text.replace(from, to)).unwrap();
    }
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"],
                 &["config", "commit.gpgsign", "false"], &["add", "-A"], &["commit", "-qm", "start"]] {
        git(&project, args);
    }
    Fixture { process: home.join("process.yaml"), project, _tmp: tmp }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

fn command(f: &Fixture, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rlt"));
    c.arg("process").arg(&f.process).arg(&f.project).args(args).env("HOME", f.project.join(".home"))
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output_for(300).unwrap();
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn rlt(f: &Fixture, args: &[&str]) -> (i32, String) {
    command(f, args, &[])
}

/// `--check` of the fixture's process file: whether it passed, and what it said.
fn check(f: &Fixture) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["process"]).arg(&f.process).arg("--check").output_bounded().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
}

/// The `written <start> to <end>` window of a file the writer made.
fn window(project: &Path, file: &str) -> (u64, u64) {
    let text = fs::read_to_string(project.join(file)).unwrap();
    let w: Vec<&str> = text.split_whitespace().collect();
    (w[2].parse().unwrap(), w[4].parse().unwrap())
}

/// A writer that fails the guide unless `LANE_OK` is set, and a slides writer that is never held up: a cause a human removes
/// between two runs.
const FAILS_GUIDE: (&str, &str, &str) = ("demo/agents/worker.sh", "start=$(date +%s)",
                                        "[ \"$file\" = guide.md ] && [ -z \"$LANE_OK\" ] && { echo \"the guide needs LANE_OK\"; exit 1; }\nstart=$(date +%s)");
const SLOW: (&str, &str, &str) = ("demo/agents/worker.sh", "sleep 2", "sleep 12");

/// The guide and the slides are written at once, joined in the order they are listed, and the lanes' history is in the run's.
#[test]
fn different_steps_run_at_once_and_join_in_the_listed_order() {
    // long enough that a lane whose start-up is slow on a loaded machine still overlaps the other
    let f = fixture(&[SLOW]);
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 0, "{said}");
    let (guide, slides) = (window(&f.project, "guide.md"), window(&f.project, "slides.md"));
    assert!(guide.0 < slides.1 && slides.0 < guide.1, "the two lanes overlapped: guide {guide:?}, slides {slides:?}");
    assert_eq!(git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join docs guide\nrlt: join docs slides");
    let history = fs::read_to_string(f.project.join("reports/steps.jsonl")).unwrap();
    assert!(["guide", "slides", "docs:guide", "docs:slides"].iter().all(|id| history.contains(id)), "{history}");
    assert!(!f.project.join("rlt/private/lanes/docs-guide").exists() && git(&f.project, &["branch", "--list", "rlt/*/lane-*"]).is_empty());
    assert!(fs::read_to_string(f.project.join("docs.txt")).unwrap().contains("slides.md"), "the step after the block saw both files");
}

/// A first child that fails fails the block; the second child's finished work waits (the join stays in order), and a resume runs
/// only the child that is left.
#[test]
fn a_failed_first_child_fails_the_block_and_a_resume_runs_only_what_is_left() {
    let f = fixture(&[FAILS_GUIDE, ("demo/agents/worker.sh", "sleep 2", "sleep 1")]);
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 3, "{said}");
    assert!(said.contains("lane `guide` failed"), "{said}");
    assert!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]).is_empty(), "the second lane waits for the first");
    assert!(f.project.join("rlt/private/lanes/docs-slides").exists(), "its work is kept for the resume");
    let slides = fs::read_to_string(f.project.join("rlt/private/lanes/docs-slides/slides.md")).unwrap();
    let (code, said) = command(&f, &["--resume"], &[("LANE_OK", "1")]);
    assert_eq!(code, 0, "{said}");
    assert_eq!(fs::read_to_string(f.project.join("slides.md")).unwrap(), slides, "the slides were not written again");
    assert_eq!(git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join docs guide\nrlt: join docs slides");
}

/// A second child that fails: the first is joined, and the block fails.
#[test]
fn a_failed_second_child_leaves_the_first_joined_and_fails_the_block() {
    let f = fixture(&[("demo/agents/worker.sh", "start=$(date +%s)", "[ \"$file\" = slides.md ] && { echo \"no slides\"; exit 1; }\nstart=$(date +%s)"),
                      ("demo/agents/worker.sh", "sleep 2", "sleep 1")]);
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 3, "{said}");
    assert!(said.contains("lane `slides` failed"), "{said}");
    assert_eq!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]), "rlt: join docs guide");
    assert!(f.project.join("guide.md").is_file() && !f.project.join("slides.md").exists());
}

/// A child whose `when:` isn't met ends its lane done with nothing to merge; the other is joined.
#[test]
fn a_child_that_is_skipped_by_its_when_merges_nothing() {
    let f = fixture(&[("process.yaml", "      name: Write the guide\n", "      name: Write the guide\n      when: scripts/never.sh\n"),
                      ("scripts/collect.sh", "cat guide.md slides.md > docs.txt", "cat slides.md > docs.txt"),
                      ("demo/agents/worker.sh", "sleep 2", "sleep 1")]);
    let never = f.process.parent().unwrap().join("scripts/never.sh");
    fs::write(&never, "#!/bin/sh\nprintf '{\"met\": false, \"rationale\": \"not today\"}\\n'\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&never, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 0, "{said}");
    assert!(!f.project.join("guide.md").exists() && f.project.join("slides.md").is_file());
    assert!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]).contains("rlt: join docs slides"));
}

/// What `--check` refuses in a parallel step: a checkpoint, a parallel step or `each` inside a lane, `max: 0`, and keys of other
/// kinds.
#[test]
fn a_parallel_step_refuses_what_a_lane_cannot_do() {
    let cp = fixture(&[("process.yaml", "    - id: slides\n", "    - { id: cp-docs, name: Approve, kind: checkpoint, doc: guide.md, question: \"ok?\" }\n    - id: slides\n")]);
    let (ok, said) = check(&cp);
    assert!(!ok && said.contains("a checkpoint inside a parallel step"), "{said}");
    let nested = fixture(&[("process.yaml", "    steps:\n    - id: guide\n      name: Write the guide\n      kind: agent\n",
                            "    steps:\n    - id: inner\n      name: Inner\n      kind: parallel\n      steps:\n      - { id: deep, name: Deep, kind: script, run: scripts/collect.sh }\n    - id: guide\n      name: Write the guide\n      kind: agent\n")]);
    let (ok, said) = check(&nested);
    assert!(!ok && said.contains("parallel inside the parallel step `docs`"), "{said}");
    let none = fixture(&[("process.yaml", "max: 2", "max: 0")]);
    let (ok, said) = check(&none);
    assert!(!ok && said.contains("`parallel.max` must be at least 1"), "{said}");
    let keys = fixture(&[("process.yaml", "    kind: parallel\n", "    kind: parallel\n    run: scripts/collect.sh\n")]);
    let (ok, said) = check(&keys);
    assert!(!ok && said.contains("`run` doesn't belong to a Parallel step"), "{said}");
    let empty = fixture(&[("process.yaml", "    parallel: { max: 2 }             # optional: without it, every child runs at once\n", "")]);
    assert!(check(&empty).0, "`parallel:` is optional");
}

/// The steps of the file are numbered through the block like an `each`'s, and `--list` shows them.
#[test]
fn the_children_of_a_parallel_step_are_numbered_and_listed() {
    let f = fixture(&[]);
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["process"]).arg(&f.process).arg("--list").output_bounded().unwrap();
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows: Vec<(String, Option<String>)> = list.as_array().unwrap().iter()
        .map(|r| (r["id"].as_str().unwrap().to_string(), r["number"].as_str().map(String::from))).collect();
    assert_eq!(rows, [("docs".into(), None), ("guide".into(), Some("1.1".into())), ("slides".into(), Some("1.2".into())), ("collect".into(), Some("1.3".into()))]);
}

/// A parallel step inside an `each`: its lanes run once per item, and each lane has the item.
#[test]
fn a_parallel_step_inside_an_each_gives_every_lane_the_item() {
    let f = fixture(&[
        ("process.yaml", "  - id: docs\n    name: The docs\n    kind: parallel\n    parallel: { max: 2 }             # optional: without it, every child runs at once\n    steps:\n    - id: guide\n      name: Write the guide\n      kind: agent\n      sections: [{ text: \"FILE=guide.md\" }]     # the brief names the file this lane writes\n      checks: [verifiers/guide-made.sh]\n    - id: slides\n      name: Write the slides\n      kind: agent\n      sections: [{ text: \"FILE=slides.md\" }]\n      checks: [verifiers/slides-made.sh]\n  - id: collect\n    name: Collect the docs\n    kind: script\n    run: scripts/collect.sh\n",
         "  - id: units\n    name: The units\n    kind: each\n    items: { file: plan.yaml, items: units, key: id }\n    steps:\n    - id: docs\n      name: The docs\n      kind: parallel\n      steps:\n      - id: guide\n        name: Write the guide\n        kind: agent\n        sections: [{ text: \"FILE={{ item }}-guide.md\" }]\n        checks: [verifiers/guide-made.sh]\n      - id: slides\n        name: Write the slides\n        kind: agent\n        sections: [{ text: \"FILE={{ item }}-slides.md\" }]\n        checks: [verifiers/slides-made.sh]\n  - id: collect\n    name: Collect the docs\n    kind: script\n    run: scripts/collect.sh\n"),
        ("verifiers/guide-made.sh", "[ -s guide.md ]", "ls *-guide.md >/dev/null 2>&1"),
        ("verifiers/slides-made.sh", "[ -s slides.md ]", "ls *-slides.md >/dev/null 2>&1"),
        ("scripts/collect.sh", "cat guide.md slides.md > docs.txt", "cat a-guide.md a-slides.md b-guide.md b-slides.md > docs.txt"),
        ("demo/agents/worker.sh", "sleep 2", "sleep 1"),
    ]);
    fs::write(f.project.join("plan.yaml"), "units:\n  - { id: a }\n  - { id: b }\n").unwrap();
    git(&f.project, &["add", "-A"]);
    git(&f.project, &["commit", "-qm", "plan"]);
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 0, "{said}");
    for file in ["a-guide.md", "a-slides.md", "b-guide.md", "b-slides.md"] {
        assert!(f.project.join(file).is_file(), "{file} is missing: {said}");
    }
    let joins = git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]);
    assert_eq!(joins.lines().count(), 4, "{joins}");
}

/// `examples/parallel-steps` plays through its scenario without tokens: two different lanes, joined in the listed order, then the
/// script step.
#[test]
fn the_parallel_steps_example_plays_through_its_scenario() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/parallel-steps");
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process").arg(root.join("process.yaml"))
        .arg("--scenario").arg(root.join("scenario.yaml"))
        .arg("--projects").arg(root.join("projects"))
        .output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.contains("PASS scenario"), "{said}");
}
