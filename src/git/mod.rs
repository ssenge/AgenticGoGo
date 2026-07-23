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
mod session;
pub use session::*;

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

/// Discard the CURRENT branch's uncommitted modifications to TRACKED files. LEGACY: GIT_REDESIGN
/// replaced this on the normal staging path with `auto_commit_tracked` — agg now COMMITS the worker's
/// edits rather than discarding them (the worker never runs git). Retained for the (unused)
/// `resolve_session` path + its unit test. Only tracked modifications are reset (`git checkout -- .`)
/// — untracked files are left, and `agg/state/` runtime state is never touched. Returns true if there
/// was something to discard (for logging).
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

/// Commit the CURRENT (session) branch's uncommitted TRACKED edits — GIT_REDESIGN: agg owns git,
/// the worker just edits files and never runs git. Stages everything EXCEPT `agg/state/**` (runtime
/// state, gitignored + excluded via the moat pathspec) and commits IFF something is staged (no
/// `--allow-empty`: a no-op session makes no commit, exactly like a worker that changed nothing, and
/// a worker that DID commit leaves nothing staged so no extra commit is made). This REPLACES
/// `discard_uncommitted_tracked` on the normal path: the worker's work is now KEPT (committed) rather
/// than thrown away, which kills the "worker forgot to commit → work lost" failure (GIT_REDESIGN §2).
/// `--no-verify` so a project's pre-commit hook can't block agg's mechanical commit. Returns true if
/// it made a commit (for logging).
pub fn auto_commit_tracked(dir: &Path, message: &str) -> bool {
    // stage tracked modifications, deletions, AND new files — but never agg's runtime state.
    let _ = git(dir, &["add", "-A", "--", ".", ":(exclude)agg/state/**"]);
    // commit only if the index actually differs from HEAD (worker already committed / did nothing).
    if git(dir, &["diff", "--cached", "--name-only"]).1.is_empty() {
        return false;
    }
    let (ok, _, err) = git(dir, &["commit", "--no-verify", "--no-edit", "-m", message]);
    if !ok {
        eprintln!("  [git] agg auto-commit failed on the session branch: {err}");
    }
    ok
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

/// Ensure the project's `.gitignore` carries `agg/state/` (agg's runtime state must never get
/// committed onto session branches or merged into base) AND `.obsidian/` (so a user who opens the
/// `agg/` folder as an Obsidian vault — to visualize the LLM wiki — never commits Obsidian's config).
/// Idempotent, order-independent. MIGRATION: runtime state used to live at `<project>/.agg/`, so a
/// pre-move project ignores that now-stale path — we DROP it rather than leave two contradictory
/// lines. Only `agg/state/` is ignored under `agg/`: the committed config + judges must stay tracked.
/// Best-effort.
pub fn ensure_agg_gitignored(dir: &Path) {
    let gi = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    let has = |opts: &[&str]| existing.lines().any(|l| opts.contains(&l.trim()));
    let is_stale = |t: &str| matches!(t, ".agg" | ".agg/" | "/.agg" | "/.agg/");

    let has_state = has(&["agg/state", "agg/state/", "/agg/state", "/agg/state/"]);
    // `.obsidian/` (no leading slash) matches an Obsidian vault at ANY depth — root, `agg/`, or
    // `agg/state/` — so it covers whichever folder the user opens as the vault.
    let has_obsidian = has(&[".obsidian", ".obsidian/", "/.obsidian", "/.obsidian/"]);
    let has_stale = existing.lines().any(|l| is_stale(l.trim()));
    if has_state && has_obsidian && !has_stale {
        return; // both entries present, nothing stale — done
    }

    // Re-emit every line except the stale pre-move `.agg/` entry, then append whichever entries are
    // missing, so the file ends with exactly one (correct) copy of each, never a contradictory pair.
    let mut new: String = existing
        .lines()
        .filter(|l| !is_stale(l.trim()))
        .map(|l| format!("{l}\n"))
        .collect();
    if !has_state {
        new.push_str("agg/state/\n");
    }
    if !has_obsidian {
        new.push_str(".obsidian/\n");
    }
    let _ = std::fs::write(&gi, new);
    // also stop tracking runtime state if it was committed under the OLD layout (keeps files on
    // disk). Target `agg/state` ONLY — `git rm --cached agg` would untrack the whole committed
    // config + judge library, inverting the moat. (We do NOT untrack a `.obsidian/` a user chose to
    // commit — only ignore it going forward.)
    let _ = git(dir, &["rm", "-r", "--cached", "--quiet", "agg/state"]);
}


// ── per-session branch resolution: a pure decision + its execution ──────────────────────────
//
// What to do with a finished session's branch is the single highest-risk decision in the loop
// (it can lose or corrupt the worker's commits), so the DECISION is split out as a pure
// function over three booleans and unit-tested exhaustively. The loop only performs the I/O.







#[cfg(test)]
mod tests;
