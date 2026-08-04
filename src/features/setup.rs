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
            crate::hooks::gather_prompt_includes(&st.cfg.prompt_includes, &st.dir);
        st.ext.get::<AGGState>().summary.last_summary =
            Some(Instant::now() - Duration::from_secs(st.cfg.summary.min_interval_secs));
        if st.cfg.memory.enabled {
            crate::core::memory::ensure_scratch_dir(&st.dir);
            crate::core::memory::sweep_scratch(&st.dir);
        }
        st.bus = Bus::open(&st.dir).ok();
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
        let ruler = ctx.ruler;
        let judge_model = ctx.judge_model.clone();
        let judge_timeout = ctx.judge_timeout;
        let rs = ctx.run_state();
        // Confined with the RUN-level tier, exactly as the `on_stop` hook is (registry.rs) — the
        // same "no step context yet" problem, so the same answer.
        //
        // The old rationale here was "baseline runs on the clean committed tree, so the judges
        // cannot have been tampered with yet". That is false ACROSS RUNS, and was demonstrated: the
        // carve-out covers `agg/private/`, but `agg/judges/` is committed BY DESIGN (the moat), so a
        // confined worker in run 1 rewrites a judge, agg commits it, and run 2's baseline executes
        // it — unconfined, before any jail exists — forging `merged` rows straight into the ledger.
        // `stalled` then reports met and an `abort_if: "stalled"` project has its worker end its own
        // run. Clean-and-committed is not the same as untampered.
        let pre = ctx.eng.run_step(&ctx.dir, &rs, ruler, &judge_model, judge_timeout, "baseline", None, false, ctx.cfg.run_isolation());
        ctx.tokens_spent += pre.judge_tokens;
        if let Some(c) = pre.judge_cost {
            ctx.cost_spent += c;
        }
        let ruler_agent = ctx.cfg.judge.agent.clone();
        ctx.charge(&ruler_agent, pre.judge_tokens, pre.judge_cost);
        eprint!("{}", indent(&ctx.eng.scoreboard()));
        ctx.publish();
        crate::core::verdicts::append(&ctx.dir, None, "baseline", &pre.fresh_verdicts, crate::core::verdicts::Outcome::Baseline)?;
        if pre.halt {
            eprintln!("⚠ ABORT at baseline — abort_if already true: {}", pre.halt_reason.clone().unwrap_or_default());
            // The "stop + notify" policy has to hold HERE too, and this is its likeliest trigger:
            // a stale `agg/state/BLOCKED.md` survives a crash, a reboot and a rollback, so the run
            // an operator kicked off remotely can die at t=0. Without this the config written
            // expressly to page them on a stop stops and pages nobody.
            crate::features::notify::halt_ping(ctx, pre.halt_reason.as_deref());
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
        crate::hooks::spawn_background(&self.cmds, &ctx.dir);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "BackgroundSpawn"
    }
}

#[cfg(test)]
mod tests {
    /// The baseline pass must confine its judges with the RUN-level tier, not `Isolation::None`.
    ///
    /// This pins a real, reproduced bypass: `agg/judges/` is committed by design (the moat), so a
    /// sandboxed worker in run 1 can rewrite a judge, agg commits it, and run 2's baseline — which
    /// used to hardcode `Isolation::None` on the premise that a clean committed tree is untampered —
    /// executed it before any jail existed, forging `merged` rows into `agg/private/verdicts.jsonl`.
    /// `stalled` then reported met and an `abort_if: "stalled"` project ended its own run.
    ///
    /// Asserted on the CONFIG→tier mapping rather than by spawning a sandbox (that lives in the
    /// `#[ignore]`d kernel test): the defect was reading `None` from a hardcode, so what needs
    /// pinning is that the tier is DERIVED and non-`None` whenever any step asks for confinement.
    #[test]
    fn a_sandboxed_config_gives_the_baseline_pass_a_confining_tier() {
        use crate::core::config::AggConfig;
        use crate::isolation::Isolation;
        let load = |iso: &str| -> AggConfig {
            serde_yaml::from_str(&format!(
                "project: p\nsteps:\n  worker:\n    isolation: {iso}\nsequence:\n  steps: [worker]\n  done_if: \"ok\"\n"
            ))
            .expect("fixture config parses")
        };
        assert_eq!(
            load("sandbox").run_isolation(),
            Isolation::Sandbox,
            "a project with a sandboxed step must NOT baseline-judge unconfined — that is the bypass"
        );
        // …and a project that asked for nothing still pays nothing: no behaviour change for the
        // default tier, which is what makes this fix safe to ship.
        assert_eq!(load("none").run_isolation(), Isolation::None);
    }
}
