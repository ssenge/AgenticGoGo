//! `Relentless::fork` and `merge` through the library: forks do plain work (write and commit a file) or run steps against a
//! fake agent, on std threads and under rayon, and their work comes back through git, in the order the driver merges it.
//!
//! Unix-only (the fake agent is a shell script).

#![cfg(unix)]

use relentless::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// The one environment mutation in this binary: a `bin/` holding the fake `claude`, a throwaway `HOME`, and the log every
/// worker session writes its folder to (one line per session: which fork's worktree ran it).
static ENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let root = std::env::temp_dir().join(format!("rlt-forks-env-{}", std::process::id()));
    let (bin, home) = (root.join("bin"), root.join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
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
  echo "$PWD" >> "$RLT_FORKS_LOG"
  echo "session $$" > "worker-$$.txt"
  git add -A >/dev/null 2>&1
  git commit -qm "worker: did work" >/dev/null 2>&1
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"fake-'$$'","usage":{"output_tokens":1},"total_cost_usd":0.01}'
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    std::env::set_var("HOME", &home);
    std::env::set_var("RLT_FORKS_LOG", root.join("workers.log"));
    root
});

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repository on `main` with one commit.
fn project() -> tempfile::TempDir {
    LazyLock::force(&ENV);
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    git(d.path(), &["config", "user.name", "t"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    std::fs::write(d.path().join("README"), "x").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "base"]);
    d
}

/// The fork's work: a file, committed in its worktree.
fn make(fork: &Relentless, file: &str) {
    std::fs::write(fork.dir().join(file), format!("{file}\n")).unwrap();
    git(fork.dir(), &["add", "-A"]);
    git(fork.dir(), &["commit", "-qm", file]);
}

fn work() -> Step {
    Step::new("implement").model("fake").prompt("do a chunk")
}

/// How many worker sessions ran in worktrees under `project`, by the fork each ran in (`fork-<name>`).
fn sessions_in(project: &Path, name: &str) -> usize {
    let log = std::fs::read_to_string(ENV.join("workers.log")).unwrap_or_default();
    let top = project.canonicalize().unwrap();
    let tail = format!("rlt/private/lanes/fork-{name}");
    log.lines().filter(|l| l.starts_with(top.to_str().unwrap()) && l.ends_with(&tail)).count()
}

fn merges(project: &Path) -> Vec<String> {
    git(project, &["log", "--reverse", "--format=%s", "--grep=^rlt: merge fork"]).lines().map(String::from).collect()
}

/// Two different pieces of work overlap in time under `std::thread::scope`, and merge in the order the driver merges them.
#[test]
fn forks_overlap_under_std_threads_and_merge_in_call_order() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    let (guide, slides) = (rlt.fork().unwrap(), rlt.fork().unwrap());
    let spans = Mutex::new(Vec::<(Instant, Instant)>::new());
    std::thread::scope(|s| {
        for (fork, file) in [(&guide, "guide.md"), (&slides, "slides.md")] {
            let spans = &spans;
            s.spawn(move || {
                let from = Instant::now();
                make(fork, file);
                std::thread::sleep(Duration::from_millis(900));
                spans.lock().unwrap().push((from, Instant::now()));
            });
        }
    });
    let spans = spans.into_inner().unwrap();
    assert!(spans[0].0 < spans[1].1 && spans[1].0 < spans[0].1, "the two pieces of work overlapped: {spans:?}");
    rlt.merge(guide).unwrap();
    rlt.merge(slides).unwrap();
    assert_eq!(merges(d.path()), ["rlt: merge fork 0", "rlt: merge fork 1"]);
    assert!(d.path().join("guide.md").is_file() && d.path().join("slides.md").is_file());
    assert_eq!(git(d.path(), &["worktree", "list"]).lines().count(), 1, "the worktrees are gone");
    assert!(!git(d.path(), &["branch", "--list", "rlt/*/lane-fork-*"]).is_empty(), "the branches are kept");
}

