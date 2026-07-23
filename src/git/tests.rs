//! git operations tests.

use super::*;

#[test]
fn session_branch_sanitizes() {
    assert_eq!(session_branch("agg", "telos-miplib28", 7), "agg/telos-miplib28/session-7");
    assert_eq!(session_branch("agg", "my proj!", 3), "agg/my-proj-/session-3");
}

// The merge/veto truth table — the highest-risk logic in the loop. `merge_ok` must NOT be
// consulted unless we're on base and not vetoed (else we'd attempt a merge we shouldn't).
#[test]
fn decide_checkout_failure_short_circuits_everything() {
    // off base → CheckoutFailed regardless of veto, and merge_ok must never run.
    assert_eq!(
        decide_session(false, false, || panic!("merge must not be attempted off-base")),
        SessionResolution::CheckoutFailed
    );
    assert_eq!(
        decide_session(true, false, || panic!("merge must not be attempted off-base")),
        SessionResolution::CheckoutFailed
    );
}

#[test]
fn decide_veto_discards_without_merging() {
    // on base + vetoed → Vetoed, and merge_ok must never run.
    assert_eq!(
        decide_session(true, true, || panic!("merge must not be attempted when vetoed")),
        SessionResolution::Vetoed
    );
}

#[test]
fn decide_clean_merge() {
    assert_eq!(decide_session(false, true, || true), SessionResolution::Merge);
}

#[test]
fn decide_merge_conflict_keeps_branch() {
    assert_eq!(decide_session(false, true, || false), SessionResolution::MergeConflict);
}

// ── rollback gate: real-git tests for stage_session / finalize_session ──────────────────────
use std::process::Command;

fn git_t(dir: &Path, args: &[&str]) {
    Command::new("git").args(args).current_dir(dir).output().unwrap();
}

/// A fresh repo with a `main` base commit + a session branch that adds a line. Returns the dir.
fn repo_with_session_branch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("agg-git-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    git_t(&d, &["config", "user.email", "t@t"]);
    git_t(&d, &["config", "user.name", "t"]);
    // isolate from the contributor's global git config (gpgsign/hooks would flake commits).
    git_t(&d, &["config", "commit.gpgsign", "false"]);
    git_t(&d, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "base"]);
    // session branch adds a line + commits.
    git_t(&d, &["checkout", "-q", "-b", "agg/p/session-1"]);
    std::fs::write(d.join("f.txt"), "base\nsession-work\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "session work"]);
    git_t(&d, &["checkout", "-q", "main"]);
    d
}

fn head_commit_count(dir: &Path) -> usize {
    let o = Command::new("git").args(["rev-list", "--count", "HEAD"]).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0)
}

