//! The RUN feature — launch the fresh worker for this step's (agent, model, effort). ONE plugin on
//! `on_run`: the unknown-agent and SIGINT early returns forbid splitting. Reads `RLTScratch::prompt`,
//! writes `RLTScratch::outcome`. `Flow::Stop(finish_interrupted())` on SIGINT is the RUN stage's only
//! control-flow exit.

use anyhow::Result;

use crate::backend::worker::{self, SessionOutcome};
use crate::loop_::{RLTScratch, RLTState, Flow, Handler, LoopState};

/// A session that failed without a word: not rate-limited, a non-zero exit, no output.
fn is_dud(outcome: &SessionOutcome) -> bool {
    !outcome.rate_limited && outcome.exit_code != Some(0) && outcome.usage.tokens.output_or_zero() == 0
}

/// A session that did not end well: a non-zero exit, a kill by the watchdog or the step's timeout.
fn failed(outcome: &SessionOutcome) -> bool {
    outcome.exit_code != Some(0) || outcome.killed_by_watchdog || outcome.timed_out
}

/// One try of the step's worker.
#[allow(clippy::too_many_arguments)]
fn launch(
    ctx: &LoopState,
    agent: &crate::backend::BackendRef,
    model: &str,
    effort: &str,
    step: &crate::core::config::ResolvedStep,
    prompt: &str,
    denied: &[String],
    resume: Option<&(String, bool)>,
) -> SessionOutcome {
    worker::run_session(
        &ctx.cfg,
        agent,
        model,
        effort,
        &step.worker_args,
        prompt,
        &ctx.dir,
        ctx.session,
        &ctx.live,
        step.isolation,
        denied,
        &step.write_deny,
        &step.image,
        resume,
        step.user_settings,
        step.system_prompt.as_deref(),
        step.timeout_secs,
    )
}

pub struct LaunchWorker;
impl Handler for LaunchWorker {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let prompt = ctx.scratch.get::<RLTScratch>().prompt.take().expect("WriteInstructions set scratch.prompt");
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        // `backend_ref`: an owned handle, so the stream-reader thread can hold a driver's own backend too
        let agent = match step.backend_ref() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  step `{}` names an unknown agent: {e}", step.name);
                ctx.ext.get::<RLTState>().worker.dud_streak += 1;
                ctx.scratch.get::<RLTScratch>().outcome = Some(SessionOutcome::failed());
                return Ok(Flow::Continue);
            }
        };
        let model = step.model(&*agent).to_string();
        let effort = step.effort(&*agent).to_string();
        let isolation = step.isolation;
        // The step's own deny list, on its way to the OS wrapper. Under `Isolation::None` there is
        // no wrapper to deliver it to — say so, in the operator's terms ("the step CAN WRITE these
        // paths"), because the failure mode is a step that believes it is protected.
        //
        // ponytail: this fires once per SESSION, so a long run repeats it. The ceiling is noise, not
        // correctness; the upgrade path when it bites is a once-per-step-name set on `RLTState`.
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
        if let Some(why) = crate::os::disk::run_shortfall(ctx.cfg.min_free_disk, &ctx.dir) {
            return Ok(Flow::Stop(ctx.disk_halt(&why)));
        }
        // another lane's rate limit closes the gate for this agent: no session starts before it reopens
        if let Some((lane, _)) = &ctx.lane {
            lane.wait_rate(&step.agent);
        }
        crate::features::protect::before_worker(ctx, &step)?;
        crate::features::partial::note_untracked_before(ctx);
        let mut outcome = launch(ctx, &agent, &model, &effort, &step, &prompt, &denied, step.resume.as_ref());
        // A session the agent no longer has fails before any work: the context was only a cache, so
        // the step starts fresh rather than failing (A-33).
        if let Some((id, _)) = step.resume.as_ref().filter(|_| is_dud(&outcome) && !crate::os::signals::interrupted()) {
            eprintln!("  resuming session {id} failed (exit {:?}); starting fresh", outcome.exit_code);
            outcome = launch(ctx, &agent, &model, &effort, &step, &prompt, &denied, None);
        }
        // A failure that is not the work's fault (an overload, a dropped connection, a crash, a timeout) is tried again
        // inside this session: one ledger record, no new session branch, nothing counted toward `stalled`, the repair
        // budget or the dud streak. What the failed tries spent is charged.
        let mut made = 1;
        let mut spent = crate::usage::Usage::default();
        while let Some(policy) = step.retry.as_ref().filter(|_| failed(&outcome) && !crate::os::signals::interrupted()) {
            let class = agent.classify(&outcome);
            if !policy.again(class, made) {
                break;
            }
            let wait = policy.backoff.wait(made + 1, crate::util::unit_random());
            eprintln!("  ↻ try {made} of {} failed ({}): trying again in {}s", policy.attempts, class.word(), wait.as_secs());
            spent.absorb(&outcome.usage);
            if let Some(end) = crate::features::backoff::wait_between_tries(ctx, wait.as_secs()) {
                outcome.usage.absorb(&spent);
                ctx.charge_usage(crate::usage::Role::Worker, &step.agent, &outcome.usage);
                return Ok(Flow::Stop(end));
            }
            made += 1;
            let note = format!("{prompt}\n\nAn earlier try was cut off ({}); its work is in place: continue, and change nothing that holds.", class.word());
            outcome = launch(ctx, &agent, &model, &effort, &step, &note, &denied, None);
        }
        outcome.usage.absorb(&spent);
        outcome.attempts = made;
        crate::features::protect::after_worker(ctx, &step)?;
        ctx.charge_usage(crate::usage::Role::Worker, &step.agent, &outcome.usage);

        if crate::os::signals::interrupted() {
            // an interrupted session is incomplete: never verified, never lost (A-18.3).
            crate::features::partial::save_and_reset(ctx);
            return Ok(Flow::Stop(ctx.finish_interrupted()));
        }
        eprintln!(
            "  session #{} exited (code {:?}) after {}s{}{}{}  (+{} out-tok, {} total; +${:.4}, ${:.4} total)",
            ctx.session,
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            if outcome.killed_by_watchdog { "  [WATCHDOG-KILLED: hung worker]" } else { "" },
            if outcome.attempts > 1 { format!("  [{} tries]", outcome.attempts) } else { String::new() },
            outcome.usage.tokens.output_or_zero(),
            ctx.tokens_spent,
            outcome.usage.cost_usd.unwrap_or(0.0),
            ctx.cost_spent,
        );
        // warn (loudly) if the agent never touched its forward state file (§5.6 / OQ3).
        if let (Some(step), false) = (ctx.cur_step.as_ref().filter(|s| s.memory), outcome.rate_limited) {
            let now = std::fs::read_to_string(ctx.config_base.join(&step.state)).ok();
            if now == ctx.ext.get::<RLTState>().inject.state_before {
                eprintln!("  ⚠ the worker did not update `{}` this session — the next session inherits stale forward-state.", step.state);
            }
        }

        let dud = is_dud(&outcome);
        let w = &mut ctx.ext.get::<RLTState>().worker;
        w.dud_streak = if dud { w.dud_streak + 1 } else { 0 };
        ctx.scratch.get::<RLTScratch>().outcome = Some(outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "LaunchWorker"
    }
}
