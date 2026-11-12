//! Each step's base (A-37): the commit where it first started in this increment (`refs/agg/base/<key>`),
//! kept across its re-entries. `refs/agg/before/<key>` moves with every start, so a step re-entered after a failure
//! that left its work in place would count nothing it changed before (a quality floor saw no changed file). What a
//! step changed counts from its base.

use std::path::Path;
use std::process::Command;

/// The ref that holds step `key`'s base.
pub fn base(key: &str) -> String {
    format!("refs/agg/base/{key}")
}

/// Marks HEAD as step `key`'s base unless it has one: a re-entry keeps the first.
pub fn mark(project: &Path, key: &str) {
    // git's empty old value: create the ref only when it doesn't exist yet
    let _ = git(project, &["update-ref", &base(key), "HEAD", ""]);
}

/// Drops the bases that went stale: after a reset, every one HEAD no longer contains (the steps the reset took
/// back start anew); when the file `run.bases_reset_when_changed` changed (`all`), every one.
pub fn drop_stale(project: &Path, all: bool) {
    let Some(refs) = git(project, &["for-each-ref", "--format=%(refname)", "refs/agg/base/"]) else { return };
    for name in refs.lines().filter(|l| !l.is_empty()) {
        if all || git(project, &["merge-base", "--is-ancestor", name, "HEAD"]).is_none() {
            let _ = git(project, &["update-ref", "-d", name]);
        }
    }
}

/// Runs git in `project`; its output when it succeeds.
fn git(project: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(project).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@t"]] {
            git(dir.path(), args).unwrap();
        }
        dir
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", "-A"]).unwrap();
        git(dir, &["commit", "-q", "-m", file]).unwrap();
    }

    /// A re-entry keeps the first base; a reset drops the bases HEAD no longer holds, a new increment all of them.
    #[test]
    fn a_reentry_keeps_the_first_base_and_stale_ones_are_dropped() {
        let dir = repo();
        commit(dir.path(), "a");
        mark(dir.path(), "build");
        let first = git(dir.path(), &["rev-parse", &base("build")]).unwrap();
        commit(dir.path(), "b");
        mark(dir.path(), "build");
        assert_eq!(git(dir.path(), &["rev-parse", &base("build")]).unwrap(), first, "the first start is kept");
        mark(dir.path(), "later");
        commit(dir.path(), "c");
        git(dir.path(), &["reset", "-q", "--hard", "HEAD~2"]).unwrap();
        drop_stale(dir.path(), false);
        assert!(git(dir.path(), &["rev-parse", "--verify", "-q", &base("later")]).is_none(), "taken back by the reset");
        assert!(git(dir.path(), &["rev-parse", "--verify", "-q", &base("build")]).is_some());
        drop_stale(dir.path(), true);
        assert!(git(dir.path(), &["rev-parse", "--verify", "-q", &base("build")]).is_none());
    }
}
