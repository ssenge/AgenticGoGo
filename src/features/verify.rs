//! The verify feature group: the decomposed VERIFY-stage handlers (on_verify).

use anyhow::Result;
use crate::backend::worker::SessionOutcome;
use crate::loop_::{RLTScratch, RLTState, Flow, Handler, LifecycleEvent, LoopState, indent};

/// The early ENFORCED memory floor — FIRST on on_verify, BEFORE any verifying, so the session's facts
/// survive a later panic (R1). Sets `scratch.mem_folded` for the post-verifier refine fold in GATE.
pub struct FloorFold;
impl Handler for FloorFold {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let outcome = ctx.scratch.get::<RLTScratch>().outcome.clone().expect("LaunchWorker set scratch.outcome");
        ctx.scratch.get::<RLTScratch>().mem_folded = fold_memory_floor(ctx, &outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "FloorFold"
    }
}

// The rate-limit exit (`RateLimitBackoff`) lives in `features::backoff` (A-18).

/// Snapshot the pre-step (base) verifier truth so a GATE rollback can restore it (W5). Runs only past
/// the rate-limit exit — exactly like the old `pre_cycle_goals` snapshot after the rate-limit return.
/// Auto-commit the worker's tracked edits on the session branch (GIT_REDESIGN: rlt owns git, the
/// worker never runs git). Runs after the worker (on_run) and the rate-limit check, BEFORE staging
/// (StageSpan/StageMerge) — the session branch is still checked out here, so the commit lands on it
/// and the subsequent merge picks it up. Best-effort → Continue; skipped cleanly when isolation
/// produced no session branch. Runs on skip AND verified steps (both stage the branch's work).
pub struct GitAutoCommit;
impl Handler for GitAutoCommit {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(br) = ctx.ext.get::<RLTState>().git.session_branch.clone() {
            let step = ctx.cur_step.clone().expect("PickStep set cur_step");
            let msg = format!("rlt: session {} ({}) on {}", ctx.session, step.name, step.agent);
            if crate::git::auto_commit_tracked(&ctx.dir, &msg) {
                eprintln!("  [git] rlt committed the worker's edits on {br}");
            }
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "GitAutoCommit"
    }
}

pub struct SnapshotGoals;
impl Handler for SnapshotGoals {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        ctx.scratch.get::<RLTScratch>().pre_cycle_goals = ctx.eng.snapshot_goal_state();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "SnapshotGoals"
    }
}

/// The `skip_verifiers` path (§5.7): no verifiers — keep the branch, extend the span tip, run ceilings only.
/// Runs on a skip step (an internal guard makes it a no-op on a verified step, where StageMerge/RunVerifiers
/// take over). Sets `scratch.res` (ceilings-only) and leaves `scratch.staged = None`.
pub struct StageSpan;
impl Handler for StageSpan {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !ctx.scratch.get::<RLTScratch>().skip_verifiers {
            return Ok(Flow::Continue);
        }
        ctx.emit(LifecycleEvent::Staging);
        let iso = &ctx.cfg.session_isolation;
        let vetoed = ctx.dir.join(&iso.red_file).exists();
        let red_file = iso.red_file.clone();
        let sb = ctx.ext.get::<RLTState>().git.session_branch.clone();
        if vetoed {
            eprintln!("  [span] session #{} VETOED (red_file) → work discarded, not staged", ctx.session);
            crate::git::remove_file(&ctx.dir, &red_file);
            // leave the branch orphaned; the span tip is unchanged.
        } else if let Some(br) = sb {
            let base = crate::features::inject::base_ref(ctx);
            let empty = crate::git::branch_has_no_new_commits(&ctx.dir, &base, &br);
            ctx.scratch.get::<RLTScratch>().span_empty = empty;
            let what = if empty { "changed nothing; " } else { "" };
            eprintln!("  [span] session #{} {what}staged on {br} (skip_verifiers) — nothing merged yet", ctx.session);
            let git = &mut ctx.ext.get::<RLTState>().git;
            git.span_tip = Some(br.clone());
            git.span_branches.push(br);
        }
        // ceilings only (no verifiers ran) — done_if reads stale state and cannot fire, ceilings can.
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let rs = ctx.run_state();
        let res = ctx.eng.run_step(&ctx.dir, &rs, ctx.ruler, &ctx.verifier_model, ctx.verifier_timeout, &step.name, Some(ctx.session), true, ctx.cfg.run_isolation());
        ctx.scratch.get::<RLTScratch>().res = Some(res);
        ctx.scratch.get::<RLTScratch>().staged = None;
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "StageSpan"
    }
}

