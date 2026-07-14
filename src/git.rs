//! Per-session git isolation primitives.
//!
//! Session isolation is MANDATORY: the loop runs each worker session on its own branch
//! (`<prefix>/<project>/session-<N>`) cut from a base branch. After the session, the branch
//! is merged back into the base UNLESS the worker vetoed it (wrote the red file), in which
//! case the branch is discarded and the base is untouched.
//!
//! All operations shell out to `git` (no libgit2 dependency — agg stays lean). Every call is
//! best-effort-logged: a git failure is surfaced but never panics the loop. Isolation can only
//! proceed on a usable repo (a git repo, a clean tree, a non-detached HEAD), so `agg run`
//! REFUSES to start otherwise (see `loop_::run`) rather than degrading — isolation is the
//! product's correctness guarantee, not an enhancement.

use std::path::Path;
use std::process::Command;

/// Run a git command in `dir`, returning (success, stdout-trimmed, stderr-trimmed).
fn git(dir: &Path, args: &[&str]) -> (bool, String, String) {
    match Command::new("git").current_dir(dir).args(args).output() {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (false, String::new(), e.to_string()),
    }
}

/// Is `dir` inside a git work tree?
pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"]).1 == "true"
}

/// The current branch name, or None if detached / not a repo.
pub fn current_branch(dir: &Path) -> Option<String> {
    let (ok, out, _) = git(dir, &["symbolic-ref", "--short", "HEAD"]);
    if ok && !out.is_empty() {
        Some(out)
    } else {
        None
    }
}

/// Is the work tree clean enough to branch (no tracked modifications OUTSIDE agg's own runtime
/// state)? Untracked files are allowed (they carry across a checkout). agg's `agg/state/` runtime
/// state (state.json/project.json/run.pid) churns every cycle and MUST NOT count as dirty —
/// it's runtime, not project content (and gitignored). We exclude it via a pathspec. The
/// pathspec is `agg/state/**` (NOT `agg`): the committed config + judge library under `agg/`
/// must still be checked, only `agg/state/` is runtime churn.
pub fn is_clean(dir: &Path) -> bool {
    // pathspec `:(exclude)agg/state/**` drops agg's state churn; `--untracked-files=no` ignores untracked.
    git(
        dir,
        &["status", "--porcelain", "--untracked-files=no", "--", ".", ":(exclude)agg/state/**"],
    )
    .1
    .is_empty()
}

/// Is a merge in progress in `dir` (MERGE_HEAD present)? The rollback gate stages a merge with
/// `merge --no-ff --no-commit` and only finalizes it AFTER the (minutes-long) judging phase — so
/// a crash/Ctrl-C/kill in that window leaves the repo mid-merge. On the next run that makes
/// `is_clean` false, which would silently disable isolation and let the next worker build on a
/// half-merged tree.
pub fn merge_in_progress(dir: &Path) -> bool {
    // `git rev-parse -q --verify MERGE_HEAD` exits 0 iff a merge is in progress.
    git(dir, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).0
}

/// If a leftover staged merge from a crashed rollback-gate cycle is present, abort it so the repo
/// returns to a clean base. Only aborts an agg-owned merge (one whose branch heads under the
/// session branch prefix exist / whose MERGE_MSG names an agg session) — a merge the USER started
/// by hand is left untouched and reported. Call at loop startup, BEFORE the is_clean check.
/// Returns true if it aborted something (for logging). Best-effort.
pub fn recover_stranded_merge(dir: &Path, branch_prefix: &str) -> bool {
    if !merge_in_progress(dir) {
        return false;
    }
    // Is this agg's merge? Check MERGE_MSG for our merge-commit signature or the session prefix.
    let msg = std::fs::read_to_string(dir.join(".git").join("MERGE_MSG")).unwrap_or_default();
    let is_aggs = msg.contains("agg: merge session") || msg.contains(branch_prefix);
    if is_aggs {
        eprintln!("  [iso] found a leftover staged merge from an interrupted session — aborting it to restore a clean base");
        let _ = git(dir, &["merge", "--abort"]);
        true
    } else {
        eprintln!("  [iso] WARNING a merge is in progress that agg did not start (MERGE_HEAD present) — leaving it alone; resolve it, then re-run");
        false
    }
}