/// The same under rayon: a plain `rayon::join` over two forks.
#[test]
fn forks_overlap_under_rayon() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    let (a, b) = (rlt.fork().unwrap(), rlt.fork().unwrap());
    let started = Instant::now();
    rayon::join(
        || {
            make(&a, "a.txt");
            std::thread::sleep(Duration::from_millis(900));
        },
        || {
            make(&b, "b.txt");
            std::thread::sleep(Duration::from_millis(900));
        },
    );
    assert!(started.elapsed() < Duration::from_millis(1700), "ran at once, not one after the other: {:?}", started.elapsed());
    rlt.merge(a).unwrap();
    rlt.merge(b).unwrap();
    assert!(d.path().join("a.txt").is_file() && d.path().join("b.txt").is_file());
}

/// Three tries, one verified and merged: the others are discarded (worktrees gone, branches kept, nothing on base).
#[test]
fn the_best_of_three_tries_is_merged_and_the_others_are_discarded() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    let tries = [rlt.fork().unwrap(), rlt.fork().unwrap(), rlt.fork().unwrap()];
    let ok = Verifier::native("ok", |c| Verdict::binary(c.read("ok.txt").is_ok()));
    std::thread::scope(|s| {
        for (i, t) in tries.iter().enumerate() {
            s.spawn(move || make(t, if i == 1 { "ok.txt" } else { "no.txt" }));
        }
    });
    let best = tries.into_iter().find(|t| t.verifier(&ok).met()).expect("one try passes");
    rlt.merge(best).unwrap();
    assert!(d.path().join("ok.txt").is_file() && !d.path().join("no.txt").exists());
    assert_eq!(merges(d.path()), ["rlt: merge fork 1"]);
    assert_eq!(git(d.path(), &["worktree", "list"]).lines().count(), 1);
    assert_eq!(git(d.path(), &["branch", "--list", "rlt/*/lane-fork-*"]).lines().count(), 3, "every try's branch is kept");
}

/// A fork forks, merges its child, and the root merges the fork: both files are on base, and the names say who is whose.
#[test]
fn a_fork_can_fork_and_the_names_say_whose_child_it_is() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    let mut outer = rlt.fork().unwrap();
    let inner = outer.fork().unwrap();
    make(&inner, "inner.txt");
    outer.merge(inner).unwrap();
    make(&outer, "outer.txt");
    rlt.merge(outer).unwrap();
    assert!(d.path().join("inner.txt").is_file() && d.path().join("outer.txt").is_file());
    assert_eq!(merges(d.path()), ["rlt: merge fork 0-0", "rlt: merge fork 0"], "the child's merge came into the root with its parent");
    assert!(git(d.path(), &["branch", "--list", "rlt/*/lane-fork-0-0"]).contains("lane-fork-0-0"), "the child is `0-0`");
}

