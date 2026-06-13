//! Per-session git isolation primitives.
//!
//! When `session_isolation.enabled`, the loop runs each worker session on its own branch
//! (`<prefix>/<project>/session-<N>`) cut from a base branch. After the session, the branch
//! is merged back into the base UNLESS the worker vetoed it (wrote the red file), in which
//! case the branch is discarded and the base is untouched.
//!
//! All operations shell out to `git` (no libgit2 dependency — agg stays lean). Every call is
//! best-effort-logged: a git failure is surfaced but never panics the loop. If isolation
//! can't proceed cleanly (dirty tree, detached HEAD, not a repo), the caller falls back to
//! running the session directly on the current branch — isolation is an enhancement, not a
//! correctness requirement.

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
/// state)? Untracked files are allowed (they carry across a checkout). agg's `.agg/` runtime
/// state (state.json/project.json/run.pid) churns every cycle and MUST NOT count as dirty —
/// it's runtime, not project content (and ideally gitignored). We exclude it via a pathspec.
pub fn is_clean(dir: &Path) -> bool {
    // pathspec `:(exclude).agg/**` drops agg's state churn; `--untracked-files=no` ignores untracked.
    git(
        dir,
        &["status", "--porcelain", "--untracked-files=no", "--", ".", ":(exclude).agg", ":(exclude).agg/**"],
    )
    .1
    .is_empty()
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

/// Ensure `.agg/` is gitignored (so agg's runtime state never gets committed onto session
/// branches or merged into base). Idempotent: appends the entry only if absent. Best-effort.
pub fn ensure_agg_gitignored(dir: &Path) {
    let gi = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    if existing.lines().any(|l| {
        let t = l.trim();
        t == ".agg" || t == ".agg/" || t == "/.agg" || t == "/.agg/"
    }) {
        return;
    }
    let mut new = existing;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(".agg/\n");
    let _ = std::fs::write(&gi, new);
    // also stop tracking it if it was already committed (keeps the file on disk).
    let _ = git(dir, &["rm", "-r", "--cached", "--quiet", ".agg"]);
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
pub fn resolve_session(
    dir: &Path,
    base: &str,
    branch: &str,
    red_file: &str,
    session: u32,
) -> SessionResolution {
    let vetoed = file_exists(dir, red_file);
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
}