/// Create + checkout `branch` from `base`. Returns true on success.
pub fn create_branch(dir: &Path, branch: &str, base: &str) -> bool {
    // delete a stale same-named branch first (a prior crashed session) so -b doesn't fail.
    let _ = git(dir, &["branch", "-D", branch]);
    let (ok, _, err) = git(dir, &["checkout", "-b", branch, base]);
    if !ok {
        eprintln!("  [git] failed to create session branch {branch} from {base}: {err}");
    }
    ok
}

/// Checkout an existing branch. Returns true on success.
pub fn checkout(dir: &Path, branch: &str) -> bool {
    let (ok, _, err) = git(dir, &["checkout", branch]);
    if !ok {
        eprintln!("  [git] failed to checkout {branch}: {err}");
    }
    ok
}

/// Discard the CURRENT branch's uncommitted modifications to TRACKED files, so they don't leak
/// onto base at the next `checkout base` and get judged as a real result. Called on the session
/// branch after the worker exits: the worker was told to commit its work, so anything still
/// uncommitted is not a durable result (the exact out-of-context-stop case). Only tracked
/// modifications are reset (`git checkout -- .`) — untracked files (build artifacts a judge may
/// read) are left, and `agg/state/` runtime state is never touched. Returns true if there was
/// something to discard (for logging).
pub fn discard_uncommitted_tracked(dir: &Path) -> bool {
    let dirty = !git(
        dir,
        &["status", "--porcelain", "--untracked-files=no", "--", ".", ":(exclude)agg/state/**"],
    )
    .1
    .is_empty();
    if dirty {
        // restore tracked files to HEAD of the (session) branch; pathspec form leaves untracked
        // files and can't over-reach into base — we are still ON the session branch here. The
        // exclude is `agg/state/**` ONLY: the committed `agg/` config + judges MUST be restored
        // (a rolled-back session must not keep the worker's edits to the judges that graded it).
        let _ = git(dir, &["checkout", "--", ".", ":(exclude)agg/state/**"]);
    }
    dirty
}

/// Merge `branch` into the currently-checked-out branch (no-ff so each session is one merge
/// commit in the history). Returns true on a clean merge; on conflict, aborts and returns false.
pub fn merge_no_ff(dir: &Path, branch: &str, message: &str) -> bool {
    let (ok, _, err) = git(dir, &["merge", "--no-ff", "-m", message, branch]);
    if !ok {
        eprintln!("  [git] merge of {branch} hit a conflict/error ({err}); aborting merge");
        let _ = git(dir, &["merge", "--abort"]);
        return false;
    }
    true
}

/// Outcome of staging a merge (the first half of the rollback gate). `Staged` means the merge
/// applied cleanly but is NOT yet committed — the caller must re-test the working tree and then
/// call `commit_merge` (keep) or `abort_merge` (roll back). `Conflict` means the merge couldn't
/// apply and was already aborted (nothing staged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedMerge {
    Staged,
    Conflict,
}

/// Stage `branch` into the current branch WITHOUT committing (`merge --no-ff --no-commit`), so the
/// caller can re-test the merged working tree before deciding to keep or roll back. On a conflict
/// the merge is aborted and `Conflict` is returned (base untouched, no half-merge left behind).
/// `--no-ff` so a clean merge still stages a merge commit (consistent with `merge_no_ff`); even a
/// fast-forwardable merge leaves the index/worktree at the merged state for the re-test.
pub fn stage_merge(dir: &Path, branch: &str) -> StagedMerge {
    let (ok, _, err) = git(dir, &["merge", "--no-ff", "--no-commit", branch]);
    if !ok {
        eprintln!("  [git] merge of {branch} hit a conflict/error ({err}); aborting merge");
        let _ = git(dir, &["merge", "--abort"]);
        return StagedMerge::Conflict;
    }
    StagedMerge::Staged
}

/// Commit a previously-`stage_merge`d merge (the keep path of the rollback gate). Returns true on
/// success. A real staged `--no-ff --no-commit` merge always leaves `MERGE_HEAD` + a staged tree,
/// so a plain commit succeeds; the no-commits-to-merge case is filtered out earlier by
/// `stage_session` (→ `NoChanges`) and never reaches here.
pub fn commit_merge(dir: &Path, message: &str) -> bool {
    git(dir, &["commit", "--no-edit", "-m", message]).0
}