#[test]
fn stage_then_keep_lands_the_work() {
    let d = repo_with_session_branch("keep");
    let before = head_commit_count(&d);
    let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
    assert_eq!(staged, StagedSession::Staged);
    // staged but not committed: the merged content is in the working tree, no new commit yet.
    assert!(std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    assert_eq!(head_commit_count(&d), before, "no commit while merely staged");
    // keep → commit lands it (a merge commit).
    let res = finalize_session(&d, "agg/p/session-1", 1, true);
    assert_eq!(res, SessionResolution::Merge);
    assert!(head_commit_count(&d) > before, "kept merge adds a commit");
    assert!(std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn stage_then_rollback_leaves_base_pristine() {
    let d = repo_with_session_branch("rollback");
    let before_count = head_commit_count(&d);
    let before_content = std::fs::read_to_string(d.join("f.txt")).unwrap();
    let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
    assert_eq!(staged, StagedSession::Staged);
    // roll back → base must be byte-for-byte pristine, no new commit, work NOT present.
    let res = finalize_session(&d, "agg/p/session-1", 1, false);
    assert_eq!(res, SessionResolution::MergeConflict);
    assert_eq!(head_commit_count(&d), before_count, "rollback adds no commit");
    assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), before_content, "base content pristine after rollback");
    assert!(!std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    // the session branch is kept for inspection.
    let branches = Command::new("git").args(["branch", "--list", "agg/p/session-1"]).current_dir(&d).output().unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).contains("session-1"), "branch kept after rollback");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn stage_respects_veto() {
    let d = repo_with_session_branch("veto");
    std::fs::write(d.join(".agg_red"), "").unwrap(); // worker vetoed
    let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
    assert_eq!(staged, StagedSession::Vetoed);
    assert!(!std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"), "veto: no merge");
    let _ = std::fs::remove_dir_all(&d);
}

/// W3: a session whose worker committed NOTHING must resolve as `NoChanges` — never enter the
/// stage/commit path (whose old failure fallback ran `reset --hard`, destroying uncommitted
/// work). Base is left exactly as-is and the empty branch is discarded.
#[test]
fn empty_session_resolves_as_no_changes_and_preserves_uncommitted_work() {
    let d = std::env::temp_dir().join(format!("agg-git-{}-nochanges", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    git_t(&d, &["config", "user.email", "t@t"]);
    git_t(&d, &["config", "user.name", "t"]);
    git_t(&d, &["config", "commit.gpgsign", "false"]);
    git_t(&d, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "base"]);
    // a session branch off base with NO new commits (worker did nothing).
    git_t(&d, &["branch", "agg/p/session-1"]);
    // the operator (or a killed worker) has some UNCOMMITTED work in the tree.
    std::fs::write(d.join("precious.txt"), "do not delete me\n").unwrap();

    let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
    assert_eq!(staged, StagedSession::NoChanges, "no-commit session must be NoChanges");
    // the uncommitted work must survive (the old reset --hard would have wiped it).
    assert_eq!(
        std::fs::read_to_string(d.join("precious.txt")).unwrap(),
        "do not delete me\n",
        "uncommitted work must be preserved on a no-op session"
    );
    // the empty branch is gone.
    let branches = Command::new("git").args(["branch", "--list", "agg/p/session-1"]).current_dir(&d).output().unwrap();
    assert!(!String::from_utf8_lossy(&branches.stdout).contains("session-1"), "empty branch discarded");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn branch_has_no_new_commits_detects_empty() {
    let d = repo_with_session_branch("nonew");
    // session-1 HAS a commit beyond main.
    assert!(!branch_has_no_new_commits(&d, "main", "agg/p/session-1"));
    // a fresh branch off main has none.
    git_t(&d, &["branch", "agg/p/session-2"]);
    assert!(branch_has_no_new_commits(&d, "main", "agg/p/session-2"));
    let _ = std::fs::remove_dir_all(&d);
}

/// GIT_REDESIGN (was "Blocker 2"): a worker that EDITS a tracked file but never commits used to
/// have its edits DISCARDED (and lost). Now agg owns git — `auto_commit_tracked` (run by the
/// GitAutoCommit handler on the session branch BEFORE staging) COMMITS the edit, so stage_session
/// sees a real commit and STAGES the merge: the work is KEPT. A truly empty session still commits
/// nothing → NoChanges, base pristine.
#[test]
fn auto_commit_keeps_the_worker_edit_and_stages_it() {
    let d = std::env::temp_dir().join(format!("agg-git-{}-autocommit", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    git_t(&d, &["config", "user.email", "t@t"]);
    git_t(&d, &["config", "user.name", "t"]);
    git_t(&d, &["config", "commit.gpgsign", "false"]);
    git_t(&d, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "base"]);
    // session branch, worker EDITS the tracked file but never commits.
    git_t(&d, &["checkout", "-q", "-b", "agg/p/session-1"]);
    std::fs::write(d.join("f.txt"), "base\nWORKER-UNCOMMITTED-EDIT\n").unwrap();

    // agg owns git: commit the worker's edit on the session branch (what GitAutoCommit does).
    assert!(auto_commit_tracked(&d, "agg: session 1 (worker) on fake"), "agg commits the worker's uncommitted edit");
    let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
    assert_eq!(staged, StagedSession::Staged, "the committed edit is a durable, stageable result");
    // keep it: the merge commits and the worker's edit lands on base — work KEPT, not lost.
    let _ = finalize_session(&d, "agg/p/session-1", 1, true);
    assert!(
        std::fs::read_to_string(d.join("f.txt")).unwrap().contains("WORKER-UNCOMMITTED-EDIT"),
        "the auto-committed worker edit must be KEPT on base (GIT_REDESIGN: agg owns git)"
    );

    // a truly EMPTY session (worker changed nothing) still commits nothing → NoChanges, base pristine.
    git_t(&d, &["checkout", "-q", "-b", "agg/p/session-2", "main"]);
    assert!(!auto_commit_tracked(&d, "agg: session 2 (worker) on fake"), "nothing changed → no commit");
    assert_eq!(
        stage_session(&d, "main", "agg/p/session-2", ".agg_red"),
        StagedSession::NoChanges,
        "empty session → NoChanges"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn discard_uncommitted_tracked_leaves_untracked() {
    let d = repo_with_session_branch("discard");
    git_t(&d, &["checkout", "-q", "agg/p/session-1"]);
    std::fs::write(d.join("f.txt"), "base\nsession-work\nEDIT\n").unwrap(); // modify tracked
    std::fs::write(d.join("new_untracked.txt"), "keep me\n").unwrap();     // untracked
    assert!(discard_uncommitted_tracked(&d), "should report there was something to discard");
    // tracked file reverted to the branch's committed state, untracked preserved.
    assert!(std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    assert!(!std::fs::read_to_string(d.join("f.txt")).unwrap().contains("EDIT"), "tracked edit discarded");
    assert!(d.join("new_untracked.txt").exists(), "untracked file preserved");
    let _ = std::fs::remove_dir_all(&d);
}

/// Blocker 4: a merge stranded by a crash mid-rollback-gate (MERGE_HEAD present) must be
/// detected and aborted at startup so isolation isn't silently disabled.
#[test]
fn recover_stranded_merge_aborts_an_agg_merge() {
    let d = repo_with_session_branch("stranded");
    // leave a staged agg merge in progress (the crash window).
    let staged = stage_merge(&d, "agg/p/session-1");
    assert_eq!(staged, StagedMerge::Staged);
    assert!(merge_in_progress(&d), "MERGE_HEAD present after stage_merge");
    // recovery aborts it (MERGE_MSG names an agg session branch).
    assert!(recover_stranded_merge(&d, "agg"), "should abort agg's stranded merge");
    assert!(!merge_in_progress(&d), "merge aborted — MERGE_HEAD cleared");
    assert!(is_clean(&d), "base clean after recovery");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn recover_leaves_a_non_agg_merge_alone() {
    let d = repo_with_session_branch("usermerge");
    // a merge the USER started by hand (a differently-named branch).
    git_t(&d, &["checkout", "-q", "-b", "my-feature", "main"]);
    std::fs::write(d.join("g.txt"), "feature\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "feature work"]);
    git_t(&d, &["checkout", "-q", "main"]);
    git_t(&d, &["merge", "--no-ff", "--no-commit", "my-feature"]);
    assert!(merge_in_progress(&d));
    // agg must NOT touch a merge it didn't start.
    assert!(!recover_stranded_merge(&d, "agg"), "must not abort a user's own merge");
    assert!(merge_in_progress(&d), "user's merge left intact");
    let _ = std::fs::remove_dir_all(&d);
}

/// §6.2 migration: a pre-move project already ignores the now-stale `.agg/`. The writer must
/// switch it to `agg/state/`, DROP the stale line (never leave two contradictory ones), keep
/// unrelated entries, and stay idempotent.
#[test]
fn ensure_agg_gitignored_migrates_the_stale_dot_agg_line() {
    let d = std::env::temp_dir().join(format!("agg-git-{}-gitignore", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    // a pre-move .gitignore: real content + the now-stale `.agg/` runtime entry.
    std::fs::write(d.join(".gitignore"), "target/\n.agg/\n").unwrap();

    ensure_agg_gitignored(&d);
    let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    let lines: Vec<&str> = gi.lines().map(str::trim).collect();
    assert!(lines.contains(&"agg/state/"), "new runtime path must be ignored: {gi:?}");
    assert!(lines.contains(&".obsidian/"), "the Obsidian vault config must be ignored too: {gi:?}");
    assert!(!lines.iter().any(|l| *l == ".agg/" || *l == ".agg"), "stale .agg/ line must be dropped: {gi:?}");
    assert!(lines.contains(&"target/"), "unrelated entries must survive: {gi:?}");

    // idempotent: a second call recognises both entries and appends nothing.
    ensure_agg_gitignored(&d);
    let gi2 = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    assert_eq!(gi2.matches("agg/state/").count(), 1, "must not append a duplicate: {gi2:?}");
    assert_eq!(gi2.matches(".obsidian/").count(), 1, "must not duplicate .obsidian/: {gi2:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A project that ALREADY ignores `agg/state/` (from an earlier agg version) but not `.obsidian/`
/// must still get `.obsidian/` added on the next call — the early-return must not skip it.
#[test]
fn ensure_agg_gitignored_adds_obsidian_to_an_already_migrated_project() {
    let d = std::env::temp_dir().join(format!("agg-git-{}-obsidian", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join(".gitignore"), "target/\nagg/state/\n").unwrap();

    ensure_agg_gitignored(&d);
    let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    let lines: Vec<&str> = gi.lines().map(str::trim).collect();
    assert!(lines.contains(&".obsidian/"), "must add .obsidian/ even when agg/state/ already present: {gi:?}");
    assert_eq!(gi.matches("agg/state/").count(), 1, "must not duplicate the existing agg/state/: {gi:?}");
    let _ = std::fs::remove_dir_all(&d);
}
