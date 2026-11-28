//! Joining a finished lane into the run's branch: `git merge --no-ff` of the lane's branch, in item order, one commit
//! each (`rlt: join <each> <key>`). A lane whose tip the run's branch already holds (a human merged it by hand) is only
//! recorded. A conflict is aborted and named, never half-resolved.

use super::worktree::Lane;
use crate::git::git;
use std::path::Path;

/// What joining a lane did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Joined {
    /// merged: the new `HEAD`
    Merged(String),
    /// the run's branch held the lane's tip already
    Already(String),
    /// the merge conflicted in these files and was aborted
    Conflict(Vec<String>),
    /// git refused for another reason
    Failed(String),
}

/// The commit message of a lane's join.
pub fn subject(lane: &Lane) -> String {
    format!("rlt: join {} {}", lane.each, lane.key)
}

/// Whether `branch`'s tip is already part of `HEAD`.
pub fn is_in(project: &Path, branch: &str) -> bool {
    git(project, &["merge-base", "--is-ancestor", &format!("refs/heads/{branch}"), "HEAD"]).0
}

/// Merges `lane`'s branch into the branch `project` has checked out.
pub fn join(project: &Path, lane: &Lane) -> Joined {
    let head = |p: &Path| git(p, &["rev-parse", "HEAD"]).1;
    if is_in(project, &lane.branch) {
        return Joined::Already(head(project));
    }
    let (ok, _, err) = git(project, &["merge", "--no-ff", "-m", &subject(lane), &lane.branch]);
    if ok {
        return Joined::Merged(head(project));
    }
    let (_, files, _) = git(project, &["diff", "--name-only", "--diff-filter=U"]);
    let conflicted: Vec<String> = files.lines().map(str::to_string).filter(|f| !f.is_empty()).collect();
    let _ = git(project, &["merge", "--abort"]);
    if conflicted.is_empty() {
        Joined::Failed(err)
    } else {
        Joined::Conflict(conflicted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::worktree;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"],
                     &["config", "commit.gpgsign", "false"], &["config", "core.hooksPath", "/dev/null"]] {
            assert!(git(d.path(), args).0);
        }
        std::fs::write(d.path().join("shared"), "base\n").unwrap();
        crate::git::ensure_rlt_gitignored(d.path());   // the lanes' worktrees sit under rlt/private/
        assert!(git(d.path(), &["add", "-A"]).0 && git(d.path(), &["commit", "-qm", "base"]).0);
        d
    }

    fn commit_in(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        assert!(git(dir, &["add", "-A"]).0 && git(dir, &["commit", "-qm", file]).0);
    }

    /// Lanes that touch different files join in order, one merge commit each, and a lane already in is only recorded.
    #[test]
    fn lanes_join_with_one_merge_commit_each_and_a_lane_already_in_is_recorded() {
        let r = repo();
        let (a, b) = (Lane::of(r.path(), "p", "units", "a"), Lane::of(r.path(), "p", "units", "b"));
        worktree::add(r.path(), &a, "HEAD").unwrap();
        worktree::add(r.path(), &b, "HEAD").unwrap();
        commit_in(&b.dir, "b.txt", "b");
        commit_in(&a.dir, "a.txt", "a");
        assert!(matches!(join(r.path(), &a), Joined::Merged(_)));
        assert!(matches!(join(r.path(), &b), Joined::Merged(_)));
        assert!(r.path().join("a.txt").is_file() && r.path().join("b.txt").is_file());
        let log = git(r.path(), &["log", "--format=%s", "-n", "2"]).1;
        assert_eq!(log, "rlt: join units b\nrlt: join units a");
        assert!(matches!(join(r.path(), &a), Joined::Already(_)), "joined twice is recorded, not merged again");
    }

    /// A conflict is aborted and names the files; the run's tree is clean again.
    #[test]
    fn a_conflict_is_aborted_and_names_the_files() {
        let r = repo();
        let (a, b) = (Lane::of(r.path(), "p", "units", "a"), Lane::of(r.path(), "p", "units", "b"));
        worktree::add(r.path(), &a, "HEAD").unwrap();
        worktree::add(r.path(), &b, "HEAD").unwrap();
        commit_in(&a.dir, "shared", "from a\n");
        commit_in(&b.dir, "shared", "from b\n");
        assert!(matches!(join(r.path(), &a), Joined::Merged(_)));
        assert_eq!(join(r.path(), &b), Joined::Conflict(vec!["shared".to_string()]));
        assert!(crate::git::is_clean(r.path()), "the aborted merge leaves nothing behind");
        assert_eq!(std::fs::read_to_string(r.path().join("shared")).unwrap(), "from a\n");
    }
}
