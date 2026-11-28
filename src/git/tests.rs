//! git operations tests.

use super::*;

#[test]
fn session_branch_sanitizes() {
    assert_eq!(session_branch("rlt", "telos-miplib28", 7), "rlt/telos-miplib28/session-7");
    assert_eq!(session_branch("rlt", "my proj!", 3), "rlt/my-proj-/session-3");
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
    let d = std::env::temp_dir().join(format!("rlt-git-{}-{tag}", std::process::id()));
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
    git_t(&d, &["checkout", "-q", "-b", "rlt/p/session-1"]);
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
    let staged = stage_session(&d, "main", "rlt/p/session-1", ".rlt_red");
    assert_eq!(staged, StagedSession::Staged);
    // staged but not committed: the merged content is in the working tree, no new commit yet.
    assert!(std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    assert_eq!(head_commit_count(&d), before, "no commit while merely staged");
    // keep → commit lands it (a merge commit).
    let res = finalize_session(&d, "rlt/p/session-1", 1, true);
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
    let staged = stage_session(&d, "main", "rlt/p/session-1", ".rlt_red");
    assert_eq!(staged, StagedSession::Staged);
    // roll back → base must be byte-for-byte pristine, no new commit, work NOT present.
    let res = finalize_session(&d, "rlt/p/session-1", 1, false);
    assert_eq!(res, SessionResolution::MergeConflict);
    assert_eq!(head_commit_count(&d), before_count, "rollback adds no commit");
    assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), before_content, "base content pristine after rollback");
    assert!(!std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"));
    // the session branch is kept for inspection.
    let branches = Command::new("git").args(["branch", "--list", "rlt/p/session-1"]).current_dir(&d).output().unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).contains("session-1"), "branch kept after rollback");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn stage_respects_veto() {
    let d = repo_with_session_branch("veto");
    std::fs::write(d.join(".rlt_red"), "").unwrap(); // worker vetoed
    let staged = stage_session(&d, "main", "rlt/p/session-1", ".rlt_red");
    assert_eq!(staged, StagedSession::Vetoed);
    assert!(!std::fs::read_to_string(d.join("f.txt")).unwrap().contains("session-work"), "veto: no merge");
    let _ = std::fs::remove_dir_all(&d);
}

/// W3: a session whose worker committed NOTHING must resolve as `NoChanges` — never enter the
/// stage/commit path (whose old failure fallback ran `reset --hard`, destroying uncommitted
/// work). Base is left exactly as-is and the empty branch is discarded.
#[test]
fn empty_session_resolves_as_no_changes_and_preserves_uncommitted_work() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-nochanges", std::process::id()));
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
    git_t(&d, &["branch", "rlt/p/session-1"]);
    // the operator (or a killed worker) has some UNCOMMITTED work in the tree.
    std::fs::write(d.join("precious.txt"), "do not delete me\n").unwrap();

    let staged = stage_session(&d, "main", "rlt/p/session-1", ".rlt_red");
    assert_eq!(staged, StagedSession::NoChanges, "no-commit session must be NoChanges");
    // the uncommitted work must survive (the old reset --hard would have wiped it).
    assert_eq!(
        std::fs::read_to_string(d.join("precious.txt")).unwrap(),
        "do not delete me\n",
        "uncommitted work must be preserved on a no-op session"
    );
    // the empty branch is gone.
    let branches = Command::new("git").args(["branch", "--list", "rlt/p/session-1"]).current_dir(&d).output().unwrap();
    assert!(!String::from_utf8_lossy(&branches.stdout).contains("session-1"), "empty branch discarded");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn branch_has_no_new_commits_detects_empty() {
    let d = repo_with_session_branch("nonew");
    // session-1 HAS a commit beyond main.
    assert!(!branch_has_no_new_commits(&d, "main", "rlt/p/session-1"));
    // a fresh branch off main has none.
    git_t(&d, &["branch", "rlt/p/session-2"]);
    assert!(branch_has_no_new_commits(&d, "main", "rlt/p/session-2"));
    let _ = std::fs::remove_dir_all(&d);
}

