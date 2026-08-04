//! Gate feature: the on_gate keep/rollback plugins (ceiling-poison guard + judged merge keep/rollback).

use anyhow::Result;

use crate::loop_::{AGGState, AGGScratch, Flow, Handler, LoopState, LifecycleEvent, RunOutcome, indent};
use crate::core::engine::{CycleResult, GoalRuntime};
use crate::core::model::Verdict;
use crate::core::verdicts::Outcome;
use crate::git::StagedSession;

/// FIRST on on_gate: a skip-step ceiling halt (nothing staged, no verdicts) stops the run WITHOUT the
/// session-end work — and crucially WITHOUT `emit(Gate)`, so the poison path never publishes a Gate
/// phase (R10). Emits nothing; reads scratch by ref (leaves `res`/`staged` for GateKeepRollback).
pub struct CeilingPoisonGuard;
impl Handler for CeilingPoisonGuard {
    // ponytail: this `Flow::Stop` short-circuits the rest of on_gate, so `NotifyOnStuck` never runs
    // and a run ending here does NOT fire `notify.cmd` (STUCK_NOTIFY §12.6). Accepted: the guard
    // needs `res.deltas.is_empty()`, which only holds with an EMPTY run-set, and a config with no
    // judges at all cannot express a notify condition worth delivering. Upgrade path if a real run
    // ever lands here: `notify::halt_ping(ctx, res.halt_reason.as_deref())` before the Stop.
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let sc = ctx.scratch.get::<AGGScratch>();
        let res = sc.res.as_ref().expect("an on_verify handler set scratch.res");
        if sc.skip_judges
            && res.halt
            && sc.staged.is_none()
            && res.fresh_verdicts.is_empty()
            && res.deltas.is_empty()
        {
            return Ok(Flow::Stop(RunOutcome::Halt));
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "CeilingPoisonGuard"
    }
}

/// Keep / roll back the judged merge. `emit(Gate)` is its FIRST line (fires for skip + judged; the
/// poison path never reaches here, so it stays Gate-free — R10). Runs on skip steps too (an internal
/// `if skip_judges` guard makes the keep/rollback a no-op there — a skip step emits Gate but merges
/// nothing). On a rollback it REWRITES `scratch.res` and sets `scratch.rolled_back`. The
/// `verdicts::append` `?` is a HARD disk Err that bubbles out of `run()` (R7) — NOT a clean Halt.
///
/// The decision itself lives in [`keep_or_rollback`], because the Rust driver's `gate()` makes the
/// same one and this handler is a NO-OP on that path (every driver step is `skip_judges`, so the
/// guard above returns first).
pub struct GateKeepRollback;
impl Handler for GateKeepRollback {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        ctx.emit(LifecycleEvent::Gate);
        if ctx.scratch.get::<AGGScratch>().skip_judges {
            return Ok(Flow::Continue); // a skip step: the span was staged in VERIFY; nothing to gate.
        }
        let mut res = ctx.scratch.get::<AGGScratch>().res.take().expect("an on_verify handler set scratch.res");
        let staged = ctx.scratch.get::<AGGScratch>().staged.take();
        let pre_cycle_goals = std::mem::take(&mut ctx.scratch.get::<AGGScratch>().pre_cycle_goals);

        // the regression gate: a DoD-set judge MET before (durable, §5.7) that now fails. Scope to
        // the DoD-set exactly as `any_regressed`/`count_regressed` do (stop.rs `in_scope` →
        // `g.in_dod`). A run-set-only control judge like `stalled` is DESIGNED to flip met→unmet —
        // that flip is the very signal that fired `reconsider` — so counting its flip as a
        // regression would roll back the work that escaped the stall (and, because rolled-back rows
        // never land, livelock the loop). §5.7 protects the DoD-set; a judge named only in an `if`
        // condition is not in it.
        //
        // ⚠ This is exactly the half the DRIVER path spells differently (BUILD.md §3.5 item 5:
        // every judge asked since the last gate, no DoD scope), which is why it lives HERE and not
        // in the shared body below.
        let regressed = matches!(staged, Some((_, StagedSession::Staged))) && {
            let landed = crate::core::verdicts::landed_met(&ctx.dir);
            res.fresh_verdicts.iter().any(|(id, v)| {
                ctx.eng.judges.iter().any(|g| g.in_dod && &g.name == id)
                    && v.error.is_none()
                    && !v.met
                    && landed.get(id).copied().unwrap_or(false)
            })
        };

