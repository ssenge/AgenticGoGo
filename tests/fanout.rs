//! `Relentless::fan_out` through the library: lanes are plain functions that write and commit a file, no agent.

use relentless::prelude::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("README"), "x").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "base"]);
    d
}

fn make(lane: &Relentless, key: &str, file: &str) -> anyhow::Result<()> {
    std::fs::write(lane.dir().join(file), format!("{key}\n"))?;
    git(lane.dir(), &["add", "-A"]);
    git(lane.dir(), &["commit", "-qm", key]);
    Ok(())
}

/// Lanes join in key order whatever order they end in, each with a merge commit, and their worktrees and branches go.
#[test]
fn lanes_join_in_key_order_whatever_order_they_end_in() {
    let d = project();
    let rlt = Relentless::open(d.path()).unwrap();
    let reports = rlt.fan_out(&["a", "b", "c"], FanOut::new().max(3), |lane, key| {
        // the first key ends last
        std::thread::sleep(std::time::Duration::from_millis(if key == "a" { 1500 } else { 100 }));
        make(lane, key, &format!("{key}.txt"))
    }).unwrap();
    assert!(reports.iter().all(|r| r.joined() && r.tip.is_some()), "{reports:?}");
    assert_eq!(git(d.path(), &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join fan a\nrlt: join fan b\nrlt: join fan c");
    assert!(["a", "b", "c"].iter().all(|k| d.path().join(format!("{k}.txt")).is_file()));
    assert!(git(d.path(), &["worktree", "list"]).lines().count() == 1 && git(d.path(), &["branch", "--list", "rlt/*/lane-*"]).is_empty());
}

/// A lane that fails keeps its worktree; a lane after it that ended done waits (the join stays in order); a lane before it
/// is joined; a panic is a failed lane.
#[test]
fn a_failed_lane_stops_the_join_there_and_a_panic_is_a_failed_lane() {
    let d = project();
    let rlt = Relentless::open(d.path()).unwrap();
    let reports = rlt.fan_out(&["a", "b", "c", "d"], FanOut::new().max(4), |lane, key| match key {
        "b" => anyhow::bail!("b cannot"),
        "c" => panic!("c blew up"),
        _ => make(lane, key, &format!("{key}.txt")),
    }).unwrap();
    let ends: Vec<LaneEnd> = reports.iter().map(|r| r.end.clone()).collect();
    assert_eq!(ends[0], LaneEnd::Joined);
    assert!(matches!(&ends[1], LaneEnd::Failed(w) if w.contains("b cannot")), "{ends:?}");
    assert!(matches!(&ends[2], LaneEnd::Failed(w) if w.contains("panic: c blew up")), "{ends:?}");
    assert_eq!(ends[3], LaneEnd::Done, "d is done, and waits for the lanes before it");
    assert!(d.path().join("a.txt").is_file() && !d.path().join("d.txt").exists());
    assert!(d.path().join("rlt/private/lanes/fan-b").exists() && d.path().join("rlt/private/lanes/fan-d").exists());
}

/// Two lanes that change one file: the second is a conflict that names the file, aborted; the first stays joined.
#[test]
fn a_conflict_is_reported_with_its_files_and_aborted() {
    let d = project();
    let rlt = Relentless::open(d.path()).unwrap();
    let reports = rlt.fan_out(&["a", "b"], FanOut::new(), |lane, key| make(lane, key, "same.txt")).unwrap();
    assert_eq!(reports[0].end, LaneEnd::Joined);
    assert_eq!(reports[1].end, LaneEnd::Conflict(vec!["same.txt".into()]));
    assert!(git(d.path(), &["status", "--porcelain"]).is_empty(), "the merge is aborted, the tree clean");
}

/// Keys that become one name, and a project that isn't a repository, are refused before any lane starts.
#[test]
fn keys_that_clash_and_a_project_that_is_no_repository_are_refused() {
    let d = project();
    let rlt = Relentless::open(d.path()).unwrap();
    let err = rlt.fan_out(&["a b", "a-b"], FanOut::new(), |_, _| Ok(())).unwrap_err().to_string();
    assert!(err.contains("`a b` and `a-b` both become `a-b`"), "{err}");
    let plain = tempfile::tempdir().unwrap();
    let rlt = Relentless::open(plain.path()).unwrap();
    let err = rlt.fan_out(&["a"], FanOut::new(), |_, _| Ok(())).unwrap_err().to_string();
    assert!(err.contains("git repository"), "{err}");
}
