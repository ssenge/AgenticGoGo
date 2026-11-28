//! A lane's worktree and branches: a linked worktree of the run's repository under `rlt/private/lanes/`, on a branch of
//! its own cut from the run's `HEAD`.
//!
//! All of a lane's refs sit under `rlt/<project>/lane-<each>-<key>` (the branch the join merges) and its sessions under
//! the project name the lane runs as (`<project>-<each>-<key>`, so `rlt/<project>-<each>-<key>/session-N`): apart from the
//! run's own `rlt/<project>/session-N`, which rules out a collision and git's file/folder ref conflicts.

use crate::git::git;
use std::path::{Path, PathBuf};

/// One lane's names and places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    /// the item's key, as the process file has it
    pub key: String,
    /// the each's id
    pub each: String,
    /// the worktree's root
    pub worktree: PathBuf,
    /// the lane's project folder: the worktree plus the run's project path inside the repository
    pub dir: PathBuf,
    /// the branch the join merges into the run's
    pub branch: String,
    /// the project name the lane's engine instance runs as
    pub project: String,
}

impl Lane {
    /// The lane of item `key` of `each`, for the run on `project` (the project folder) named `name`.
    pub fn of(project: &Path, name: &str, each: &str, key: &str) -> Lane {
        let id = format!("{}-{}", super::safe(each), super::safe(key));
        let top = toplevel(project).unwrap_or_else(|| project.to_path_buf());
        let inside = project.strip_prefix(&top).map(Path::to_path_buf).unwrap_or_default();
        let worktree = crate::paths::private_dir(project).join("lanes").join(&id);
        Lane {
            key: key.to_string(),
            each: each.to_string(),
            dir: worktree.join(inside),
            worktree,
            branch: format!("rlt/{}/lane-{id}", super::safe(name)),
            project: format!("{}-{id}", super::safe(name)),
        }
    }

    /// Whether the lane's worktree exists.
    pub fn exists(&self) -> bool {
        self.worktree.join(".git").exists()
    }
}

/// The root of the work tree `dir` is in.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    match git(dir, &["rev-parse", "--show-toplevel"]) {
        (true, out, _) if !out.is_empty() => {
            let path = PathBuf::from(out);
            Some(path.canonicalize().unwrap_or(path))
        }
        _ => None,
    }
}

/// Creates the lane's worktree on its branch, cut from `base`; one that exists (a resumed run's) is kept as it is.
pub fn add(project: &Path, lane: &Lane, base: &str) -> Result<(), String> {
    if lane.exists() {
        return Ok(());
    }
    if let Some(parent) = lane.worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // `-B`: a branch of the same name left by a run that was reset is started again from `base`
    let (ok, _, err) = git(project, &["worktree", "add", "-q", "-B", &lane.branch, &lane.worktree.display().to_string(), base]);
    if ok {
        Ok(())
    } else {
        Err(format!("lane `{}`: git worktree add failed: {err}", lane.key))
    }
}

/// Checks the lane's existing branch out into a worktree again, as it is (a settled fork's, to replay it on a resume); an
/// existing worktree is kept.
pub fn reopen(project: &Path, lane: &Lane) -> Result<(), String> {
    if lane.exists() {
        return Ok(());
    }
    if let Some(parent) = lane.worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let (ok, _, err) = git(project, &["worktree", "add", "-q", &lane.worktree.display().to_string(), &lane.branch]);
    if ok {
        Ok(())
    } else {
        Err(format!("lane `{}`: git worktree add of its branch failed: {err}", lane.key))
    }
}

