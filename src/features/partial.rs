//! The partial-work feature (A-18.3, decision 5): a session cut short — by a rate limit or an
//! interrupt — is incomplete, so it is never judged. Its work is saved under
//! `refs/agg/partial/<run>/<session>` and the tree goes back to where the session started.

use crate::features::inject::base_ref;
use crate::loop_::{AGGState, LoopState};

/// Remember the untracked files that exist right before the worker starts, so a later reset removes
/// only the files the SESSION created. Called by `LaunchWorker`.
pub fn note_untracked_before(ctx: &mut LoopState) {
    let files = crate::git::untracked_files(&ctx.dir);
    ctx.ext.get::<AGGState>().git.untracked_before = Some(files);
}

/// Save the current session's unjudged work to its partial ref, reset the tree, and step off the
/// session branch. Returns the ref when anything was saved.
///
/// The session branch is deleted only when nothing on it can be lost: it carried no commits of its
/// own, or the ref now holds them.
pub fn save_and_reset(ctx: &mut LoopState) -> Option<String> {
    let base = base_ref(ctx);
    let (branch, keep) = {
        let git = &mut ctx.ext.get::<AGGState>().git;
        (git.session_branch.take(), git.untracked_before.take())
    };
    let ahead = branch.as_deref().is_some_and(|b| !crate::git::branch_has_no_new_commits(&ctx.dir, &base, b));
    let dirty = crate::git::has_uncommitted_work(&ctx.dir, keep.as_deref());
    let saved = crate::git::save_partial(&ctx.dir, &ctx.clock.run_id, ctx.session, ahead, keep.as_deref());
    if dirty && saved.is_none() {
        // never reset what could not be saved: the work stays in the tree, on its branch.
        eprintln!("  ⚠ [iso] could not save session #{}'s unfinished work to a ref — left in the tree", ctx.session);
        let git = &mut ctx.ext.get::<AGGState>().git;
        (git.session_branch, git.untracked_before) = (branch, keep);
        return None;
    }
    crate::git::reset_work_tree(&ctx.dir, keep.as_deref());
    if let Some(r) = &saved {
        eprintln!(
            "  [iso] session #{} did not finish — its work is saved under {r} (NOT judged); tree reset",
            ctx.session
        );
    }
    if let Some(b) = branch {
        if crate::git::checkout(&ctx.dir, &base) && (saved.is_some() || !ahead) {
            crate::git::delete_branch(&ctx.dir, &b);
        }
    }
    saved
}
