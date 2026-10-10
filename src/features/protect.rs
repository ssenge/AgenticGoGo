//! The protected-path GATE CHECK — the half of `write_deny` that works on every tier.
//!
//! [`before_worker`] fingerprints every covered entry (tracked, untracked and gitignored) right
//! before the worker starts; [`after_worker`] does it again the moment the worker exits and diffs
//! the two. A difference is a `protected_path_violation`:
//!
//! 1. the paths are put back from agg's own copies — BEFORE judging, so no judge ever grades
//!    against a weakened oracle;
//! 2. a row lands in `agg/private/violations.jsonl` with the paths;
//! 3. the open span is marked, and the next gate ([`crate::features::gate::keep_or_rollback`])
//!    rolls it back whatever the judges said and whatever `gate_regressions` is set to.
//!
//! Both calls sit inside `LaunchWorker`, around `run_session` and nothing else, so the window they
//! compare is exactly the worker's. A change made AFTER the check — by a process the worker left
//! running, say — is caught by the next [`before_worker`], which compares against the state the
//! last check left behind ("between sessions") and restores it too.
//!
//! The decision state is in agg's MEMORY, never re-read from disk: under `isolation: none` the
//! worker can write `agg/private/`, so a ledger row is a record, not the authority.

use anyhow::{bail, Context, Result};

use crate::core::config::ResolvedStep;
use crate::loop_::{AGGScratch, AGGState, LoopState};
use crate::protect::snapshot::{self, Snapshot};
use crate::protect::{PatternSet, ViolationRecord, VIOLATION};

/// Per-run protected-path state, in [`AGGState`].
#[derive(Default)]
pub struct ProtectState {
    /// paths a session in the OPEN span changed. Non-empty ⇒ the next gate rolls the span back.
    pub span_violation: Vec<String>,
    /// how the protected entries looked when the last check finished (restored if need be) — the
    /// reference for catching a change made between two sessions. `None` after a session that
    /// protected nothing, so a step with a different list never compares against it.
    pub last: Option<LastCheck>,
}

/// The state the last [`after_worker`] left behind.
pub struct LastCheck {
    pub patterns: Vec<String>,
    pub snap: Snapshot,
    pub session: u32,
    pub step: String,
}

/// Snapshot the step's protected entries before its worker runs. No-op for a step with none.
pub fn before_worker(ctx: &mut LoopState, step: &ResolvedStep) -> Result<()> {
    if step.write_deny.is_empty() {
        ctx.ext.get::<AGGState>().protect.last = None;
        return Ok(());
    }
    // a bind-based tier needs `dir/**` roots to exist; create them now, so the snapshot sees them
    // and agg's own mkdir is not read as the worker's.
    crate::protect::materialize(&ctx.dir, step.isolation, &step.write_deny);
    let set = PatternSet::compile(&ctx.dir, &step.write_deny)?;
    let store = crate::paths::protected_store(&ctx.dir);
    let snap = snapshot::take(&ctx.dir, &set, Some(&store)).context("snapshotting the protected paths")?;

    let last = ctx.ext.get::<AGGState>().protect.last.take();
    let snap = match last {
        Some(l) if l.patterns == step.write_deny => {
            let changed = snapshot::diff(&l.snap, &snap);
            if changed.is_empty() {
                snap
            } else {
                eprintln!(
                    "  ⛔ {VIOLATION}: {} protected path(s) changed BETWEEN sessions (after session \
                     #{}'s check): {} — restoring them",
                    changed.len(),
                    l.session,
                    changed.join(", ")
                );
                undo(ctx, &set, &l.snap, &changed, (l.session, &l.step, "between_sessions"))?;
                l.snap
            }
        }
        _ => snap,
    };
    ctx.scratch.get::<AGGScratch>().protect_before = Some(snap);
    Ok(())
}

/// Compare against [`before_worker`]'s snapshot; on any difference restore, record and mark the
/// span for rollback.
pub fn after_worker(ctx: &mut LoopState, step: &ResolvedStep) -> Result<()> {
    let Some(before) = ctx.scratch.get::<AGGScratch>().protect_before.take() else {
        return Ok(());
    };
    let set = PatternSet::compile(&ctx.dir, &step.write_deny)?;
    let after = snapshot::take(&ctx.dir, &set, None).context("re-reading the protected paths")?;
    let changed = snapshot::diff(&before, &after);
    if !changed.is_empty() {
        eprintln!(
            "  ⛔ {VIOLATION}: session #{} changed {} protected path(s): {} — restoring them; the \
             session will be ROLLED BACK",
            ctx.session,
            changed.len(),
            changed.join(", ")
        );
        let session = ctx.session;
        undo(ctx, &set, &before, &changed, (session, &step.name, "session"))?;
        ctx.ext.get::<AGGState>().protect.span_violation.extend(changed);
    }
    ctx.ext.get::<AGGState>().protect.last = Some(LastCheck {
        patterns: step.write_deny.clone(),
        snap: before,
        session: ctx.session,
        step: step.name.clone(),
    });
    Ok(())
}

/// Restore `changed` to `want`, record the violation, and PROVE the restore by re-reading. Any
/// failure is a hard error: a weakened oracle left on disk is what this feature exists to prevent,
/// so the run stops rather than judge against it.
fn undo(
    ctx: &LoopState,
    set: &PatternSet,
    want: &Snapshot,
    changed: &[String],
    (session, step, window): (u32, &str, &str),
) -> Result<()> {
    let store = crate::paths::protected_store(&ctx.dir);
    let restored = snapshot::restore(&ctx.dir, want, changed, &store).and_then(|()| {
        let now = snapshot::take(&ctx.dir, set, None)?;
        match snapshot::diff(want, &now) {
            d if d.is_empty() => Ok(()),
            d => bail!("still different after the restore: {}", d.join(", ")),
        }
    });
    let rec = ViolationRecord {
        session,
        step: step.to_string(),
        reason: VIOLATION.to_string(),
        window: window.to_string(),
        paths: changed.to_vec(),
        restored: restored.is_ok(),
        ts: crate::util::now_epoch(),
    };
    crate::protect::append(&ctx.dir, &rec).context("recording the protected_path_violation")?;
    restored.context("could not restore the protected paths — stopping rather than judging against them")
}
