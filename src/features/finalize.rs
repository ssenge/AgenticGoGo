//! The finalize feature group — the on-session-end tail: the post-judge memory refinement fold (`RefineFold`) and the run-stop decision (`CheckRunStop`).

use anyhow::Result;

use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LifecycleEvent, LoopState, RunOutcome};

/// Institutional memory: the post-judge refinement fold. Gated on the floor fold (`scratch.mem_folded`)
/// and reads `scratch.rolled_back` + the summary — exactly the old post-judge fold.
pub struct RefineFold;
impl Handler for RefineFold {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !(ctx.cfg.memory.enabled && ctx.scratch.get::<AGGScratch>().mem_folded) {
            return Ok(Flow::Continue);
        }
        let outcome = ctx.scratch.get::<AGGScratch>().outcome.clone().expect("LaunchWorker set scratch.outcome");
        let deltas = ctx.scratch.get::<AGGScratch>().res.as_ref().map(|r| r.deltas.clone()).unwrap_or_default();
        let rolled_back = ctx.scratch.get::<AGGScratch>().rolled_back;
        let summarized_this_cycle = ctx.scratch.get::<AGGScratch>().summarized_this_cycle;
        let scoreboard = ctx.eng.scoreboard();
        let ended = crate::util::now_epoch();
        let mut mech = crate::core::memory::mechanical_note(
            outcome.exit_code, outcome.killed_by_watchdog, outcome.rate_limited,
            outcome.duration_secs, ended.saturating_sub(outcome.duration_secs), ended,
            &scoreboard, &deltas,
        );
        if rolled_back {
            mech = format!(
                "session ROLLED BACK — a goal regressed on the staged merge; the work below is \
                 NOT on the base branch (kept on the session branch for inspection).\n{mech}"
            );
        }
        let worker_note = crate::core::memory::read_worker_note(ctx.dir, ctx.session);
        let (source, body) = match worker_note {
            Some(note) => (
                "mechanical+worker",
                format!("{mech}\n\n[worker note — UNTRUSTED hint, not authoritative]\n```text\n{note}\n```"),
            ),
            None if summarized_this_cycle && !ctx.dash.summary_windowed.trim().is_empty() => (
                "mechanical+summary",
                format!("{mech}\n\nsummary: {}", ctx.dash.summary_windowed.trim()),
            ),
            None => ("mechanical", mech),
        };
        ctx.dash.memory_bytes = crate::core::memory::fold_entry(
            ctx.dir, ctx.session, source, &body, ctx.cfg.memory.max_kb, true,
        );
        crate::core::memory::clear_scratch(ctx.dir, ctx.session);
        ctx.ext.get::<AGGState>().memory.last_session =
            crate::core::memory::last_session_block(&deltas, &scoreboard);
        eprintln!("  [memory] session #{} folded ({source}); LOG.md {} B", ctx.session, ctx.dash.memory_bytes);
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "RefineFold"
    }
}

/// The run-stop decision — the LAST on_session_end handler (R2): the winning/aborting session has
/// ALREADY run the session-end shell hook + summary + refine fold above. Emits `Finished` itself,
/// then `Flow::Stop`. `Continue` means the loop goes round again (the old `GateDecision::Loop`).
pub struct CheckRunStop;
impl Handler for CheckRunStop {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let res = ctx.scratch.get::<AGGScratch>().res.take().expect("an on_verify handler set scratch.res");
        if res.halt {
            let reason = res.halt_reason.unwrap_or_default();
            eprintln!("\n⚠ ABORT — abort_if true: {reason}\n  stopping the loop (a ceiling / guard, not success).");
            ctx.report_stranded_span();
            ctx.emit(LifecycleEvent::Finished {
                reason: format!("ABORT: {reason}"),
                ledger_tag: format!("abort:{reason}"),
            });
            return Ok(Flow::Stop(RunOutcome::Halt));
        }
        if res.stop {
            let (mt, tt) = ctx.eng.tally();
            eprintln!("\n✔ done_if satisfied — {mt}/{tt} goals met. Done after {} session(s).", ctx.session);
            ctx.emit(LifecycleEvent::Finished {
                reason: format!("{mt}/{tt} goals met after {} session(s)", ctx.session),
                ledger_tag: "goals-met".into(),
            });
            return Ok(Flow::Stop(RunOutcome::GoalsMet));
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "CheckRunStop"
    }
}
