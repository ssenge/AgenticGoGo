//! Higher-level per-session branch isolation: resolve/stage/finalize a finished session's branch.

use super::*;
use std::path::Path;

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
    // The worker's tracked edits were already committed on the session branch by the GitAutoCommit
    // handler (GIT_REDESIGN: agg owns git) — so there is nothing uncommitted to discard here. A truly
    // empty session (worker edited nothing) made no commit → `branch_has_no_new_commits` → NoChanges.
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
