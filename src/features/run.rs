//! The RUN feature — launch the fresh worker for this step's (agent, model, effort). ONE plugin on
//! `on_run`: the unknown-agent and SIGINT early returns forbid splitting. Reads `AGGScratch::prompt`,
//! writes `AGGScratch::outcome`. `Flow::Stop(finish_interrupted())` on SIGINT is the RUN stage's only
//! control-flow exit.

use anyhow::Result;

use crate::backend::worker::{self, SessionOutcome};
use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LoopState};

/// A session that failed without a word: not rate-limited, a non-zero exit, no output.
fn is_dud(outcome: &SessionOutcome) -> bool {
    !outcome.rate_limited && outcome.exit_code != Some(0) && outcome.usage.tokens.output_or_zero() == 0
}

pub struct LaunchWorker;
impl Handler for LaunchWorker {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let prompt = ctx.scratch.get::<AGGScratch>().prompt.take().expect("WriteInstructions set scratch.prompt");
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        // `static_backend`, not `backend`: the stream-reader thread below needs the backend to be
        // `&'static`, which a driver's own `Arc`-held one is not (see `ResolvedStep::static_backend`).
        let agent = match step.static_backend() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  step `{}` names an unknown agent: {e}", step.name);
                ctx.ext.get::<AGGState>().worker.dud_streak += 1;
                ctx.scratch.get::<AGGScratch>().outcome = Some(SessionOutcome::failed());
                return Ok(Flow::Continue);
            }
        };
        let model = step.model(agent).to_string();
        let effort = step.effort(agent).to_string();
        let isolation = step.isolation;
        // The step's own deny list, on its way to the OS wrapper. Under `Isolation::None` there is
        // no wrapper to deliver it to — say so, in the operator's terms ("the step CAN WRITE these
        // paths"), because the failure mode is a step that believes it is protected.
        //
        // ponytail: this fires once per SESSION, so a long run repeats it. The ceiling is noise, not
        // correctness; the upgrade path when it bites is a once-per-step-name set on `AGGState`.
        if let Some(w) =
            crate::isolation::unenforced_denies(&step.name, isolation, &step.readonly, &step.writable)
        {
            eprintln!("{w}");
        }
        let denied = step.denied();
        // ⛔ The protected-path bracket: snapshot IMMEDIATELY before the worker, check IMMEDIATELY
        // after it — before the interrupt return below, so even a Ctrl-C'd session cannot leave a
        // changed oracle on disk. Done here rather than as separate handlers because a `Flow::Stop`
        // out of this handler would skip a handler registered after it.
        crate::features::protect::before_worker(ctx, &step)?;
        crate::features::partial::note_untracked_before(ctx);
        let launch = |resume| {
            worker::run_session(
                &ctx.cfg,
                agent,
                &model,
                &effort,
                &step.worker_args,
                &prompt,
                &ctx.dir,
                ctx.session,
                &ctx.live,
                isolation,
                &denied,
                &step.write_deny,
                &step.image,
                resume,
                step.user_settings,
            )
        };
        let mut outcome = launch(step.resume.as_ref());
        // A session the agent no longer has fails before any work: the context was only a cache, so
        // the step starts fresh rather than failing (A-33).
        if let Some((id, _)) = step.resume.as_ref().filter(|_| is_dud(&outcome) && !crate::os::signals::interrupted()) {
            eprintln!("  resuming session {id} failed (exit {:?}); starting fresh", outcome.exit_code);
            outcome = launch(None);
        }
        crate::features::protect::after_worker(ctx, &step)?;
        ctx.charge_usage(crate::usage::Role::Worker, &step.agent, &outcome.usage);

        if crate::os::signals::interrupted() {
            // an interrupted session is incomplete: never judged, never lost (A-18.3).
            crate::features::partial::save_and_reset(ctx);
            return Ok(Flow::Stop(ctx.finish_interrupted()));
        }
        eprintln!(
            "  session #{} exited (code {:?}) after {}s{}{}  (+{} out-tok, {} total; +${:.4}, ${:.4} total)",
            ctx.session,
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            if outcome.killed_by_watchdog { "  [WATCHDOG-KILLED: hung worker]" } else { "" },
            outcome.usage.tokens.output_or_zero(),
            ctx.tokens_spent,
            outcome.usage.cost_usd.unwrap_or(0.0),
            ctx.cost_spent,
        );
        // warn (loudly) if the agent never touched its forward state file (§5.6 / OQ3).
        if let (Some(step), false) = (ctx.cur_step.as_ref().filter(|s| s.memory), outcome.rate_limited) {
            let now = std::fs::read_to_string(ctx.config_base.join(&step.state)).ok();
            if now == ctx.ext.get::<AGGState>().inject.state_before {
                eprintln!("  ⚠ the worker did not update `{}` this session — the next session inherits stale forward-state.", step.state);
            }
        }

        let dud = is_dud(&outcome);
        let w = &mut ctx.ext.get::<AGGState>().worker;
        w.dud_streak = if dud { w.dud_streak + 1 } else { 0 };
        ctx.scratch.get::<AGGScratch>().outcome = Some(outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "LaunchWorker"
    }
}
