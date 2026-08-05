//! The verify feature group: the decomposed VERIFY-stage handlers (on_verify).

use std::time::Duration;

use anyhow::Result;
use crate::backend::worker::SessionOutcome;
use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LifecycleEvent, LoopState, indent};

/// The early ENFORCED memory floor — FIRST on on_verify, BEFORE any judging, so the session's facts
/// survive a later panic (R1). Sets `scratch.mem_folded` for the post-judge refine fold in GATE.
pub struct FloorFold;
impl Handler for FloorFold {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let outcome = ctx.scratch.get::<AGGScratch>().outcome.clone().expect("LaunchWorker set scratch.outcome");
        ctx.scratch.get::<AGGScratch>().mem_folded = fold_memory_floor(ctx, &outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "FloorFold"
    }
}

/// The rate-limit exit: a rate-limited session is INCOMPLETE. Plain rate-limit → `SkipSession` (skip
/// gate + session_end, loop on). A ceiling tripped DURING backoff → `Stop(Halt)` (abort_now emits
/// Finished first). Ceilings are checked even here so an all-night spin still trips the guard (§5.5).
pub struct RateLimitBackoff;
impl Handler for RateLimitBackoff {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        // Copy the flags out first so the immutable scratch borrow ends before the mutable ctx calls
        // (abort_now / emit) below.
        let (rate_limited, reset) = ctx
            .scratch
            .get::<AGGScratch>()
            .outcome
            .as_ref()
            .map(|o| (o.rate_limited, o.rate_limit_reset))
            .unwrap_or((false, None));
        if !rate_limited {
            return Ok(Flow::Continue);
        }
        // Wait until the reported reset (+ a small buffer to clear the boundary) if we know it —
        // this is what lets a SUBSCRIPTION limit ("resets 12:50pm") resume promptly instead of
        // blind fixed retries. Unknown reset ⇒ the fixed `ratelimit_backoff_secs`. See stream.rs.
        const BUFFER_SECS: u64 = 90;
        let secs = match reset {
            Some(r) => r.saturating_sub(crate::util::now_epoch()).saturating_add(BUFFER_SECS),
            None => ctx.cfg.ratelimit_backoff_secs,
        };
        match reset {
            Some(_) => eprintln!("  rate limit detected — waiting {}m{:02}s until it resets", secs / 60, secs % 60),
            None => eprintln!("  rate limit detected — backing off {secs}s (no reset time reported)"),
        }
        if ctx.cfg.memory.enabled {
            crate::core::memory::clear_scratch(&ctx.dir, ctx.session);
        }
        ctx.emit(LifecycleEvent::Backoff);

        // Sleep in short steps so a long wait (a subscription reset can be hours out) stays
        // responsive: it can be Ctrl-C'd, and §5.5 item 6's ceilings (`wall_hours`/`over_budget`)
        // are re-checked as it waits — an all-night rate-limit wait must still be able to abort.
        let mut slept = 0u64;
        while slept < secs {
            if crate::os::signals::interrupted() {
                return Ok(Flow::Stop(ctx.finish_interrupted()));
            }
            let rs = ctx.run_state();
            let ceil = ctx.eng.conditions_only(&rs);
            if ceil.halt {
                // ponytail: aborts without firing `notify.cmd` — this path leaves the session
                // before on_gate, so `NotifyOnStuck` never sees it (STUCK_NOTIFY §12.6). Accepted:
                // reaching here means a ceiling tripped while ALREADY blocked on a rate limit, so
                // the operator's next question is about quota, not about the loop being stuck.
                // Upgrade path: `notify::halt_ping(ctx, ceil.halt_reason.as_deref())` before
                // `abort_now`, which is where the baseline path now calls it from.
                eprintln!("  ⚠ ceiling tripped during backoff — aborting");
                let outcome = ctx.abort_now(&format!("abort_if: {}", ceil.halt_reason.unwrap_or_default()));
                return Ok(Flow::Stop(outcome));
            }
            let chunk = (secs - slept).min(5);
            std::thread::sleep(Duration::from_secs(chunk));
            slept += chunk;
        }
        Ok(Flow::SkipSession)
    }
    fn name(&self) -> &'static str {
        "RateLimitBackoff"
    }
}