/// GIT_REDESIGN (was "Blocker 2"): a worker that EDITS a tracked file but never commits used to
/// have its edits DISCARDED (and lost). Now rlt owns git — `auto_commit_tracked` (run by the
/// GitAutoCommit handler on the session branch BEFORE staging) COMMITS the edit, so stage_session
/// sees a real commit and STAGES the merge: the work is KEPT. A truly empty session still commits
/// nothing → NoChanges, base pristine.
#[test]
fn auto_commit_keeps_the_worker_edit_and_stages_it() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-autocommit", std::process::id()));
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
    git_t(&d, &["checkout", "-q", "-b", "rlt/p/session-1"]);
    std::fs::write(d.join("f.txt"), "base\nWORKER-UNCOMMITTED-EDIT\n").unwrap();

    // rlt owns git: commit the worker's edit on the session branch (what GitAutoCommit does).
    assert!(auto_commit_tracked(&d, "rlt: session 1 (worker) on fake"), "rlt commits the worker's uncommitted edit");
    let staged = stage_session(&d, "main", "rlt/p/session-1", ".rlt_red");
    assert_eq!(staged, StagedSession::Staged, "the committed edit is a durable, stageable result");
    // keep it: the merge commits and the worker's edit lands on base — work KEPT, not lost.
    let _ = finalize_session(&d, "rlt/p/session-1", 1, true);
    assert!(
        std::fs::read_to_string(d.join("f.txt")).unwrap().contains("WORKER-UNCOMMITTED-EDIT"),
        "the auto-committed worker edit must be KEPT on base (GIT_REDESIGN: rlt owns git)"
    );

    // a truly EMPTY session (worker changed nothing) still commits nothing → NoChanges, base pristine.
    git_t(&d, &["checkout", "-q", "-b", "rlt/p/session-2", "main"]);
    assert!(!auto_commit_tracked(&d, "rlt: session 2 (worker) on fake"), "nothing changed → no commit");
    assert_eq!(
        stage_session(&d, "main", "rlt/p/session-2", ".rlt_red"),
        StagedSession::NoChanges,
        "empty session → NoChanges"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn discard_uncommitted_tracked_leaves_untracked() {
    let d = repo_with_session_branch("discard");
    git_t(&d, &["checkout", "-q", "rlt/p/session-1"]);
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
fn recover_stranded_merge_aborts_an_rlt_merge() {
    let d = repo_with_session_branch("stranded");
    // leave a staged rlt merge in progress (the crash window).
    let staged = stage_merge(&d, "rlt/p/session-1");
    assert_eq!(staged, StagedMerge::Staged);
    assert!(merge_in_progress(&d), "MERGE_HEAD present after stage_merge");
    // recovery aborts it (MERGE_MSG names an rlt session branch).
    assert!(recover_stranded_merge(&d, "rlt"), "should abort rlt's stranded merge");
    assert!(!merge_in_progress(&d), "merge aborted — MERGE_HEAD cleared");
    assert!(is_clean(&d), "base clean after recovery");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn recover_leaves_a_non_rlt_merge_alone() {
    let d = repo_with_session_branch("usermerge");
    // a merge the USER started by hand (a differently-named branch).
    git_t(&d, &["checkout", "-q", "-b", "my-feature", "main"]);
    std::fs::write(d.join("g.txt"), "feature\n").unwrap();
    git_t(&d, &["add", "-A"]);
    git_t(&d, &["commit", "-qm", "feature work"]);
    git_t(&d, &["checkout", "-q", "main"]);
    git_t(&d, &["merge", "--no-ff", "--no-commit", "my-feature"]);
    assert!(merge_in_progress(&d));
    // rlt must NOT touch a merge it didn't start.
    assert!(!recover_stranded_merge(&d, "rlt"), "must not abort a user's own merge");
    assert!(merge_in_progress(&d), "user's merge left intact");
    let _ = std::fs::remove_dir_all(&d);
}

/// §6.2 migration: a pre-move project already ignores the now-stale `.rlt/`. The writer must
/// switch it to `rlt/state/`, DROP the stale line (never leave two contradictory ones), keep
/// unrelated entries, and stay idempotent.
#[test]
fn ensure_rlt_gitignored_migrates_the_stale_dot_rlt_line() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-gitignore", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    // a pre-move .gitignore: real content + the now-stale `.rlt/` runtime entry.
    std::fs::write(d.join(".gitignore"), "target/\n.rlt/\n").unwrap();

    ensure_rlt_gitignored(&d);
    let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    let lines: Vec<&str> = gi.lines().map(str::trim).collect();
    assert!(lines.contains(&"rlt/state/"), "new runtime path must be ignored: {gi:?}");
    assert!(lines.contains(&"rlt/private/"), "the rlt-owned runtime root must be ignored too: {gi:?}");
    assert!(lines.contains(&".obsidian/"), "the Obsidian vault config must be ignored too: {gi:?}");
    assert!(!lines.iter().any(|l| *l == ".rlt/" || *l == ".rlt"), "stale .rlt/ line must be dropped: {gi:?}");
    assert!(lines.contains(&"target/"), "unrelated entries must survive: {gi:?}");

    // idempotent: a second call recognises both entries and appends nothing.
    ensure_rlt_gitignored(&d);
    let gi2 = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    assert_eq!(gi2.matches("rlt/state/").count(), 1, "must not append a duplicate: {gi2:?}");
    assert_eq!(gi2.matches(".obsidian/").count(), 1, "must not duplicate .obsidian/: {gi2:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A project that ALREADY ignores `rlt/state/` (from an earlier rlt version) must still gain the
/// entries it is missing on the next call — the early-return must not skip them.
///
/// `rlt/private/` is the load-bearing one: it is the NEWER entry, so every existing project reaches
/// this branch, and the whole population of upgrading users would silently start committing rlt's
/// private state (the verdict ledger, the bus, run.log) if the writer stopped at `has_state`. The
/// `.obsidian/` case is the same early-return bug, found earlier.
#[test]
fn ensure_rlt_gitignored_adds_the_missing_entries_to_an_already_migrated_project() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-obsidian", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join(".gitignore"), "target/\nrlt/state/\n").unwrap();

    ensure_rlt_gitignored(&d);
    let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    let lines: Vec<&str> = gi.lines().map(str::trim).collect();
    assert!(lines.contains(&"rlt/private/"), "an upgrading project must pick up rlt/private/: {gi:?}");
    assert!(lines.contains(&".obsidian/"), "must add .obsidian/ even when rlt/state/ already present: {gi:?}");
    assert_eq!(gi.matches("rlt/state/").count(), 1, "must not duplicate the existing rlt/state/: {gi:?}");

    // idempotent: the second call sees all three and appends nothing.
    ensure_rlt_gitignored(&d);
    let gi2 = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    assert_eq!(gi2.matches("rlt/private/").count(), 1, "must not append a duplicate rlt/private/: {gi2:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// REGRESSION: rlt's own ignore entries must survive a ROLLBACK on a project whose base branch has
/// no committed `.gitignore`.
///
/// The mechanism, end to end: `ensure_rlt_gitignored` writes an UNTRACKED `.gitignore`; session 1's
/// auto-commit commits it on the session branch; checking base out for the gate and aborting the
/// rolled-back merge then DELETE it from the work tree (it is tracked on the session branch and
/// absent on base). From then on nothing ignored `rlt/private/`, and the next worker's own
/// `git add -A` committed rlt's ledger onto its session branch. The entries live in the
/// repository's `info/exclude` too, which no checkout, merge or reset ever touches.
#[test]
fn rlt_ignore_entries_survive_a_rollback_on_a_base_without_a_gitignore() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-exclude", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    git_t(&d, &["config", "user.email", "t@t"]);
    git_t(&d, &["config", "user.name", "t"]);
    git_t(&d, &["config", "commit.gpgsign", "false"]);
    git_t(&d, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    git_t(&d, &["add", "f.txt"]);
    git_t(&d, &["commit", "-qm", "base — no .gitignore"]);

    // run start: the ignore entries, then session 1 does work and rlt auto-commits it.
    ensure_rlt_gitignored(&d);
    assert!(create_branch(&d, "rlt/p/session-1", "main").is_ok());
    std::fs::create_dir_all(d.join("rlt/private")).unwrap();
    std::fs::write(d.join("rlt/private/state.json"), "{}").unwrap();
    std::fs::write(d.join("f.txt"), "base\nregressing work\n").unwrap();
    assert!(auto_commit_tracked(&d, "rlt: session 1"));
    // …and the gate rolls it back.
    assert_eq!(stage_session(&d, "main", "rlt/p/session-1", ".rlt_red"), StagedSession::Staged);
    finalize_session(&d, "rlt/p/session-1", 1, false);
    assert!(!d.join(".gitignore").exists(), "precondition: the rollback took the untracked .gitignore with it");

    // session 2's worker stages EVERYTHING it can see, as a worker running git would.
    assert!(create_branch(&d, "rlt/p/session-2", "main").is_ok());
    git_t(&d, &["add", "-A"]);
    let staged = Command::new("git").args(["diff", "--cached", "--name-only"]).current_dir(&d).output().unwrap();
    let staged = String::from_utf8_lossy(&staged.stdout).to_string();
    assert!(!staged.contains("rlt/private"), "rlt's private state must stay ignored after a rollback: {staged:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// The `info/exclude` writer is idempotent and keeps whatever the repository already excluded.
#[test]
fn ensure_rlt_excluded_appends_once_and_keeps_existing_entries() {
    let d = std::env::temp_dir().join(format!("rlt-git-{}-exclude-idem", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git_t(&d, &["init", "-q", "-b", "main"]);
    let exclude = d.join(".git/info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, "*.swp\n").unwrap();
    ensure_rlt_excluded(&d);
    ensure_rlt_excluded(&d);
    let text = std::fs::read_to_string(&exclude).unwrap();
    assert!(text.lines().any(|l| l == "*.swp"), "existing entries survive: {text:?}");
    for entry in RLT_IGNORED {
        assert_eq!(text.lines().filter(|l| *l == entry).count(), 1, "{entry} exactly once: {text:?}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A-18.3: an interrupted session's work — tracked edits AND new files — lands on its partial ref,
/// without moving the branch or the index; the reset then restores HEAD, removes the session's new
/// files, and leaves the untracked files that were there before the session.
#[test]
fn save_partial_keeps_the_work_on_a_ref_and_the_reset_restores_the_tree() {
    let d = repo_with_session_branch("partial");
    std::fs::write(d.join("operator-notes.txt"), "mine, untracked, before the session\n").unwrap();
    let before = untracked_files(&d);
    git_t(&d, &["checkout", "-q", "rlt/p/session-1"]);
    let head = rev_parse(&d, "HEAD").unwrap();
    std::fs::write(d.join("f.txt"), "half-finished edit\n").unwrap();
    std::fs::write(d.join("new.rs"), "fn wip() {}\n").unwrap();
    std::fs::create_dir_all(d.join("rlt/private")).unwrap();
    std::fs::write(d.join("rlt/private/state.json"), "{}").unwrap();

    assert!(has_uncommitted_work(&d, Some(&before)), "a tracked edit and a new file are the session's work");
    let r = save_partial(&d, "run-7", 3, false, Some(&before)).expect("dirty tree ⇒ a ref");
    assert_eq!(r, "refs/rlt/partial/run-7/3");
    assert_eq!(rev_parse(&d, "HEAD").unwrap(), head, "the branch does not move");
    let show = |spec: &str| {
        let o = Command::new("git").args(["show", spec]).current_dir(&d).output().unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    assert_eq!(show(&format!("{r}:f.txt")), "half-finished edit\n");
    assert_eq!(show(&format!("{r}:new.rs")), "fn wip() {}\n");
    assert!(show(&format!("{r}:rlt/private/state.json")).is_empty(), "rlt's runtime state is not the work");

    reset_work_tree(&d, Some(&before));
    assert_eq!(untracked_files(&d), before, "only the pre-session untracked files remain");
    assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "base\nsession-work\n", "tracked edit reverted");
    assert!(!d.join("new.rs").exists(), "the session's new file is gone from the tree (it is on the ref)");
    assert!(d.join("operator-notes.txt").exists(), "an untracked file from BEFORE the session survives");
    assert!(d.join("rlt/private/state.json").exists(), "rlt's runtime state is never reset");
    let _ = std::fs::remove_dir_all(&d);
}

/// Nothing uncommitted and no session commits ⇒ no ref; a session that only COMMITTED gets HEAD saved.
#[test]
fn save_partial_saves_nothing_from_a_clean_tree_unless_head_is_the_work() {
    let d = repo_with_session_branch("partial-clean");
    assert_eq!(save_partial(&d, "r", 1, false, None), None);
    // an untracked file from BEFORE the session is not the session's work…
    std::fs::write(d.join("notes.txt"), "x").unwrap();
    let before = untracked_files(&d);
    assert!(!has_uncommitted_work(&d, Some(&before)));
    assert_eq!(save_partial(&d, "r", 1, false, Some(&before)), None);
    // …but after a crash rlt cannot tell, so it counts.
    assert!(has_uncommitted_work(&d, None));
    std::fs::remove_file(d.join("notes.txt")).unwrap();
    git_t(&d, &["checkout", "-q", "rlt/p/session-1"]);
    let r = save_partial(&d, "r", 2, true, None).expect("the session's own commit is work");
    assert_eq!(rev_parse(&d, &r), rev_parse(&d, "HEAD"));
    // crash path: no knowledge of the pre-session files ⇒ untracked files are left alone.
    std::fs::write(d.join("stray.txt"), "x").unwrap();
    reset_work_tree(&d, None);
    assert!(d.join("stray.txt").exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// A run that ends on a session branch checks the base out again, the session's commit kept on its branch; a dirty
/// tree stays where it is, and a run on its base moves nothing.
#[test]
fn back_to_base_leaves_the_session_branch_at_run_end() {
    let d = repo_with_session_branch("back-to-base");
    assert_eq!(back_to_base(&d, "main"), None);
    git_t(&d, &["checkout", "-q", "rlt/p/session-1"]);
    std::fs::write(d.join("f.txt"), "dirty\n").unwrap();
    assert!(back_to_base(&d, "main").unwrap().contains("uncommitted"));
    assert_eq!(current_branch(&d).as_deref(), Some("rlt/p/session-1"));
    git_t(&d, &["checkout", "--", "f.txt"]);
    assert!(back_to_base(&d, "main").unwrap().contains("checked out main again"));
    assert_eq!(current_branch(&d).as_deref(), Some("main"));
    assert_eq!(head_commit_count(&d), 1);
    let _ = std::fs::remove_dir_all(&d);
}

/// A session branch's step is noted when it is cut and survives parking (a rename); a partial ref's step is noted too. A
/// later session of the same step finds both, newest first and at most [`LIMIT`], with what each changed against its
/// base; another step's work, and work HEAD already has, are not found.
#[test]
fn a_stopped_sessions_work_is_found_again_by_its_step() {
    let d = repo_with_session_branch("stopped");
    note_branch_step(&d, "rlt/p/session-1", "1.3 Survey");
    assert!(rename_branch(&d, "rlt/p/session-1", "rlt/p/orphaned-5/session-1"), "parking keeps the note");
    let found = stopped_work(&d, "1.3 Survey");
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].name.as_str(), found[0].base.len()), ("rlt/p/orphaned-5/session-1", 12));
    assert!(found[0].stat.contains("f.txt"), "{}", found[0].stat);
    assert!(stopped_work(&d, "2.1 Other").is_empty(), "another step's work is not offered");
    // a partial ref of the same step: saved from the tree, its step noted
    std::fs::write(d.join("new.rs"), "fn wip() {}\n").unwrap();
    let r = save_partial(&d, "run-7", 3, false, None).unwrap();
    note_ref_step(&d, &r, "1.3 Survey");
    let names: Vec<String> = stopped_work(&d, "1.3 Survey").into_iter().map(|s| s.name).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&r) && names.contains(&"rlt/p/orphaned-5/session-1".to_string()), "{names:?}");
    // the newest three of many
    for n in 0..3 {
        let to = format!("rlt/p/orphaned-{n}/session-9");
        git_t(&d, &["branch", &to, "rlt/p/orphaned-5/session-1"]);
        git_t(&d, &["config", &format!("branch.{to}.rlt-step"), "1.3 Survey"]);
    }
    assert_eq!(stopped_work(&d, "1.3 Survey").len(), LIMIT);
    // taken: once HEAD has the files as the sessions left them, nothing is left to offer
    git_t(&d, &["checkout", "-q", "rlt/p/orphaned-5/session-1", "--", "f.txt"]);
    git_t(&d, &["commit", "-qam", "took the file"]);
    git_t(&d, &["checkout", "-q", &r, "--", "new.rs"]);
    git_t(&d, &["commit", "-qam", "took the other"]);
    assert!(stopped_work(&d, "1.3 Survey").is_empty(), "{:?}", stopped_work(&d, "1.3 Survey"));
    let _ = std::fs::remove_dir_all(&d);
}

/// A long `git diff --stat` is cut to 40 lines and says how many it left out.
#[test]
fn a_long_stat_is_cut() {
    let long = (0..45).map(|n| format!(" f{n} | 1 +")).collect::<Vec<_>>().join("\n");
    let cut = truncated(&long);
    assert_eq!(cut.lines().count(), 41);
    assert!(cut.ends_with("… (5 more lines)"), "{cut}");
    assert_eq!(truncated(" a | 1 +"), " a | 1 +");
}

// ── a linked worktree: `.git` there is a file, so what lives in the git dir is looked up, not assumed ──

/// A linked worktree of `main` (on a branch of its own) beside the repository `d`; its path.
fn linked_worktree(d: &Path, tag: &str) -> std::path::PathBuf {
    let wt = std::env::temp_dir().join(format!("rlt-git-{}-{tag}-wt", std::process::id()));
    let _ = std::fs::remove_dir_all(&wt);
    git_t(d, &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap(), "main"]);
    wt
}

/// In a linked worktree the git dir is the worktree's own, the common dir is the repository's, and a stranded rlt merge
/// there is found and aborted (it read `.git/MERGE_MSG`, which isn't a path in a worktree).
#[test]
fn a_linked_worktree_has_its_own_git_dir_and_recovers_its_stranded_merge() {
    let d = repo_with_session_branch("wt");
    let wt = linked_worktree(&d, "wt");
    assert!(wt.join(".git").is_file(), "a linked worktree's .git is a file");
    assert_ne!(git_dir(&wt), common_dir(&wt));
    assert!(git_dir(&wt).to_string_lossy().contains("worktrees"), "{:?}", git_dir(&wt));
    assert_eq!(common_dir(&wt).canonicalize().unwrap(), d.join(".git").canonicalize().unwrap());
    assert_eq!(git_dir(&d).canonicalize().unwrap(), d.join(".git").canonicalize().unwrap());
    assert_eq!(stage_merge(&wt, "rlt/p/session-1"), StagedMerge::Staged);
    assert!(recover_stranded_merge(&wt, "rlt"), "the stranded merge of a linked worktree is aborted");
    assert!(!merge_in_progress(&wt) && is_clean(&wt));
    let _ = std::fs::remove_dir_all(&wt);
    let _ = std::fs::remove_dir_all(&d);
}

/// Two threads that write git config at once both land: a held `config.lock` is waited for, not lost.
#[test]
fn concurrent_config_writes_all_land() {
    let d = repo_with_session_branch("config");
    std::thread::scope(|s| {
        for t in 0..4 {
            let d = &d;
            s.spawn(move || {
                for i in 0..10 {
                    stopped::note_branch_step(d, &format!("b{t}-{i}"), &format!("step-{t}-{i}"));
                }
            });
        }
    });
    let (_, out, _) = git(&d, &["config", "--get-regexp", r"^branch\..*\.rlt-step$"]);
    assert_eq!(out.lines().count(), 40, "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

/// What git says about a lock is recognised, other failures are not retried.
#[test]
fn only_a_held_lock_is_tried_again() {
    assert!(locked("error: could not lock config file .git/config: File exists"));
    assert!(locked("fatal: Unable to create '/r/.git/refs/heads/x.lock': File exists."));
    assert!(!locked("fatal: not a git repository"));
    assert!(!locked("error: pathspec 'x' did not match"));
}

/// A branch that can't be cut says why, and the caller refuses to go on unisolated.
#[test]
fn a_branch_that_cannot_be_cut_is_an_error_with_gits_words() {
    let d = repo_with_session_branch("nocut");
    let err = create_branch(&d, "rlt/p/session-9", "no-such-base").unwrap_err();
    assert!(err.contains("failed to create session branch rlt/p/session-9 from no-such-base"), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}
