//! An interrupted session's work, saved under `refs/agg/partial/<run>/<session>` (A-18.3).
//!
//! A session that a rate limit or an operator cut short is INCOMPLETE: it is never judged, so it is
//! never merged — but the work is the operator's, and leaving it in the tree strands it (the next
//! session would sweep it into its own commit, and a new `agg run` refuses a dirty tree). So it is
//! committed to a ref outside every branch, and the tree goes back to the gated state.
//!
//! The commit is built through a THROWAWAY index (`GIT_INDEX_FILE`), so neither the real index nor
//! any branch moves: the snapshot is a side effect on the ref namespace only.

use super::{git, rev_parse};
use std::path::Path;
use std::process::Command;

/// agg's runtime state, excluded from every snapshot and every reset — it is not the worker's work.
const RUNTIME: [&str; 2] = [":(exclude)agg/state/**", ":(exclude)agg/private/**"];

/// The ref an interrupted session's work is saved under.
pub fn partial_ref(run: &str, session: u32) -> String {
    let run: String = run.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    format!("refs/agg/partial/{run}/{session}")
}

/// Untracked, non-ignored files in the work tree, agg's runtime state excluded.
pub fn untracked_files(dir: &Path) -> Vec<String> {
    // `-z`: NUL-separated and unquoted, so a name with a space or a non-ASCII byte comes back as is.
    let mut args = vec!["ls-files", "-z", "--others", "--exclude-standard", "--", "."];
    args.extend(RUNTIME);
    let (ok, out, _) = git(dir, &args);
    if !ok {
        return Vec::new();
    }
    out.split('\0').map(str::to_string).filter(|l| !l.is_empty()).collect()
}

/// Whether the work tree holds anything a session produced: a tracked or staged change, or an
/// untracked file that is not in `before` (the files present when the session started). `before:
/// None` counts every untracked file — after a crash agg no longer knows which were there.
pub fn has_uncommitted_work(dir: &Path, before: Option<&[String]>) -> bool {
    let mut args = vec!["status", "--porcelain", "--untracked-files=no", "--", "."];
    args.extend(RUNTIME);
    let (ok, out, _) = git(dir, &args);
    if ok && !out.is_empty() {
        return true;
    }
    untracked_files(dir).iter().any(|f| before.is_none_or(|b| !b.contains(f)))
}

/// Save HEAD plus everything uncommitted under [`partial_ref`] and return the ref.
///
/// `head_is_work` says HEAD itself carries the session's commits (a worker that committed on its
/// branch); then a clean tree is still worth a ref. `before` is as for [`has_uncommitted_work`].
/// Returns `None` when there is nothing to save or git refused — the caller resets only what was
/// saved, so a failure here loses nothing.
pub fn save_partial(dir: &Path, run: &str, session: u32, head_is_work: bool, before: Option<&[String]>) -> Option<String> {
    let head = rev_parse(dir, "HEAD")?;
    let commit = if has_uncommitted_work(dir, before) {
        snapshot_commit(dir, &head, run, session)?
    } else if head_is_work {
        head
    } else {
        return None;
    };
    let name = partial_ref(run, session);
    git(dir, &["update-ref", &name, &commit]).0.then_some(name)
}

/// A commit of the work tree as it stands, parented on `head`, built without touching the index.
fn snapshot_commit(dir: &Path, head: &str, run: &str, session: u32) -> Option<String> {
    let (ok, index, _) = git(dir, &["rev-parse", "--git-path", "agg-partial.index"]);
    if !ok {
        return None;
    }
    let index = dir.join(index);
    let _ = std::fs::remove_file(&index);
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(RUNTIME);
    let tree = git_with_index(dir, &index, &["read-tree", head]).and_then(|_| {
        // `add` exits 1 when an EXCLUDE pathspec names an ignored directory — AFTER staging
        // everything else (the quirk `auto_commit_tracked` lives with too) — so `write-tree` decides.
        let _ = git_with_index(dir, &index, &add);
        git_with_index(dir, &index, &["write-tree"])
    });
    let _ = std::fs::remove_file(&index);
    let msg = format!("agg: partial work of session {session} (run {run}) — interrupted, NOT judged");
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(["commit-tree", &tree?, "-p", head, "-m", &msg]);
    // A repository without an identity must not cost the operator their work: name agg instead.
    if git(dir, &["config", "user.email"]).1.is_empty() {
        cmd.env("GIT_AUTHOR_NAME", "agg").env("GIT_AUTHOR_EMAIL", "agg@localhost");
        cmd.env("GIT_COMMITTER_NAME", "agg").env("GIT_COMMITTER_EMAIL", "agg@localhost");
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run git against a private index file; `Some(stdout)` on success.
fn git_with_index(dir: &Path, index: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").current_dir(dir).env("GIT_INDEX_FILE", index).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Put the work tree back to HEAD: unstage, restore every tracked file, and remove the untracked
/// files that are NOT in `keep` (the ones that existed before the session). `keep: None` leaves
/// every untracked file alone — the crash path, where agg no longer knows which ones were the
/// session's. agg's runtime state is never touched.
pub fn reset_work_tree(dir: &Path, keep: Option<&[String]>) {
    let _ = git(dir, &["reset", "-q", "--", "."]);
    let mut restore = vec!["checkout", "--", "."];
    restore.extend(RUNTIME);
    let _ = git(dir, &restore);
    let Some(keep) = keep else { return };
    for f in untracked_files(dir) {
        if !keep.contains(&f) {
            let _ = std::fs::remove_file(dir.join(&f));
        }
    }
}