/// A standalone clone of the project's repository for a lane that runs somewhere else, on its branch from `base`: it holds
/// everything the lane needs and nothing that points back at this machine (no remote, no link to this repository's folder).
pub fn add_clone(project: &Path, lane: &Lane, base: &str) -> Result<(), String> {
    if lane.exists() {
        return Ok(());
    }
    let Some(top) = toplevel(project) else { return Err(format!("lane `{}`: the project is in no git repository", lane.key)) };
    if lane.dir != lane.worktree {
        return Err(format!("lane `{}`: a lane that runs elsewhere needs the project at the root of its repository", lane.key));
    }
    if let Some(parent) = lane.worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let to = lane.worktree.display().to_string();
    let (ok, _, err) = git(project, &["clone", "-q", "--no-hardlinks", "--no-checkout", &top.display().to_string(), &to]);
    if !ok {
        return Err(format!("lane `{}`: git clone failed: {err}", lane.key));
    }
    let (ok, _, err) = git(&lane.worktree, &["checkout", "-q", "-B", &lane.branch, base]);
    if !ok {
        return Err(format!("lane `{}`: checking out {base} failed: {err}", lane.key));
    }
    let _ = git(&lane.worktree, &["remote", "remove", "origin"]);
    // the commits of the lane carry the run's identity: a clone doesn't inherit the repository's own config
    for key in ["user.name", "user.email"] {
        if let (true, value, _) = git(project, &["config", "--get", key]) {
            if !value.is_empty() {
                let _ = git(&lane.worktree, &["config", key, &value]);
            }
        }
    }
    Ok(())
}

/// Brings the commits of a lane's standalone clone (its `HEAD`) into the project's repository as the lane's branch, for the join.
pub fn fetch_back(project: &Path, lane: &Lane) -> Result<(), String> {
    let (ok, _, err) = git(project, &["fetch", "-q", &lane.worktree.display().to_string(), &format!("+HEAD:refs/heads/{}", lane.branch)]);
    if ok {
        Ok(())
    } else {
        Err(format!("lane `{}`: fetching its commits failed: {err}", lane.key))
    }
}

/// Removes the lane's worktree and its branch; what a lane's steps marked under `refs/rlt/` goes with the engine's own.
pub fn remove(project: &Path, lane: &Lane) {
    let _ = git(project, &["worktree", "remove", "--force", &lane.worktree.display().to_string()]);
    let _ = std::fs::remove_dir_all(&lane.worktree);
    let _ = git(project, &["worktree", "prune"]);
    let _ = git(project, &["branch", "-D", &lane.branch]);
}

/// Removes the lane's worktree and keeps its branch (the work stays reachable: a fork's, merged or not).
pub fn remove_worktree(project: &Path, lane: &Lane) {
    let _ = git(project, &["worktree", "remove", "--force", &lane.worktree.display().to_string()]);
    let _ = std::fs::remove_dir_all(&lane.worktree);
    let _ = git(project, &["worktree", "prune"]);
}

/// The lane's tip: its branch's commit.
pub fn tip(project: &Path, lane: &Lane) -> Option<String> {
    match git(project, &["rev-parse", &format!("refs/heads/{}", lane.branch)]) {
        (true, out, _) if !out.is_empty() => Some(out),
        _ => None,
    }
}

/// Copies each `seed` path (a file or a folder, relative to `from`) into `to`, skipping what isn't there.
pub fn seed(from: &Path, to: &Path, paths: &[String]) {
    for rel in paths {
        let (src, dst) = (from.join(rel), to.join(rel));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else if src.is_file() {
            if let Some(d) = dst.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::copy(&src, &dst);
        }
    }
}