/// Snapshot the pre-step (base) judge truth so a GATE rollback can restore it (W5). Runs only past
/// the rate-limit exit — exactly like the old `pre_cycle_goals` snapshot after the rate-limit return.
/// Auto-commit the worker's tracked edits on the session branch (GIT_REDESIGN: agg owns git, the
/// worker never runs git). Runs after the worker (on_run) and the rate-limit check, BEFORE staging
/// (StageSpan/StageMerge) — the session branch is still checked out here, so the commit lands on it
/// and the subsequent merge picks it up. Best-effort → Continue; skipped cleanly when isolation
/// produced no session branch. Runs on skip AND judged steps (both stage the branch's work).
pub struct GitAutoCommit;
impl Handler for GitAutoCommit {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(br) = ctx.ext.get::<AGGState>().git.session_branch.clone() {
            let step = ctx.cur_step.clone().expect("PickStep set cur_step");
            let msg = format!("agg: session {} ({}) on {}", ctx.session, step.name, step.agent);
            if crate::git::auto_commit_tracked(&ctx.dir, &msg) {
                eprintln!("  [git] agg committed the worker's edits on {br}");
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
        ctx.scratch.get::<AGGScratch>().pre_cycle_goals = ctx.eng.snapshot_goal_state();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "SnapshotGoals"
    }
}

/// The `skip_judges` path (§5.7): no judges — keep the branch, extend the span tip, run ceilings only.
/// Runs on a skip step (an internal guard makes it a no-op on a judged step, where StageMerge/RunJudges
/// take over). Sets `scratch.res` (ceilings-only) and leaves `scratch.staged = None`.
pub struct StageSpan;
impl Handler for StageSpan {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !ctx.scratch.get::<AGGScratch>().skip_judges {
            return Ok(Flow::Continue);
        }
        ctx.emit(LifecycleEvent::Staging);
        let iso = &ctx.cfg.session_isolation;
        let vetoed = ctx.dir.join(&iso.red_file).exists();
        let red_file = iso.red_file.clone();
        let sb = ctx.ext.get::<AGGState>().git.session_branch.clone();
        if vetoed {
            eprintln!("  [span] session #{} VETOED (red_file) → work discarded, not staged", ctx.session);
            crate::git::remove_file(&ctx.dir, &red_file);
            // leave the branch orphaned; the span tip is unchanged.
        } else if let Some(br) = sb {
            eprintln!("  [span] session #{} staged on {br} (skip_judges) — nothing merged yet", ctx.session);
            let git = &mut ctx.ext.get::<AGGState>().git;
            git.span_tip = Some(br.clone());
            git.span_branches.push(br);
        }
        // ceilings only (no judges ran) — done_if reads stale state and cannot fire, ceilings can.
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let rs = ctx.run_state();
        let res = ctx.eng.run_step(&ctx.dir, &rs, ctx.ruler, &ctx.judge_model, ctx.judge_timeout, &step.name, Some(ctx.session), true, ctx.cfg.run_isolation());
        ctx.scratch.get::<AGGScratch>().res = Some(res);
        ctx.scratch.get::<AGGScratch>().staged = None;
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "StageSpan"
    }
}

/// A JUDGED step only (bypassed on a skip step): stage the merge so the judges test the MERGED tree.
/// Sets `scratch.staged`.
pub struct StageMerge;
impl Handler for StageMerge {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let red_file = ctx.cfg.session_isolation.red_file.clone();
        let iso_base = ctx.ext.get::<AGGState>().git.iso_base.clone();
        let staged = ctx.ext.get::<AGGState>().git.session_branch.clone().map(|br| {
            let s = crate::git::stage_session(&ctx.dir, &iso_base, &br, &red_file);
            (br, s)
        });
        ctx.scratch.get::<AGGScratch>().staged = staged;
        Ok(Flow::Continue)
    }
    fn runs_on_skip(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "StageMerge"
    }
}

/// A JUDGED step only (bypassed on a skip step): run the run-set judges against the staged tree, count
/// their spend against the ceilings + the ruler's per-agent tally. Sets `scratch.res`.
pub struct RunJudges;
impl Handler for RunJudges {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        eprintln!("  running judges…");
        ctx.emit(LifecycleEvent::Verify);
        let rs = ctx.run_state();
        // §12: judges run in a kernel jail — a confined worker could otherwise rewrite
        // agg/judges/*.sh (in its writable cwd) and escape through agg's unconfined run.
        // §2.5: the tier is the RUN's, never `step.isolation`. A judge is an evaluator, not the
        // worker: taking the step's tier let a judge fired from an `isolation: none` step run
        // unconfined inside a run that sandboxes elsewhere, and dragged the step's `readonly:` list
        // — which names exactly the paths a judge most needs to execute — in with it.
        let res = ctx.eng.run_step(&ctx.dir, &rs, ctx.ruler, &ctx.judge_model, ctx.judge_timeout, &step.name, Some(ctx.session), false, ctx.cfg.run_isolation());
        // §5.6: judge spend counts against the ceilings — and against the RULER's per-agent tally.
        ctx.tokens_spent += res.judge_tokens;
        if let Some(c) = res.judge_cost {
            ctx.cost_spent += c;
        }
        let ruler_agent = ctx.cfg.judge.agent.clone();
        ctx.charge(&ruler_agent, res.judge_tokens, res.judge_cost);
        eprint!("{}", indent(&ctx.eng.scoreboard()));
        ctx.scratch.get::<AGGScratch>().res = Some(res);
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn runs_on_skip(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "RunJudges"
    }
}

/// The early ENFORCED memory floor (so the session's facts survive a later panic). Writes a
/// mechanical session-start note to LOG.md before any judging runs.
pub fn fold_memory_floor(ctx: &mut LoopState, outcome: &SessionOutcome) -> bool {
    if ctx.cfg.memory.enabled && !outcome.rate_limited {
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