/// Two forks that change one file: the second merge is a conflict that names the file and the branch, and base holds the first.
#[test]
fn a_conflict_is_an_error_that_names_the_files_and_the_branch() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    let (a, b) = (rlt.fork().unwrap(), rlt.fork().unwrap());
    std::fs::write(a.dir().join("README"), "from a\n").unwrap();
    git(a.dir(), &["commit", "-qam", "a"]);
    std::fs::write(b.dir().join("README"), "from b\n").unwrap();
    git(b.dir(), &["commit", "-qam", "b"]);
    rlt.merge(a).unwrap();
    match rlt.merge(b) {
        Err(Fatal::Conflict { branch, files }) => {
            assert_eq!(files, ["README"]);
            assert!(branch.ends_with("lane-fork-1"), "{branch}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(d.path().join("README")).unwrap(), "from a\n");
    assert!(!git(d.path(), &["branch", "--list", "rlt/*/lane-fork-1"]).is_empty(), "the fork's branch is kept for the operator");
}

/// A fork and a merge refuse what a git repository cannot hold: ungated work on either side, uncommitted changes, no repository.
#[test]
fn fork_and_merge_refuse_what_the_worktree_would_not_hold() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap();
    rlt.step(&work()).unwrap();
    let said = rlt.fork().err().unwrap().to_string();
    assert!(said.contains("ungated") && said.contains("gate()"), "{said}");
    rlt.gate().unwrap();
    let fork = rlt.fork().unwrap();
    fork.step(&work()).unwrap();
    let said = rlt.merge(fork).err().unwrap().to_string();
    assert!(said.contains("fork `2` has ungated work"), "{said}");

    let d = project();
    std::fs::write(d.path().join("README"), "edited, not committed").unwrap();
    let said = Relentless::open(d.path()).unwrap().fork().err().unwrap().to_string();
    assert!(said.contains("uncommitted"), "the run's own start refuses a tree a fork could not hold: {said}");

    let plain = tempfile::tempdir().unwrap();
    let said = Relentless::open(plain.path()).unwrap().fork().err().unwrap().to_string();
    assert!(said.contains("git"), "{said}");
}

/// A fork has the variables of the instance it came from on whichever thread runs it.
#[test]
fn a_fork_renders_the_variables_of_its_parent_on_its_own_thread() {
    let d = project();
    let mut rlt = Relentless::open(d.path()).unwrap().vars([("who", "world")]);
    let fork = rlt.fork().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let into = seen.clone();
    let probe = Verifier::native("probe", move |_| {
        *into.lock().unwrap() = relentless::core::vars::apply("hello {{ who }}").unwrap_or_default();
        Verdict::binary(true)
    });
    std::thread::scope(|s| {
        s.spawn(|| assert!(fork.verifier(&probe).met()));
    });
    assert_eq!(*seen.lock().unwrap(), "hello world");
}

/// A run that stops while fork `b` works: on a resume, fork `a`'s steps replay with no agent call (its work is merged), and `b`
/// continues after its last kept gate.
#[test]
fn a_resume_replays_the_merged_fork_and_continues_the_running_one() {
    let d = project();
    let (a_runs, b_runs);
    {
        let mut rlt = Relentless::open(d.path()).unwrap();
        let (a, b) = (rlt.fork().unwrap(), rlt.fork().unwrap());
        a.step(&work()).unwrap();
        a.gate().unwrap();
        rlt.merge(a).unwrap();
        b.step(&work()).unwrap();
        // the process dies here: nothing is dropped, nothing is written
        std::mem::forget(b);
        std::mem::forget(rlt);
        (a_runs, b_runs) = (sessions_in(d.path(), "0"), sessions_in(d.path(), "1"));
    }
    assert_eq!((a_runs, b_runs), (1, 1));
    let mut rlt = Relentless::open_with(d.path(), Opts { resume: true }).unwrap();
    let (a, b) = (rlt.fork().unwrap(), rlt.fork().unwrap());
    a.step(&work()).unwrap();
    a.gate().unwrap();
    rlt.merge(a).unwrap();
    b.step(&work()).unwrap();
    b.gate().unwrap();
    rlt.merge(b).unwrap();
    assert_eq!(sessions_in(d.path(), "0"), 1, "fork a replayed: no agent call");
    assert_eq!(sessions_in(d.path(), "1"), 2, "fork b had no kept gate: its step ran again");
    assert_eq!(merges(d.path()), ["rlt: merge fork 0", "rlt: merge fork 1"], "a's merge is not made again");
    let files = git(d.path(), &["ls-files"]);
    assert_eq!(files.lines().filter(|f| f.starts_with("worker-")).count(), 2, "both forks' work is on base: {files}");
}

/// Discarded tries replay their verdicts on a resume with no agent call, and the same one wins.
#[test]
fn a_resume_replays_the_discarded_tries_without_an_agent_call() {
    let d = project();
    let pick = |rlt: &mut Relentless| {
        let tries = [rlt.fork().unwrap(), rlt.fork().unwrap(), rlt.fork().unwrap()];
        let ok = Verifier::native("ok", |c| Verdict::binary(c.read("ok.txt").is_ok()));
        for (i, t) in tries.iter().enumerate() {
            t.step(&work()).unwrap();
            if i == 1 {
                make(t, "ok.txt");
            }
            t.gate().unwrap();
        }
        let best = tries.into_iter().find(|t| t.verifier(&ok).met()).expect("one passes");
        rlt.merge(best).unwrap();
    };
    {
        let mut rlt = Relentless::open(d.path()).unwrap();
        pick(&mut rlt);
        std::mem::forget(rlt);
    }
    let before: Vec<usize> = (0..3).map(|i| sessions_in(d.path(), &i.to_string())).collect();
    assert_eq!(before, [1, 1, 1]);
    let mut rlt = Relentless::open_with(d.path(), Opts { resume: true }).unwrap();
    pick(&mut rlt);
    let after: Vec<usize> = (0..3).map(|i| sessions_in(d.path(), &i.to_string())).collect();
    assert_eq!(after, before, "no agent call on the resume");
    assert_eq!(merges(d.path()), ["rlt: merge fork 1"]);
}
