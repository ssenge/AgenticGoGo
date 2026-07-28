//! Gate feature: the on_gate keep/rollback plugins (ceiling-poison guard + judged merge keep/rollback).

use anyhow::Result;

use crate::loop_::{AGGState, AGGScratch, Flow, Handler, LoopState, LifecycleEvent, RunOutcome, indent};
use crate::core::engine::CycleResult;

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
        let step_name = ctx.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let mut rolled_back = false;

        match &staged {
            Some((br, crate::git::StagedSession::Staged)) => {
                // the regression gate: a DoD-set judge MET before (durable, §5.7) that now fails.
                // Scope to the DoD-set exactly as `any_regressed`/`count_regressed` do (stop.rs
                // `in_scope` → `g.in_dod`). A run-set-only control judge like `stalled` is DESIGNED to
                // flip met→unmet — that flip is the very signal that fired `reconsider` — so counting
                // its flip as a regression would roll back the work that escaped the stall (and, because
                // rolled-back rows never land, livelock the loop). §5.7 protects the DoD-set; a judge
                // named only in an `if` condition is not in it.
                let landed = crate::core::verdicts::landed_met(ctx.dir);
                let regressed = res.fresh_verdicts.iter().any(|(id, v)| {
                    ctx.eng.judges.iter().any(|g| g.in_dod && &g.name == id)
                        && v.error.is_none()
                        && !v.met
                        && landed.get(id).copied().unwrap_or(false)
                });
                let keep = if ctx.gate_regressions { !regressed } else { true };
                crate::git::finalize_session(ctx.dir, br, ctx.session, keep);
                let tag = if keep {
                    crate::core::verdicts::Outcome::Merged
                } else {
                    crate::core::verdicts::Outcome::RolledBack
                };
                crate::core::verdicts::append(ctx.dir, Some(ctx.session), &step_name, &res.fresh_verdicts, tag)?;
                if keep {
                    // the whole span merged with this branch (it descends from the span). Clear it.
                    // ponytail: intermediate span branches are left as refs (no public delete);
                    // harmless, and cleanup is a later polish. REPORTED.
                    ctx.ext.get::<AGGState>().git.span_tip = None;
                    ctx.ext.get::<AGGState>().git.span_branches.clear();
                } else {
                    rolled_back = true;
                    ctx.eng.restore_goal_state(&pre_cycle_goals);
                    ctx.ext.get::<AGGState>().git.span_tip = None; // span discarded; next cuts off base
                    ctx.ext.get::<AGGState>().git.span_branches.clear();
                    eprint!("{}", indent(&ctx.eng.scoreboard()));
                    let rs = ctx.run_state();
                    let recomputed = ctx.eng.conditions_only(&rs);
                    res = CycleResult {
                        stop: recomputed.stop,
                        halt: recomputed.halt,
                        halt_reason: recomputed.halt_reason,
                        // recomputed against RESTORED base truth — NotifyOnStuck runs after this
                        // handler, so it flags what was kept, never what was rolled back.
                        notify: recomputed.notify,
                        deltas: Vec::new(),
                        fresh_verdicts: Vec::new(),
                        judge_tokens: 0,
                        judge_cost: None,
                    };
                }
                ctx.publish();
            }
            _ => {
                // Vetoed / NoChanges / Conflict / CheckoutFailed / no branch: nothing merged. The
                // judged verdicts describe base, not a landed merge — record them rolled_back and
                // restore base truth so the next step isn't gated against a phantom.
                ctx.eng.restore_goal_state(&pre_cycle_goals);
                ctx.ext.get::<AGGState>().git.span_tip = None;
                ctx.ext.get::<AGGState>().git.span_branches.clear();
                crate::core::verdicts::append(
                    ctx.dir,
                    Some(ctx.session),
                    &step_name,
                    &res.fresh_verdicts,
                    crate::core::verdicts::Outcome::RolledBack,
                )?;
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
                ctx.publish();
            }
        }

        ctx.scratch.get::<AGGScratch>().res = Some(res);
        ctx.scratch.get::<AGGScratch>().rolled_back = rolled_back;
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "GateKeepRollback"
    }
}