/// True when `branch` has no commits beyond `base` (`git rev-list --count base..branch == 0`) — a
/// session where the worker committed nothing. Under the gate, merging such a branch is a no-op
/// (`Already up to date`) that leaves NOTHING staged, so it must be handled as `NoChanges` rather
/// than a stageable merge (otherwise `commit_merge` fails and the old fallback ran `reset --hard`,
/// which would destroy any uncommitted work in the tree).
pub fn branch_has_no_new_commits(dir: &Path, base: &str, branch: &str) -> bool {
    let (ok, out, _) = git(dir, &["rev-list", "--count", &format!("{base}..{branch}")]);
    ok && out.trim() == "0"
}

/// Abort/roll back a staged (uncommitted) merge — the rollback path of the gate, used when the
/// post-merge re-test regresses. Restores the working tree + index to the pre-merge base state.
///
/// A `--no-ff --no-commit` merge that actually staged anything always leaves `MERGE_HEAD`, so
/// `git merge --abort` is sufficient and precise. We deliberately do NOT fall back to
/// `reset --hard HEAD`: the only way to reach that fallback is the nothing-was-staged case, where
/// a hard reset can ONLY destroy unrelated uncommitted work in the tree (the exact W3 data-loss
/// bug). The no-op case is filtered earlier by `stage_session` → `NoChanges`, so it never gets here.
pub fn abort_merge(dir: &Path) -> bool {
    git(dir, &["merge", "--abort"]).0
}

/// Delete a branch unconditionally (-D). Used to discard a vetoed/merged session branch.
pub fn delete_branch(dir: &Path, branch: &str) -> bool {
    git(dir, &["branch", "-D", branch]).0
}

/// Does `path` exist relative to `dir`? (for the red-file veto check)
pub fn file_exists(dir: &Path, path: &str) -> bool {
    dir.join(path).exists()
}

/// Remove a file relative to `dir` if present (clearing a stale red veto before a session).
pub fn remove_file(dir: &Path, path: &str) {
    let _ = std::fs::remove_file(dir.join(path));
}

/// Ensure `agg/state/` is gitignored (so agg's runtime state never gets committed onto session
/// branches or merged into base). Idempotent. MIGRATION: runtime state used to live at
/// `<project>/.agg/`, so a pre-move project already ignores that now-stale path — we DROP it while
/// writing the new entry rather than leave two contradictory lines. Only `agg/state/` is ignored:
/// the committed config + judge library under `agg/` must stay tracked. Best-effort.
pub fn ensure_agg_gitignored(dir: &Path) {
    let gi = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    let is_new = |t: &str| matches!(t, "agg/state" | "agg/state/" | "/agg/state" | "/agg/state/");
    let is_stale = |t: &str| matches!(t, ".agg" | ".agg/" | "/.agg" | "/.agg/");
    if existing.lines().any(|l| is_new(l.trim())) {
        return; // already migrated
    }
    // Re-emit every line except the stale pre-move `.agg/` entry, then append the new one, so a
    // migrated project ends with exactly one (correct) entry, never two contradictory ones.
    let mut new: String = existing
        .lines()
        .filter(|l| !is_stale(l.trim()))
        .map(|l| format!("{l}\n"))
        .collect();
    new.push_str("agg/state/\n");
    let _ = std::fs::write(&gi, new);
    // also stop tracking runtime state if it was committed under the OLD layout (keeps files on
    // disk). Target `agg/state` ONLY — `git rm --cached agg` would untrack the whole committed
    // config + judge library, inverting the moat.
    let _ = git(dir, &["rm", "-r", "--cached", "--quiet", "agg/state"]);
}

/// The session branch name for a given project + session number.
pub fn session_branch(prefix: &str, project: &str, session: u32) -> String {
    // sanitize project for a git ref (no spaces / odd chars).
    let proj: String = project
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("{prefix}/{proj}/session-{session}")
}

// ── per-session branch resolution: a pure decision + its execution ──────────────────────────
//
// What to do with a finished session's branch is the single highest-risk decision in the loop
// (it can lose or corrupt the worker's commits), so the DECISION is split out as a pure
// function over three booleans and unit-tested exhaustively. The loop only performs the I/O.