        let gated = keep_or_rollback(ctx, staged.as_ref(), &res.fresh_verdicts, regressed, &pre_cycle_goals)?;
        if gated != Gated::Merged {
            // nothing landed: the judged verdicts describe base, not a merge, so re-derive the
            // conditions against RESTORED base truth. `NotifyOnStuck` runs after this handler, so it
            // flags what was kept, never what was rolled back.
            let rs = ctx.run_state();
            let recomputed = ctx.eng.conditions_only(&rs);
            res = CycleResult {
                stop: recomputed.stop,
                halt: recomputed.halt,
                halt_reason: recomputed.halt_reason,
                notify: recomputed.notify,
                deltas: Vec::new(),
                fresh_verdicts: Vec::new(),
                judge_tokens: 0,
                judge_cost: None,
            };
        }
        ctx.scratch.get::<AGGScratch>().res = Some(res);
        ctx.scratch.get::<AGGScratch>().rolled_back = gated == Gated::RolledBack;
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "GateKeepRollback"
    }
}

/// What [`keep_or_rollback`] did with the span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gated {
    /// the staged merge was committed onto base.
    Merged,
    /// the staged merge was ABORTED by the regression policy — a decision, and base is unchanged.
    RolledBack,
    /// there was no staged merge to decide about (vetoed, no commits, a conflict, no branch).
    NotStaged,
}

/// The keep / roll-back BODY: the git finalization, the `verdicts.jsonl` row and the span teardown.
///
/// ⚠ **Two call sites, not one lifted out.** [`GateKeepRollback`] runs it on the YAML path; on the
/// driver path every step is `skip_judges`, so that handler returns at its guard and never gets
/// here, and [`crate::driver::Agg::gate`] calls this directly instead. Keeping the body inline in
/// the handler would have left the driver free to grow a second, drifting copy of the one decision
/// in the loop that can lose the worker's commits.
///
/// What the two callers do NOT share stays outside: what counts as a regression (DoD-scoped on the
/// YAML path, every judge asked since the last gate on the driver path) is computed by the caller
/// and passed in as `regressed`, and the YAML path's `scratch.res` rewrite happens on the way out.
/// The `keep` DECISION is shared, because `ctx.gate_regressions` carries both paths' policy —
/// `sequence.gate_regressions` from YAML, `OnRegression::Rollback` from the builder.
pub(crate) fn keep_or_rollback(
    ctx: &mut LoopState,
    staged: Option<&(String, StagedSession)>,
    fresh: &[(String, Verdict)],
    regressed: bool,
    pre_goals: &[GoalRuntime],
) -> Result<Gated> {
    let step_name = ctx.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
    // The `verdicts::append` `?` below is a HARD disk Err that bubbles out to the caller (R7) — the
    // gate's "was met" reads this file, so a swallowed write would silently un-arm the next gate.
    let out = match staged {
        Some((br, StagedSession::Staged)) => {
            let keep = if ctx.gate_regressions { !regressed } else { true };
            crate::git::finalize_session(&ctx.dir, br, ctx.session, keep);
            let tag = if keep { Outcome::Merged } else { Outcome::RolledBack };
            crate::core::verdicts::append(&ctx.dir, Some(ctx.session), &step_name, fresh, tag)?;
            // the whole span merged with this branch (it descends from every other one), or was
            // discarded with it. Either way the span is closed.
            clear_span(ctx, Some(br));
            if keep {
                Gated::Merged
            } else {
                ctx.eng.restore_goal_state(pre_goals);
                eprint!("{}", indent(&ctx.eng.scoreboard()));
                Gated::RolledBack
            }
        }
        _ => {
            // Vetoed / NoChanges / Conflict / CheckoutFailed / no branch: nothing merged. The
            // judged verdicts describe base, not a landed merge — record them rolled_back and
            // restore base truth so the next step isn't gated against a phantom.
            ctx.eng.restore_goal_state(pre_goals);
            clear_span(ctx, staged.map(|(br, _)| br.as_str()));
            crate::core::verdicts::append(&ctx.dir, Some(ctx.session), &step_name, fresh, Outcome::RolledBack)?;
            Gated::NotStaged
        }
    };
    ctx.publish();
    Ok(out)
}

/// Close the open span: forget the tip and DELETE the branches it accumulated.
///
/// `spoken_for` is the one ref git has already decided about — `finalize_session` deletes it on a
/// keep and deliberately KEEPS it for inspection on a rollback, and `stage_session` keeps it on a
/// conflict — so this must not second-guess it. Every other entry is an intermediate span branch
/// whose commits are reachable either from base (kept) or from that surviving tip (rolled back), so
/// deleting it loses nothing.
///
/// ⛔ Leaving them would not be harmless once a span is longer than one session: `create_branch`
/// opens with `git branch -D`, so the NEXT run's session-1..N would silently eat same-named refs,
/// and the stale-span detection has no way to tell a merged leftover from a stranded span. The
/// shipped loop got away with `span_branches.clear()` because it only ever held ONE branch.
fn clear_span(ctx: &mut LoopState, spoken_for: Option<&str>) {
    let branches = std::mem::take(&mut ctx.ext.get::<AGGState>().git.span_branches);
    for br in &branches {
        if Some(br.as_str()) != spoken_for {
            crate::git::delete_branch(&ctx.dir, br);
        }
    }
    ctx.ext.get::<AGGState>().git.span_tip = None;
}
