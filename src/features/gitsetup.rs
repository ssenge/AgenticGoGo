//! The git-setup feature group: agg's run-start git preconditions as a `pre_start` plugin.

use std::path::Path;

use anyhow::Result;

use crate::loop_::{Bootstrap, PreStart};

/// The `pre_start` feature: agg's run-start git preconditions, in order — recover a stranded merge
/// from a prior crash, require a clean git repo (session isolation is MANDATORY), ensure BOTH
/// runtime roots (`agg/state` + `agg/private`) are gitignored (runtime state survives rollback), and
/// resolve the isolation base branch (→ `boot.iso_base` for the constructor). Runs before the loop
/// state exists; any `bail!` is a hard error out of `run()`.
///
/// # The three normalization rules (BUILD.md §3.9) — moat-grade
///
/// Under per-session isolation HEAD stays on a session branch for a whole span, so what a CRASHED
/// run leaves behind is: HEAD on `<prefix>/<project>/session-N`, the tree possibly dirty, and the
/// span's branches all still there. Each rule below prevents one silent, real failure of the NEXT
/// run started in that state.
///
/// 1. **Never resolve the base FROM a session branch.** `base_branch` defaults to empty and the base
///    is then taken from the current branch — so the next run would base on the DEAD session branch,
///    cut every session off it, merge every gate into it, return `Kept`, write `Merged` to
///    `verdicts.jsonl`, and never touch `main`, with nothing warning because the span *was* gated.
/// 2. **On the resume path, DISCARD a dirty tree loudly** instead of refusing. A power cut
///    mid-worker leaves uncommitted tracked changes by construction (agg commits a session's work
///    only after it ends), so refusing makes the one scenario resume exists for the one scenario
///    that cannot start.
/// 3. **Do not eat the previous run's span.** Session numbering restarts per run and
///    `create_branch` opens with `git branch -D`, so a fresh run's session-1..N would delete the
///    previous run's same-named branches — including the tip the run-end warning just told the
///    operator to `git merge`. They are PARKED aside instead, before the first `create_branch`.
pub struct GitSetup;
impl PreStart for GitSetup {
    fn run(&self, boot: &mut Bootstrap) -> Result<()> {
        let dir = boot.dir;
        let iso = &boot.cfg.session_isolation;
        // recover a stranded merge left by a prior crash (guarded on being a git repo)
        if crate::git::is_repo(dir) {
            crate::git::recover_stranded_merge(dir, &iso.branch_prefix);
        }
        // require a git repo with a clean tracked tree
        if !crate::git::is_repo(dir) {
            anyhow::bail!(
                "session isolation is mandatory, but this is not a git repository.\n  \
                 fix:  git init && git add -A && git commit -m 'agg baseline'"
            );
        }
        // ── rule 2: a dirty tree ends the run — UNLESS this is a resume, which discards it ──
        // The discard runs FIRST, while HEAD is still where the crash left it: those edits belong to
        // the dead session, and carrying them across the checkout below would launder a half-finished
        // session's work into the new run's baseline.
        if !crate::git::is_clean(dir) {
            if !boot.resume {
                anyhow::bail!(
                    "session isolation is mandatory, but the work tree has uncommitted tracked changes.\n  \
                     fix:  commit or stash your changes first  (git status shows them)"
                );
            }
            eprintln!(
                "  ⚠ [iso] RESUME with a dirty work tree — DISCARDING the uncommitted tracked changes.\n\
                 \x20       They are a crashed session's half-finished edits (agg commits a session's work\n\
                 \x20       only AFTER it ends), and that session re-executes from the top anyway."
            );
            crate::git::discard_uncommitted_tracked(dir);
        }

        // ── rule 1: resolve the isolation base, and never from a session branch ──
        let stem = crate::git::session_branch_stem(&iso.branch_prefix, &boot.cfg.project);
        let head = crate::git::current_branch(dir);
        let stranded = head.as_deref().is_some_and(|h| h.starts_with(&stem));
        let iso_base: String = if !iso.base_branch.is_empty() {
            iso.base_branch.clone()
        } else if stranded {
            // the current branch is a dead span's tip. The real base was recorded by the run that cut
            // it; a run that cannot recover it REFUSES, because every alternative (guess `main`, use
            // the tip anyway) silently picks a branch the operator never chose.
            match recorded_base(dir).filter(|b| !b.starts_with(&stem)) {
                Some(b) => b,
                None => anyhow::bail!(
                    "HEAD is on the agg session branch '{}' — a previous run died mid-span — and no \
                     base branch is recorded in {}.\n  \
                     agg will NOT base a run on a session branch: every gate would merge into it and \
                     your real base would never move.\n  \
                     fix:  git checkout <your base branch>   (or set session_isolation.base_branch)",
                    head.unwrap_or_default(),
                    crate::paths::state_json(dir).display()
                ),
            }
        } else {
            match head.clone() {
                Some(b) => b,
                None => anyhow::bail!(
                    "session isolation is mandatory, but HEAD is detached.\n  \
                     fix:  git switch -c <branch>"
                ),
            }
        };
        if stranded {
            eprintln!(
                "  ⚠ [iso] HEAD is on the session branch '{}' — a previous run died mid-span.\n\
                 \x20       Basing this run on '{iso_base}' and checking it out: a run based on a dead\n\
                 \x20       session branch merges every gate into it and never moves '{iso_base}'.",
                head.as_deref().unwrap_or_default()
            );
            if !crate::git::checkout(dir, &iso_base) {
                anyhow::bail!(
                    "could not check out the base branch '{iso_base}' to recover from a mid-span crash.\n  \
                     fix:  git checkout {iso_base}   (resolve whatever git reported above first)"
                );
            }
        }

        // keep runtime state untracked (survives rollback).
        //
        // ⚠ AFTER the rule-1 checkout, never before. `.gitignore` is not committed on the base branch
        // of a fresh project — a session branch is the first thing that ever commits it — so checking
        // base out DELETES it from the work tree, and the next worker's own `git add -A` would then
        // commit `agg/private/` (agg's own ledger, state.json, LOG.md) onto a session branch and
        // wedge every subsequent checkout.
        crate::git::ensure_agg_gitignored(dir);

        // ── rule 3: park a previous run's surviving span before `create_branch` can delete it ──
        park_stranded_span(dir, &stem, &iso.branch_prefix, &boot.cfg.project);

        eprintln!("  [iso] per-session branch isolation ON — base branch '{iso_base}'");
        boot.iso_base = Some(iso_base);
        Ok(())
    }
}