/// What should happen to a finished session's branch, decided from the post-session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionResolution {
    /// Couldn't get back onto the base branch — leave the session branch in place, untouched,
    /// so nothing is lost and a human can inspect.
    CheckoutFailed,
    /// The worker vetoed (wrote the red file): discard the branch, base untouched.
    Vetoed,
    /// Default: merge the branch back into the base (no-ff), then delete it.
    Merge,
    /// Tried to merge but hit a conflict: keep the branch for inspection, base unchanged.
    MergeConflict,
}

/// Pure decision: given whether the worker vetoed, whether we got back onto the base branch,
/// and (only consulted when we merge) whether the merge succeeded, decide the outcome.
///
/// `merge_ok` is a thunk so the (side-effecting) merge is only attempted when the earlier
/// gates pass — keeping this function itself pure/total over its inputs.
pub fn decide_session(vetoed: bool, on_base: bool, merge_ok: impl FnOnce() -> bool) -> SessionResolution {
    if !on_base {
        SessionResolution::CheckoutFailed
    } else if vetoed {
        SessionResolution::Vetoed
    } else if merge_ok() {
        SessionResolution::Merge
    } else {
        SessionResolution::MergeConflict
    }
}

/// Resolve a finished session's branch: run the decision, perform its git side-effects, and
/// return the resolution (for logging). `base`/`branch` are the base + session branch names;
/// `red_file` is the worker's veto marker. Drives `checkout`/`merge_no_ff`/`delete_branch`.
///
/// This is the EAGER-COMMIT path (no rollback gate): a clean merge is committed immediately. Used
/// when `rollback_on_regression` is off. For the gated path see `stage_session` + `finalize_session`.
pub fn resolve_session(
    dir: &Path,
    base: &str,
    branch: &str,
    red_file: &str,
    session: u32,
) -> SessionResolution {
    let vetoed = file_exists(dir, red_file);
    // Discard the session's uncommitted tracked edits BEFORE leaving the branch — otherwise git
    // carries them onto base at checkout and they'd be judged/merged as if they were committed work.
    if discard_uncommitted_tracked(dir) {
        eprintln!("  [iso] session #{session} left uncommitted edits — discarding (commit your work to keep it)");
    }
    // back to base before merge/discard (git ops require not being on the branch we delete).
    let on_base = checkout(dir, base);
    let merge_msg = format!("agg: merge session #{session} ({branch})");
    let res = decide_session(vetoed, on_base, || merge_no_ff(dir, branch, &merge_msg));
    match res {
        SessionResolution::CheckoutFailed => {
            eprintln!("  [iso] WARNING could not checkout base '{base}'; leaving session branch {branch} in place");
        }
        SessionResolution::Vetoed => {
            eprintln!("  [iso] session #{session} VETOED (worker wrote {red_file}) → discarding branch {branch}");
            remove_file(dir, red_file); // don't let the veto persist on base
            delete_branch(dir, branch);
        }
        SessionResolution::Merge => {
            eprintln!("  [iso] session #{session} merged → {base}");
            delete_branch(dir, branch);
        }
        SessionResolution::MergeConflict => {
            eprintln!("  [iso] session #{session} merge FAILED (conflict) — branch {branch} kept for inspection, base unchanged");
        }
    }
    res
}

/// What the loop is mid-way through after `stage_session`, so `finalize_session` knows what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedSession {
    /// Couldn't checkout base — nothing staged; branch left in place (mirror of CheckoutFailed).
    CheckoutFailed,
    /// Worker vetoed — branch already discarded, nothing staged.
    Vetoed,
    /// Merge couldn't apply — already aborted, branch kept for inspection, base unchanged.
    Conflict,
    /// The worker committed nothing (no commits beyond base) — there is nothing to stage, judge,
    /// or commit. The branch is discarded and base is left exactly as-is. Treated like `Vetoed`:
    /// no merge, no rollback, no `finalize_session` call.
    NoChanges,
    /// Merge applied and is STAGED (uncommitted). The loop must judge the merged tree, then call
    /// `finalize_session` to commit (keep) or roll back. `branch` carried for the keep/discard.
    Staged,
}