/// Copies a folder tree (symlinks as links), keeping what is already there.
pub fn copy_dir(src: &Path, dst: &Path) {
    let _ = std::fs::create_dir_all(dst);
    let Ok(entries) = std::fs::read_dir(src) else { return };
    for e in entries.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        match e.file_type() {
            Ok(t) if t.is_dir() => copy_dir(&from, &to),
            Ok(t) if t.is_symlink() => {
                #[cfg(unix)]
                if let Ok(target) = std::fs::read_link(&from) {
                    let _ = std::os::unix::fs::symlink(target, &to);
                }
            }
            Ok(_) => {
                let _ = std::fs::copy(&from, &to);
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"],
                     &["config", "commit.gpgsign", "false"], &["config", "core.hooksPath", "/dev/null"]] {
            assert!(git(d.path(), args).0);
        }
        std::fs::write(d.path().join("f"), "x").unwrap();
        assert!(git(d.path(), &["add", "-A"]).0 && git(d.path(), &["commit", "-qm", "base"]).0);
        d
    }

    /// A lane's names: its refs are a subtree apart from the run's, the folder is under the private folder, and the
    /// project folder of a project inside the repository is the worktree plus that path.
    #[test]
    fn a_lanes_names_and_places_keep_it_apart_from_the_run() {
        let r = repo();
        let lane = Lane::of(r.path(), "demo", "units", "U 1");
        assert_eq!(lane.branch, "rlt/demo/lane-units-U-1");
        assert_eq!(lane.project, "demo-units-U-1");
        assert!(lane.worktree.ends_with("rlt/private/lanes/units-U-1") && lane.dir == lane.worktree);
        std::fs::create_dir_all(r.path().join("sub/app")).unwrap();
        let inner = Lane::of(&r.path().join("sub/app").canonicalize().unwrap(), "app", "units", "a");
        assert!(inner.dir.ends_with("lanes/units-a/sub/app"), "{:?}", inner.dir);
    }

    /// A worktree is added on its own branch from the base, kept when it exists, and removed with its branch.
    #[test]
    fn a_worktree_is_added_kept_and_removed_with_its_branch() {
        let r = repo();
        let lane = Lane::of(r.path(), "demo", "units", "a");
        add(r.path(), &lane, "HEAD").unwrap();
        assert!(lane.exists() && lane.dir.join("f").is_file());
        assert_eq!(tip(r.path(), &lane), Some(git(r.path(), &["rev-parse", "HEAD"]).1));
        std::fs::write(lane.dir.join("kept"), "work").unwrap();
        add(r.path(), &lane, "HEAD").unwrap();
        assert!(lane.dir.join("kept").exists(), "an existing worktree is left alone");
        remove(r.path(), &lane);
        assert!(!lane.exists() && tip(r.path(), &lane).is_none());
    }

    /// A clone is standalone (no remote), carries the lane's branch from the base, and its commits come back as the lane's branch.
    #[test]
    fn a_clone_is_standalone_and_its_commits_come_back_as_the_lanes_branch() {
        let r = repo();
        let lane = Lane::of(r.path(), "demo", "units", "a");
        add_clone(r.path(), &lane, "HEAD").unwrap();
        assert!(lane.worktree.join(".git").is_dir() && lane.dir.join("f").is_file(), "a clone has a .git folder and the files");
        assert!(git(&lane.worktree, &["remote"]).1.is_empty(), "no link back");
        assert_eq!(git(&lane.worktree, &["config", "--get", "user.email"]).1, "t@t", "the run's identity comes with the clone");
        assert!(git(&lane.worktree, &["config", "commit.gpgsign", "false"]).0);
        std::fs::write(lane.dir.join("new"), "from the lane").unwrap();
        assert!(git(&lane.worktree, &["add", "-A"]).0 && git(&lane.worktree, &["commit", "-qm", "lane work"]).0);
        assert_eq!(tip(r.path(), &lane), None, "not in the project's repository yet");
        fetch_back(r.path(), &lane).unwrap();
        assert_eq!(tip(r.path(), &lane).as_deref(), Some(git(&lane.worktree, &["rev-parse", "HEAD"]).1.as_str()));
        assert!(matches!(crate::lanes::join::join(r.path(), &lane), crate::lanes::join::Joined::Merged(_)));
        assert!(r.path().join("new").is_file());
        remove(r.path(), &lane);
        assert!(!lane.worktree.exists());
    }

    /// Seeded paths are copied (files and folders), the missing ones skipped.
    #[test]
    fn seeded_paths_are_copied_and_missing_ones_skipped() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(from.path().join("node_modules/pkg")).unwrap();
        std::fs::write(from.path().join("node_modules/pkg/i.js"), "1").unwrap();
        std::fs::write(from.path().join(".env"), "K=V").unwrap();
        seed(from.path(), to.path(), &["node_modules/".into(), ".env".into(), "absent".into()]);
        assert!(to.path().join("node_modules/pkg/i.js").is_file() && to.path().join(".env").is_file());
        assert!(!to.path().join("absent").exists());
    }
}