/// A VERIFIED step only (bypassed on a skip step): stage the merge so the verifiers test the MERGED tree.
/// Sets `scratch.staged`.
pub struct StageMerge;
impl Handler for StageMerge {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let red_file = ctx.cfg.session_isolation.red_file.clone();
        let iso_base = ctx.ext.get::<RLTState>().git.iso_base.clone();
        let staged = ctx.ext.get::<RLTState>().git.session_branch.clone().map(|br| {
            let s = crate::git::stage_session(&ctx.dir, &iso_base, &br, &red_file);
            (br, s)
        });
        ctx.scratch.get::<RLTScratch>().staged = staged;
        Ok(Flow::Continue)
    }
    fn runs_on_skip(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "StageMerge"
    }
}

/// A VERIFIED step only (bypassed on a skip step): run the run-set verifiers against the staged tree, count
/// their spend against the ceilings + the ruler's per-agent tally. Sets `scratch.res`.
pub struct RunVerifiers;
impl Handler for RunVerifiers {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        eprintln!("  running verifiers…");
        ctx.emit(LifecycleEvent::Verify);
        let rs = ctx.run_state();
        // §12: verifiers run in a kernel jail — a confined worker could otherwise rewrite
        // rlt/verifiers/*.sh (in its writable cwd) and escape through rlt's unconfined run.
        // §2.5: the tier is the RUN's, never `step.isolation`. A verifier is an evaluator, not the
        // worker: taking the step's tier let a verifier fired from an `isolation: none` step run
        // unconfined inside a run that sandboxes elsewhere, and dragged the step's `readonly:` list
        // — which names exactly the paths a verifier most needs to execute — in with it.
        let res = ctx.eng.run_step(&ctx.dir, &rs, ctx.ruler, &ctx.verifier_model, ctx.verifier_timeout, &step.name, Some(ctx.session), false, ctx.cfg.run_isolation());
        // §5.6: verifier spend counts against the ceilings — and against the RULER's per-agent tally.
        ctx.charge_ruler(&res.verifier_usage);
        eprint!("{}", indent(&ctx.eng.scoreboard()));
        let tier = ctx.cfg.run_isolation();
        crate::features::evidence::keep(ctx, Some(ctx.session), &res.fresh_verdicts, tier);
        ctx.scratch.get::<RLTScratch>().res = Some(res);
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn runs_on_skip(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "RunVerifiers"
    }
}

/// The early ENFORCED memory floor (so the session's facts survive a later panic). Writes a
/// mechanical session-start note to LOG.md before any verifying runs.
pub fn fold_memory_floor(ctx: &mut LoopState, outcome: &SessionOutcome) -> bool {
    if ctx.cfg.memory.enabled && crate::loop_::keeps_memory(ctx) && !outcome.rate_limited {
        let scoreboard_now = ctx.eng.scoreboard();
        let ended = crate::util::now_epoch();
        let body = crate::core::memory::mechanical_note(
            outcome.exit_code,
            outcome.killed_by_watchdog,
            outcome.rate_limited,
            outcome.duration_secs,
            ended.saturating_sub(outcome.duration_secs),
            ended,
            &scoreboard_now,
            &[],
        );
        ctx.dash.memory_bytes = crate::core::memory::append_entry(
            &ctx.dir,
            ctx.session,
            "session-start",
            &body,
            ctx.cfg.memory.max_kb,
        );
        ctx.publish();
        true
    } else {
        false
    }
}