/// The base branch the LAST run recorded in `state.json`, if any. An empty value counts as absent —
/// that is what a state.json written before this field existed deserializes to.
fn recorded_base(dir: &Path) -> Option<String> {
    crate::state::DashboardState::read(dir).map(|d| d.iso_base).filter(|b| !b.is_empty())
}

/// Rename every surviving `<prefix>/<project>/session-*` aside, so this run's session numbering
/// cannot delete a previous run's stranded span.
///
/// ⛔ **A rename, never a delete.** These branches are the only thing holding an ungated run's
/// output, and the run-end warning explicitly told the operator to `git merge` one of them — so agg
/// moves them out of the way and says where they went. Deleting them (which is exactly what
/// `create_branch`'s opening `git branch -D` does, silently, session by session) destroys an
/// overnight run's work.
fn park_stranded_span(dir: &Path, stem: &str, prefix: &str, project: &str) {
    let mut survivors = crate::git::branches_starting_with(dir, stem);
    if survivors.is_empty() {
        return;
    }
    // by SESSION NUMBER, not git's lexical order — `session-10` sorts before `session-2` there, and
    // the last one is the span tip, the branch the operator actually wants to merge.
    survivors.sort_by_key(|b| b[stem.len()..].parse::<u32>().unwrap_or(0));
    let ts = crate::util::now_epoch();
    eprintln!(
        "  ⚠ [iso] {} session branch(es) from a previous run are still here, and this run's own\n\
         \x20       session-1.. would DELETE them. Parking them aside instead — nothing is lost:",
        survivors.len()
    );
    let mut parked = Vec::new();
    for br in &survivors {
        let to = crate::git::parked_branch(prefix, project, ts, &br[stem.len()..]);
        if crate::git::rename_branch(dir, br, &to) {
            eprintln!("  \x20         {br} → {to}");
            parked.push(to);
        }
    }
    if let Some(tip) = parked.last() {
        eprintln!("  \x20       merge that run's whole span with:  git merge {tip}");
    }
}
