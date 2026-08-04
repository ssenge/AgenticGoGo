//! The RUN feature — launch the fresh worker for this step's (agent, model, effort). ONE plugin on
//! `on_run`: the unknown-agent and SIGINT early returns forbid splitting. Reads `AGGScratch::prompt`,
//! writes `AGGScratch::outcome`. `Flow::Stop(finish_interrupted())` on SIGINT is the RUN stage's only
//! control-flow exit.

use anyhow::Result;

use crate::backend::worker::{self, SessionOutcome};
use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LoopState};

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
        let outcome = worker::run_session(
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
            &step.image,
        );
        ctx.tokens_spent += outcome.output_tokens;
        ctx.cost_spent += outcome.cost_usd;
        ctx.charge(&step.agent, outcome.output_tokens, Some(outcome.cost_usd));

        if crate::os::signals::interrupted() {
            return Ok(Flow::Stop(ctx.finish_interrupted()));
        }
        eprintln!(
            "  session #{} exited (code {:?}) after {}s{}{}  (+{} out-tok, {} total; +${:.4}, ${:.4} total)",
            ctx.session,
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            if outcome.killed_by_watchdog { "  [WATCHDOG-KILLED: hung worker]" } else { "" },
            outcome.output_tokens,
            ctx.tokens_spent,
            outcome.cost_usd,
            ctx.cost_spent,
        );
        // warn (loudly) if the agent never touched its forward state file (§5.6 / OQ3).
        if let (Some(step), false) = (&ctx.cur_step, outcome.rate_limited) {
            let now = std::fs::read_to_string(ctx.config_base.join(&step.state)).ok();
            if now == ctx.ext.get::<AGGState>().inject.state_before {
                eprintln!("  ⚠ the worker did not update `{}` this session — the next session inherits stale forward-state.", step.state);
            }
        }

        let dud = !outcome.rate_limited && outcome.exit_code != Some(0) && outcome.output_tokens == 0;
        let w = &mut ctx.ext.get::<AGGState>().worker;
        w.dud_streak = if dud { w.dud_streak + 1 } else { 0 };
        ctx.scratch.get::<AGGScratch>().outcome = Some(outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "LaunchWorker"
    }
}