/// First half of the ROLLBACK GATE: get onto base, then STAGE the session's merge without
/// committing (so the loop can re-test the merged working tree before keeping it). Mirrors
/// `resolve_session`'s veto/checkout decision but stops at a staged (uncommitted) merge on the
/// merge path. The companion `finalize_session` commits or rolls back.
pub fn stage_session(dir: &Path, base: &str, branch: &str, red_file: &str) -> StagedSession {
    let vetoed = file_exists(dir, red_file);
    // Discard the session's uncommitted tracked edits BEFORE leaving the branch — otherwise git
    // carries them onto base at checkout and the judges would score them as a real (merged) result
    // even though the branch has no commits (→ NoChanges). Uncommitted == not a durable result.
    if discard_uncommitted_tracked(dir) {
        eprintln!("  [iso] session left uncommitted edits — discarding (commit your work to keep it)");
    }
    if !checkout(dir, base) {
        eprintln!("  [iso] WARNING could not checkout base '{base}'; leaving session branch {branch} in place");
        return StagedSession::CheckoutFailed;
    }
    if vetoed {
        eprintln!("  [iso] session VETOED (worker wrote {red_file}) → discarding branch {branch}");
        remove_file(dir, red_file);
        delete_branch(dir, branch);
        return StagedSession::Vetoed;
    }
    // No commits beyond base → nothing to merge. Merging would be a no-op that stages nothing,
    // so we must NOT enter the stage/commit path (commit would fail and the old rollback fallback
    // ran `reset --hard`, destroying any uncommitted work — W3). Discard the empty branch cleanly.
    if branch_has_no_new_commits(dir, base, branch) {
        eprintln!("  [iso] session made no commits → discarding empty branch {branch}, base unchanged");
        delete_branch(dir, branch);
        return StagedSession::NoChanges;
    }
    match stage_merge(dir, branch) {
        StagedMerge::Staged => StagedSession::Staged,
        StagedMerge::Conflict => {
            eprintln!("  [iso] merge of {branch} FAILED (conflict) — branch {branch} kept for inspection, base unchanged");
            StagedSession::Conflict
        }
    }
}

/// Second half of the ROLLBACK GATE: after judging a staged merge, KEEP it (commit + delete the
/// branch) or ROLL IT BACK (abort the staged merge, leave base untouched, keep the branch for
/// inspection). Only meaningful after `stage_session` returned `Staged`.
pub fn finalize_session(dir: &Path, branch: &str, session: u32, keep: bool) -> SessionResolution {
    if keep {
        let merge_msg = format!("agg: merge session #{session} ({branch})");
        if commit_merge(dir, &merge_msg) {
            eprintln!("  [iso] session #{session} merged → kept (post-merge re-test passed)");
            delete_branch(dir, branch);
            SessionResolution::Merge
        } else {
            // committing a staged, conflict-free merge should not fail; if it does, roll back to be safe.
            eprintln!("  [iso] session #{session} commit of staged merge FAILED — rolling back, branch {branch} kept");
            abort_merge(dir);
            SessionResolution::MergeConflict
        }
    } else {
        eprintln!("  [iso] session #{session} ROLLED BACK (post-merge re-test regressed) — base unchanged, branch {branch} kept for inspection");
        abort_merge(dir);
        SessionResolution::MergeConflict
    }
}

#[cfg(test)]
mod tests {
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

    /// Blocker 2: a worker that EDITS a tracked file but never commits must not have those edits
    /// leak onto base at `checkout base` and be judged as a real result. stage_session must
    /// discard them → NoChanges, base pristine.
    #[test]
    fn uncommitted_tracked_edits_do_not_leak_onto_base() {
        let d = std::env::temp_dir().join(format!("agg-git-{}-leak", std::process::id()));
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

        let staged = stage_session(&d, "main", "agg/p/session-1", ".agg_red");
        assert_eq!(staged, StagedSession::NoChanges, "no commits → NoChanges");
        // base's working tree must NOT carry the worker's uncommitted edit.
        assert_eq!(
            std::fs::read_to_string(d.join("f.txt")).unwrap(),
            "base\n",
            "base must be pristine — the uncommitted edit must not leak/merge"
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
        assert!(!lines.iter().any(|l| *l == ".agg/" || *l == ".agg"), "stale .agg/ line must be dropped: {gi:?}");
        assert!(lines.contains(&"target/"), "unrelated entries must survive: {gi:?}");

        // idempotent: a second call recognises the new spelling and appends nothing.
        ensure_agg_gitignored(&d);
        let gi2 = std::fs::read_to_string(d.join(".gitignore")).unwrap();
        assert_eq!(gi2.matches("agg/state/").count(), 1, "must not append a duplicate: {gi2:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
