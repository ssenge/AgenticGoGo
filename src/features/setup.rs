//! The setup feature group — the run-start bootstrap: finalize the prompt prefix (`Setup`), the pre-session baseline judge pass (`Baseline`), and spawning the user's `background` watchers (`BackgroundSpawn`).

use anyhow::Result;
use std::time::{Duration, Instant};

use crate::bus::Bus;
use crate::loop_::{AGGState, Flow, Handler, LoopState, RunOutcome, indent};
use crate::state::Phase;
use crate::util::now_epoch;

/// Finalize the run bootstrap before the loop: gather the `prompt_includes` into `prompt_prefix`,
/// reset the summary clock, prepare the on-disk memory scratch, and open the operator bus. On
/// `on_run_start`, after the baseline pass — which runs AFTER `on_start` (so on_start→prompt_includes
/// order holds) and BEFORE the loop (so the first `compose` sees the prefix). Behavior-unchanged:
/// nothing between the `LoopState` build and here reads `prompt_prefix` (baseline judges only).
pub struct Setup;
impl Handler for Setup {
    fn run(&self, st: &mut LoopState) -> Result<Flow> {
        st.ext.get::<AGGState>().inject.prompt_prefix =
            crate::hooks::gather_prompt_includes(&st.cfg.prompt_includes, st.dir);
        st.ext.get::<AGGState>().summary.last_summary =
            Some(Instant::now() - Duration::from_secs(st.cfg.summary.min_interval_secs));
        if st.cfg.memory.enabled {
            crate::core::memory::ensure_scratch_dir(st.dir);
            crate::core::memory::sweep_scratch(st.dir);
        }
        st.bus = Bus::open(st.dir).ok();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "Setup"
    }
}

/// Baseline pass (§5.5.1): judge the untouched repo ONCE before session 1 and write `baseline`
/// verdicts, on `on_run_start`. Its two launch-time early exits — `abort_if` already true → Halt,
/// `done_if` already satisfied → GoalsMet — come back as `Flow::Stop`; it finalizes dash + ledger
/// itself (exactly as the old inline pass did) before returning, so the core just propagates the
/// outcome. Verbatim port of the former inline baseline block.
pub struct Baseline;
impl Handler for Baseline {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        eprintln!("  baseline: running judges once before the first session…");
        ctx.dash.phase = Phase::Verify;
        ctx.publish();
        let dir = ctx.dir;
        let ruler = ctx.ruler;
        let judge_model = ctx.judge_model.clone();
        let judge_timeout = ctx.judge_timeout;
        let rs = ctx.run_state();
        let pre = ctx.eng.run_step(dir, &rs, ruler, &judge_model, judge_timeout, "baseline", None, false);
        ctx.tokens_spent += pre.judge_tokens;
        if let Some(c) = pre.judge_cost {
            ctx.cost_spent += c;
        }
        let ruler_agent = ctx.cfg.judge.agent.clone();
        ctx.charge(&ruler_agent, pre.judge_tokens, pre.judge_cost);
        eprint!("{}", indent(&ctx.eng.scoreboard()));
        ctx.publish();
        crate::core::verdicts::append(dir, None, "baseline", &pre.fresh_verdicts, crate::core::verdicts::Outcome::Baseline)?;
        if pre.halt {
            eprintln!("⚠ ABORT at baseline — abort_if already true: {}", pre.halt_reason.clone().unwrap_or_default());
            ctx.dash.phase = Phase::Done;
            ctx.dash.finished = true;
            ctx.dash.finish_reason = format!("ABORT at baseline: {}", pre.halt_reason.clone().unwrap_or_default());
            let (gm, gt) = ctx.eng.tally();
            ctx.ledger.update(0, 0, gm, gt);
            ctx.ledger.finish(now_epoch(), &format!("abort-at-baseline:{}", pre.halt_reason.unwrap_or_default()));
            ctx.publish();
            return Ok(Flow::Stop(RunOutcome::Halt));
        }
        if pre.stop {
            eprintln!("✔ done_if already satisfied at launch — nothing to do.");
            ctx.dash.phase = Phase::Done;
            ctx.dash.finished = true;
            ctx.dash.finish_reason = "already satisfied at launch".into();
            let (gm, gt) = ctx.eng.tally();
            ctx.ledger.update(0, 0, gm, gt);
            ctx.ledger.finish(now_epoch(), "already-satisfied");
            ctx.publish();
            return Ok(Flow::Stop(RunOutcome::GoalsMet));
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "Baseline"
    }
}

/// Spawn the user's long-lived `background` watchers into the loop's process group (so the straggler
/// reaper cleans them up), on the `background` hook fired once at run start. Best-effort → Continue.
pub struct BackgroundSpawn {
    pub cmds: Vec<String>,
}
impl Handler for BackgroundSpawn {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        crate::hooks::spawn_background(&self.cmds, ctx.dir);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "BackgroundSpawn"
    }
}
