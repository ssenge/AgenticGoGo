//! The harness loop: `agg run`.
//!
//! # A deterministic outer loop around a stochastic inner loop
//!
//! agg is the OUTER loop. It is plain Rust, and its control flow is DETERMINISTIC. Four stages per
//! STEP (§5.5) — one worker session per step:
//!
//! ```text
//!   INJECT  next step from the sequence → state + steering → the worker's prompt
//!   RUN     the fresh worker for THIS step's (agent, model, effort) — the ONE stochastic step
//!   VERIFY  agg runs the run-set judges itself, externally, against the (staged) filesystem
//!   GATE    keep / roll back / stage the span · check done_if / abort_if · repeat
//! ```
//!
//! The sequence repeats from the top forever until `done_if` fires (exit 0) or `abort_if` fires
//! (exit 3). Per-step agent/model/effort are resolved at session-build time (the singleton is gone).

use crate::backend::worker::{self, SessionOutcome};
use crate::backend::AgentBackend;
use crate::bus::{Bus, Command};
use crate::core::config::{AggConfig, ResolvedStep};
use crate::core::engine::{CycleResult, Engine, GoalRuntime, RunState};
use crate::core::sequence::{self, Cursor, Statement};
use crate::core::stop::{self, StopContext};
use crate::state::{DashboardState, LiveState, Phase};
use crate::summary;
use anyhow::Result;
use std::path::Path;
use std::time::{Duration, Instant};

/// How the loop ended — mapped to a process exit code in `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// `done_if` (the Definition of Done) fired, or was already satisfied at launch.
    GoalsMet,
    /// `abort_if` fired (invariant regressed, budget/cost/iteration/wall ceiling) — NOT success.
    Halt,
    /// The `--max-sessions` cap was reached with the DoD not met.
    MaxSessions,
    /// The operator stopped the run.
    Stopped,
}

impl RunOutcome {
    pub fn exit_code(self) -> u8 {
        match self {
            RunOutcome::GoalsMet => 0,
            RunOutcome::Stopped => 0,
            RunOutcome::Halt => 3,
            RunOutcome::MaxSessions => 4,
        }
    }
}

/// Something the loop DID, at the moment it did it — the single source of truth for `dash.phase`.
enum LifecycleEvent {
    Inject,
    Run,
    Verify,
    Gate,
    Backoff,
    /// a `skip_judges` step whose work is being STAGED onto the span (§7.4).
    Staging,
    Finished { reason: String, ledger_tag: String },
}

impl LifecycleEvent {
    fn phase(&self) -> Phase {
        match self {
            LifecycleEvent::Inject => Phase::Inject,
            LifecycleEvent::Run => Phase::Run,
            LifecycleEvent::Verify => Phase::Verify,
            LifecycleEvent::Gate => Phase::Gate,
            LifecycleEvent::Backoff => Phase::Backoff,
            LifecycleEvent::Staging => Phase::Staging,
            LifecycleEvent::Finished { .. } => Phase::Done,
        }
    }
}

/// The worker's ENTIRE pushed input. The full brief is composed into `agg/state/INSTRUCTIONS.md`
/// every session and the worker is pointed at it — this kills the argv size ceiling AND the
/// argv-parse fragility (a huge `-p` value that could start with `-`), and makes the exact context
/// the worker saw inspectable on disk. The path is RELATIVE so it resolves from the worker's cwd
/// (the project dir) on every backend (Claude/Copilot `-p`, Codex positional) with no per-backend
/// special-casing — it is just a short, dash-free prompt value.
const INSTRUCTIONS_POINTER: &str =
    "Read the file `agg/state/INSTRUCTIONS.md` in full and follow it — it is your complete brief for this session.";

/// The standing "Before you exit" footer of every session's brief (the wiki/OKF guidance lives here).
/// Kept as a real markdown file (`include_str!`'d, like the scaffolds + skills) rather than an inline
/// string; `{{STATE}}` is filled with the step's state path when composed.
const EXIT_FOOTER: &str = include_str!("../plugin/scaffold/exit_footer.md");

// ── the lifecycle registry (HOOK_REDESIGN §3.1/§5) ────────────────────────────────────────────
// Handlers are `.add()`ed to hook points in code and dispatched in order by the loop — the seed of
// "every task is a hook". agg's own tasks (pick/compose/judges/gate/memory) are handlers too; only
// the true scheduler control flow (over_max_sessions, worker_is_broken, the phase emits) stays core.
//
// The context a handler receives is the whole `LoopState` (§8: the context IS the run/session
// state). Handlers run STRICTLY SEQUENTIALLY, each with an exclusive `&mut LoopState`, so passing
// the whole state is legal with no borrow gymnastics. The `Lifecycle` is owned by `run()` and passed
// ALONGSIDE the state (never stored in it) — that disjointness is what keeps the borrow sound.

/// A handler's control-flow result (§3.1). MINIMAL: reason/ledger_tag are NOT here — every Stop
/// path already `emit`s `Finished{reason,ledger_tag}` itself before yielding the outcome, so a
/// handler emits then returns `Flow::Stop(outcome)` and the core never re-emits.
enum Flow {
    Continue,
    /// stop the rest of THIS session's hooks, loop to the next session (the rate-limit path).
    SkipSession,
    Stop(RunOutcome),
}

/// What a whole hook-point dispatch produced (`None` = drained cleanly, fall through to next hook).
enum End {
    NextSession,
    Stop(RunOutcome),
}

/// The per-session channel between stage-handlers. ONE field on `LoopState`, reset each session at
/// the loop top so no field (esp. `prompt`) leaks across sessions. NOT the on-disk memory scratch
/// (`memory::clear_scratch`) — a different thing. Grows a field per increment as stages convert.
#[derive(Default)]
struct Scratch {
    /// `WriteInstructions` (on_session_start) → the RUN launch. Replaces `Injected::Prompt`.
    prompt: Option<String>,
    /// `PickStep` sets it from `cur_step.skip_judges`; `run_hook`'s predicate uses it to bypass a
    /// handler that opts out of skip steps (`runs_on_skip()==false`). Truth stays in `self.cur_step`.
    skip_judges: bool,
    /// `LaunchWorker` (on_run) → VERIFY/GATE. Replaces `run()`'s `Option<SessionOutcome>` return.
    outcome: Option<SessionOutcome>,
    /// `FloorFold` (on_verify) → the post-judge refine fold in GATE. Was `Verified.mem_folded`.
    mem_folded: bool,
    /// `SnapshotGoals` (on_verify) → a rollback in GATE restores it. Was `Verified.pre_cycle_goals`.
    pre_cycle_goals: Vec<GoalRuntime>,
    /// `StageSpan` (skip) XOR `RunJudges` (judged) → GATE; REWRITTEN by GATE on a rollback. Was `Verified.res`.
    res: Option<CycleResult>,
    /// `StageMerge` (judged) → GATE's keep/rollback. `None` on a skip step. Was `Verified.staged`.
    staged: Option<(String, crate::git::StagedSession)>,
    /// `GateKeepRollback` (on_gate) → `RefineFold`'s "session ROLLED BACK" prefix. Staged-!keep only.
    rolled_back: bool,
    /// `Summarize` (on_session_end) → `RefineFold`'s mechanical+summary source choice.
    summarized_this_cycle: bool,
}

trait Handler {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow>;
    /// Whether this handler runs on a `skip_judges` step. Default yes; the judged-merge handlers
    /// override to `false` so a skip step bypasses them (mirrors the old `if !skip`).
    fn runs_on_skip(&self) -> bool {
        true
    }
    /// Stable name for the registration-order characterization tests (order is outcome-invisible).
    #[cfg_attr(not(test), allow(dead_code))]
    fn name(&self) -> &'static str;
}

/// Dispatch one hook point's handlers in order, honoring `Flow`. A handler's hard `Err` bubbles out
/// (it is NOT a `RunOutcome` — e.g. `verdicts::append`, or the worker-broken guard). §3.1.
fn run_hook(hooks: &[Box<dyn Handler>], st: &mut LoopState) -> Result<Option<End>> {
    for h in hooks {
        if st.scratch.skip_judges && !h.runs_on_skip() {
            continue;
        }
        match h.run(st)? {
            Flow::Continue => {}
            Flow::SkipSession => return Ok(Some(End::NextSession)),
            Flow::Stop(o) => return Ok(Some(End::Stop(o))),
        }
    }
    Ok(None)
}

/// A user shell-hook list wrapped as a handler — best-effort, non-fatal (exactly `hooks::run`).
struct ShellHook {
    label: &'static str,
    cmds: Vec<String>,
}
impl Handler for ShellHook {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        crate::hooks::run(self.label, &self.cmds, ctx.dir);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        self.label
    }
}

// ── on_session_start handlers = the old INJECT stage, decomposed (HOOK_REDESIGN §4) ──────────────

/// Drain the operator bus at the session boundary (inject / pause / set-budget / stop / note).
struct BusDrain;
impl Handler for BusDrain {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let cmds = match &ctx.bus {
            Some(bus) => bus.drain(),
            None => Vec::new(),
        };
        for cmd in cmds {
            match cmd {
                Command::InjectInstruction { text } => {
                    eprintln!("  [bus] inject-instruction → prepended to next session");
                    ctx.pending_instruction = Some(match ctx.pending_instruction.take() {
                        Some(prev) => format!("{prev}\n\n{text}"),
                        None => text,
                    });
                }
                Command::SetBudget { total } => {
                    eprintln!("  [bus] set-budget → {:?}", total);
                    ctx.budget_total = total;
                }
                Command::Pause => {
                    eprintln!("  [bus] pause → waiting for resume/stop…");
                    let stopped = match &ctx.bus {
                        Some(bus) => wait_for_resume(bus),
                        None => None,
                    };
                    if let Some(reason) = stopped {
                        return Ok(Flow::Stop(ctx.stopped_via_bus(reason)));
                    }
                }
                Command::Resume => {}
                Command::Stop { reason } => return Ok(Flow::Stop(ctx.stopped_via_bus(reason))),
                Command::Note { text } => eprintln!("  [bus] note: {text}"),
            }
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "BusDrain"
    }
}

/// Advance the sequence cursor → resolve the next step; then (ONLY on a resolved step) bump the
/// session counter, update the ledger, print the banner, and set `cur_step` + `scratch.skip_judges`.
struct PickStep;
impl Handler for PickStep {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let step_name = {
            let rs = ctx.run_state();
            let eng = &ctx.eng;
            let picked = ctx.cursor.next_step(&mut |cond| {
                let sc = StopContext {
                    judges: &eng.judges,
                    judge_errors: &[],
                    tokens_spent: rs.tokens_spent,
                    budget_total: rs.budget_total,
                    cost_spent: rs.cost_spent,
                    cost_limit: rs.cost_limit,
                    sessions_done: rs.sessions_done,
                    max_sessions: rs.max_sessions,
                    wall_hours: rs.wall_hours,
                };
                stop::evaluate(cond, &sc)
            });
            match picked {
                Ok(n) => n,
                Err(e) => return Ok(Flow::Stop(ctx.abort_now(&format!("sequence error: {e}")))),
            }
        };
        let step = match ctx.cfg.resolve_step(&step_name) {
            Ok(s) => s,
            Err(e) => return Ok(Flow::Stop(ctx.abort_now(&format!("{e}")))),
        };

        ctx.session += 1;
        ctx.dash.session = ctx.session;
        ctx.dash.lifetime_session = ctx.lifetime_base + ctx.session;
        let (gm, gt) = ctx.eng.tally();
        ctx.ledger.update(ctx.session, ctx.tokens_spent, gm, gt);
        let up = ctx.loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{} (#{} lifetime)  step `{}` [{}]  (up {}h{:02}m)  goals {gm}/{gt} ────",
            ctx.session,
            ctx.dash.lifetime_session,
            step.name,
            step.agent,
            up / 3600,
            (up % 3600) / 60,
        );
        // skip_judges into the channel BEFORE cur_step is moved (later hooks read the channel).
        ctx.scratch.skip_judges = step.skip_judges;
        ctx.cur_step = Some(step);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "PickStep"
    }
}

/// Cut this session's git branch off the span tip (or base).
struct SessionBranchCut;
impl Handler for SessionBranchCut {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let iso = &ctx.cfg.session_isolation;
        let base_ref = ctx.base_ref().to_string();
        let br = crate::git::session_branch(&iso.branch_prefix, &ctx.cfg.project, ctx.session);
        crate::git::remove_file(ctx.dir, &iso.red_file); // clear a stale veto
        ctx.session_branch = if crate::git::create_branch(ctx.dir, &br, &base_ref) {
            eprintln!("  [iso] session #{} on branch {br} (off {base_ref})", ctx.session);
            Some(br)
        } else {
            eprintln!("  [iso] could not create session branch — running on {base_ref}");
            None
        };
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "SessionBranchCut"
    }
}

/// Capture the state file (for the staleness warning) + compose the brief into `INSTRUCTIONS.md`;
/// the tiny pointer (or the inline brief on a write failure) goes to `scratch.prompt`.
struct WriteInstructions;
impl Handler for WriteInstructions {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(change) = crate::os::spawns::scan(ctx.dir) {
            eprintln!("  [spawn] {change}");
        }
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let state_path = ctx.config_base.join(&step.state);
        ctx.state_before = std::fs::read_to_string(&state_path).ok();
        let prompt = ctx.compose_prompt(&step);
        ctx.scratch.prompt = Some(prompt);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "WriteInstructions"
    }
}

/// Reset the on-disk per-session memory scratch for the fresh session.
struct ClearMemScratch;
impl Handler for ClearMemScratch {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if ctx.cfg.memory.enabled {
            crate::core::memory::clear_scratch(ctx.dir, ctx.session);
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "ClearMemScratch"
    }
}

// ── on_run handler = the old RUN stage (HOOK_REDESIGN §4) ─────────────────────────────────────────

/// Launch the fresh worker for this step's (agent, model, effort). ONE handler: the unknown-agent
/// and SIGINT early returns forbid splitting. Reads `scratch.prompt`, writes `scratch.outcome`.
/// `Flow::Stop(finish_interrupted())` on SIGINT — the only control-flow exit of the RUN stage.
struct LaunchWorker;
impl Handler for LaunchWorker {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let prompt = ctx.scratch.prompt.take().expect("WriteInstructions set scratch.prompt");
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let agent = match step.backend() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  step `{}` names an unknown agent: {e}", step.name);
                ctx.dud_streak += 1;
                ctx.scratch.outcome = Some(SessionOutcome {
                    exit_code: None,
                    duration_secs: 0,
                    rate_limited: false,
                    killed_by_watchdog: false,
                    output_tokens: 0,
                    cost_usd: 0.0,
                    thoughts: vec![],
                    session_id: None,
                });
                return Ok(Flow::Continue);
            }
        };
        let model = step.model(agent).to_string();
        let effort = step.effort(agent).to_string();
        let outcome = worker::run_session(
            ctx.cfg,
            agent,
            &model,
            &effort,
            &step.worker_args,
            &prompt,
            ctx.dir,
            ctx.session,
            &ctx.live,
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
            if now == ctx.state_before {
                eprintln!("  ⚠ the worker did not update `{}` this session — the next session inherits stale forward-state.", step.state);
            }
        }

        let dud = !outcome.rate_limited && outcome.exit_code != Some(0) && outcome.output_tokens == 0;
        ctx.dud_streak = if dud { ctx.dud_streak + 1 } else { 0 };
        ctx.scratch.outcome = Some(outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "LaunchWorker"
    }
}

// ── on_verify handlers = the old VERIFY stage, decomposed (HOOK_REDESIGN §4) ───────────────────────

/// The early ENFORCED memory floor — FIRST on on_verify, BEFORE any judging, so the session's facts
/// survive a later panic (R1). Sets `scratch.mem_folded` for the post-judge refine fold in GATE.
struct FloorFold;
impl Handler for FloorFold {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let outcome = ctx.scratch.outcome.clone().expect("LaunchWorker set scratch.outcome");
        ctx.scratch.mem_folded = ctx.fold_memory_floor(&outcome);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "FloorFold"
    }
}

/// The rate-limit exit: a rate-limited session is INCOMPLETE. Plain rate-limit → `SkipSession` (skip
/// gate + session_end, loop on). A ceiling tripped DURING backoff → `Stop(Halt)` (abort_now emits
/// Finished first). Ceilings are checked even here so an all-night spin still trips the guard (§5.5).
struct RateLimitBackoff;
impl Handler for RateLimitBackoff {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let rate_limited = ctx.scratch.outcome.as_ref().map(|o| o.rate_limited).unwrap_or(false);
        if !rate_limited {
            return Ok(Flow::Continue);
        }
        let secs = ctx.cfg.ratelimit_backoff_secs;
        eprintln!("  rate limit detected — backing off {secs}s");
        if ctx.cfg.memory.enabled {
            crate::core::memory::clear_scratch(ctx.dir, ctx.session);
        }
        // §5.5 item 6: check the ceilings even here — an all-night rate-limit spin must still trip
        // `wall_hours`/`over_budget`.
        let rs = ctx.run_state();
        let ceil = ctx.eng.conditions_only(&rs);
        if ceil.halt {
            eprintln!("  ⚠ ceiling tripped during backoff — aborting");
            let outcome = ctx.abort_now(&format!("abort_if: {}", ceil.halt_reason.unwrap_or_default()));
            return Ok(Flow::Stop(outcome));
        }
        ctx.emit(LifecycleEvent::Backoff);
        std::thread::sleep(Duration::from_secs(secs));
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
struct GitAutoCommit;
impl Handler for GitAutoCommit {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(br) = ctx.session_branch.clone() {
            let step = ctx.cur_step.clone().expect("PickStep set cur_step");
            let msg = format!("agg: session {} ({}) on {}", ctx.session, step.name, step.agent);
            if crate::git::auto_commit_tracked(ctx.dir, &msg) {
                eprintln!("  [git] agg committed the worker's edits on {br}");
            }
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "GitAutoCommit"
    }
}

struct SnapshotGoals;
impl Handler for SnapshotGoals {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        ctx.scratch.pre_cycle_goals = ctx.eng.snapshot_goal_state();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "SnapshotGoals"
    }
}

/// The `skip_judges` path (§5.7): no judges — keep the branch, extend the span tip, run ceilings only.
/// Runs on a skip step (an internal guard makes it a no-op on a judged step, where StageMerge/RunJudges
/// take over). Sets `scratch.res` (ceilings-only) and leaves `scratch.staged = None`.
struct StageSpan;
impl Handler for StageSpan {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !ctx.scratch.skip_judges {
            return Ok(Flow::Continue);
        }
        ctx.emit(LifecycleEvent::Staging);
        let iso = &ctx.cfg.session_isolation;
        let vetoed = ctx.dir.join(&iso.red_file).exists();
        let red_file = iso.red_file.clone();
        let sb = ctx.session_branch.clone();
        if vetoed {
            eprintln!("  [span] session #{} VETOED (red_file) → work discarded, not staged", ctx.session);
            crate::git::remove_file(ctx.dir, &red_file);
            // leave the branch orphaned; the span tip is unchanged.
        } else if let Some(br) = sb {
            eprintln!("  [span] session #{} staged on {br} (skip_judges) — nothing merged yet", ctx.session);
            ctx.span_tip = Some(br.clone());
            ctx.span_branches.push(br);
        }
        // ceilings only (no judges ran) — done_if reads stale state and cannot fire, ceilings can.
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let rs = ctx.run_state();
        let res = ctx.eng.run_step(ctx.dir, &rs, ctx.ruler, &ctx.judge_model, ctx.judge_timeout, &step.name, Some(ctx.session), true);
        ctx.scratch.res = Some(res);
        ctx.scratch.staged = None;
        ctx.publish();
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "StageSpan"
    }
}

/// A JUDGED step only (bypassed on a skip step): stage the merge so the judges test the MERGED tree.
/// Sets `scratch.staged`.
struct StageMerge;
impl Handler for StageMerge {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let red_file = ctx.cfg.session_isolation.red_file.clone();
        let staged = ctx.session_branch.clone().map(|br| {
            let s = crate::git::stage_session(ctx.dir, &ctx.iso_base, &br, &red_file);
            (br, s)
        });
        ctx.scratch.staged = staged;
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
struct RunJudges;
impl Handler for RunJudges {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        eprintln!("  running judges…");
        ctx.emit(LifecycleEvent::Verify);
        let rs = ctx.run_state();
        let res = ctx.eng.run_step(ctx.dir, &rs, ctx.ruler, &ctx.judge_model, ctx.judge_timeout, &step.name, Some(ctx.session), false);
        // §5.6: judge spend counts against the ceilings — and against the RULER's per-agent tally.
        ctx.tokens_spent += res.judge_tokens;
        if let Some(c) = res.judge_cost {
            ctx.cost_spent += c;
        }
        let ruler_agent = ctx.cfg.judge.agent.clone();
        ctx.charge(&ruler_agent, res.judge_tokens, res.judge_cost);
        eprint!("{}", indent(&ctx.eng.scoreboard()));
        ctx.scratch.res = Some(res);
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

// ── on_gate handlers = the old GATE keep/rollback (HOOK_REDESIGN §4) ───────────────────────────────

/// FIRST on on_gate: a skip-step ceiling halt (nothing staged, no verdicts) stops the run WITHOUT the
/// session-end work — and crucially WITHOUT `emit(Gate)`, so the poison path never publishes a Gate
/// phase (R10). Emits nothing; reads scratch by ref (leaves `res`/`staged` for GateKeepRollback).
struct CeilingPoisonGuard;
impl Handler for CeilingPoisonGuard {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let res = ctx.scratch.res.as_ref().expect("an on_verify handler set scratch.res");
        if ctx.scratch.skip_judges
            && res.halt
            && ctx.scratch.staged.is_none()
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
struct GateKeepRollback;
impl Handler for GateKeepRollback {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        ctx.emit(LifecycleEvent::Gate);
        if ctx.scratch.skip_judges {
            return Ok(Flow::Continue); // a skip step: the span was staged in VERIFY; nothing to gate.
        }
        let mut res = ctx.scratch.res.take().expect("an on_verify handler set scratch.res");
        let staged = ctx.scratch.staged.take();
        let pre_cycle_goals = std::mem::take(&mut ctx.scratch.pre_cycle_goals);
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
                    ctx.span_tip = None;
                    ctx.span_branches.clear();
                } else {
                    rolled_back = true;
                    ctx.eng.restore_goal_state(&pre_cycle_goals);
                    ctx.span_tip = None; // span discarded; next cuts off base
                    ctx.span_branches.clear();
                    eprint!("{}", indent(&ctx.eng.scoreboard()));
                    let rs = ctx.run_state();
                    let recomputed = ctx.eng.conditions_only(&rs);
                    res = CycleResult {
                        stop: recomputed.stop,
                        halt: recomputed.halt,
                        halt_reason: recomputed.halt_reason,
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
                ctx.span_tip = None;
                ctx.span_branches.clear();
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
                    deltas: Vec::new(),
                    fresh_verdicts: Vec::new(),
                    judge_tokens: 0,
                    judge_cost: None,
                };
                ctx.publish();
            }
        }

        ctx.scratch.res = Some(res);
        ctx.scratch.rolled_back = rolled_back;
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "GateKeepRollback"
    }
}

// ── on_session_end handlers = the old GATE tail (HOOK_REDESIGN §4) ─────────────────────────────────

/// The LLM summarizer (best-effort). Feeds `RefineFold` via `scratch.summarized_this_cycle`.
struct Summarize;
impl Handler for Summarize {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !(ctx.cfg.summary.enabled
            && ctx.last_summary.elapsed().as_secs() >= ctx.cfg.summary.min_interval_secs)
        {
            return Ok(Flow::Continue);
        }
        let model = ctx.ruler.default_summary_model().to_string();
        let thoughts = ctx.scratch.outcome.as_ref().map(|o| o.thoughts.clone()).unwrap_or_default();
        let deltas = ctx.scratch.res.as_ref().map(|r| r.deltas.clone()).unwrap_or_default();
        if let Some((s, spend)) =
            summary::summarize(ctx.ruler, &model, &ctx.cumulative, &thoughts, &deltas, 120)
        {
            eprintln!("  [SUMMARY cumulative] {}", s.cumulative);
            eprintln!("  [SUMMARY windowed]   {}", s.windowed);
            ctx.cumulative = s.cumulative.clone();
            ctx.dash.summary_cumulative = s.cumulative;
            ctx.dash.summary_windowed = s.windowed;
            ctx.last_summary = Instant::now();
            ctx.scratch.summarized_this_cycle = true;
            // §5.6: summarizer spend counts too — the summarizer runs on the ruler.
            ctx.tokens_spent += spend.tokens;
            if let Some(c) = spend.cost_usd {
                ctx.cost_spent += c;
            }
            let ruler_agent = ctx.cfg.judge.agent.clone();
            ctx.charge(&ruler_agent, spend.tokens, spend.cost_usd);
            ctx.publish();
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "Summarize"
    }
}

/// Institutional memory: the post-judge refinement fold. Gated on the floor fold (`scratch.mem_folded`)
/// and reads `scratch.rolled_back` + the summary — exactly the old post-judge fold.
struct RefineFold;
impl Handler for RefineFold {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if !(ctx.cfg.memory.enabled && ctx.scratch.mem_folded) {
            return Ok(Flow::Continue);
        }
        let outcome = ctx.scratch.outcome.clone().expect("LaunchWorker set scratch.outcome");
        let deltas = ctx.scratch.res.as_ref().map(|r| r.deltas.clone()).unwrap_or_default();
        let rolled_back = ctx.scratch.rolled_back;
        let summarized_this_cycle = ctx.scratch.summarized_this_cycle;
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
        ctx.last_session = crate::core::memory::last_session_block(&deltas, &scoreboard);
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
struct CheckRunStop;
impl Handler for CheckRunStop {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let res = ctx.scratch.res.take().expect("an on_verify handler set scratch.res");
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

/// Baseline pass (§5.5.1): judge the untouched repo ONCE before session 1 and write `baseline`
/// verdicts, on `on_run_start`. Its two launch-time early exits — `abort_if` already true → Halt,
/// `done_if` already satisfied → GoalsMet — come back as `Flow::Stop`; it finalizes dash + ledger
/// itself (exactly as the old inline pass did) before returning, so the core just propagates the
/// outcome. Verbatim port of the former inline baseline block.
struct Baseline;
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
struct BackgroundSpawn {
    cmds: Vec<String>,
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

/// The lifecycle registry: one ordered handler list per hook point. `default_pipeline` is agg's
/// built-in registration (§5); the ordering here IS the spec (order is outcome-invisible).
#[derive(Default)]
struct Lifecycle {
    on_run_start: Vec<Box<dyn Handler>>,
    background: Vec<Box<dyn Handler>>,
    on_session_start: Vec<Box<dyn Handler>>,
    on_run: Vec<Box<dyn Handler>>,
    on_verify: Vec<Box<dyn Handler>>,
    on_gate: Vec<Box<dyn Handler>>,
    on_session_end: Vec<Box<dyn Handler>>,
}
impl Lifecycle {
    fn default_pipeline(cfg: &AggConfig) -> Self {
        Self::with_hooks(&cfg.hooks)
    }
    /// Split out from `default_pipeline` so the registration ORDER is testable without a full
    /// `AggConfig` (`Hooks: Default`).
    fn with_hooks(hooks: &crate::core::config::Hooks) -> Self {
        let mut l = Lifecycle::default();
        l.on_run_start.push(Box::new(Baseline));
        l.background.push(Box::new(BackgroundSpawn { cmds: hooks.background.clone() }));
        l.on_session_start.push(Box::new(BusDrain));
        l.on_session_start.push(Box::new(PickStep));
        l.on_session_start.push(Box::new(SessionBranchCut));
        l.on_session_start
            .push(Box::new(ShellHook { label: "on_session_start", cmds: hooks.on_session_start.clone() }));
        l.on_session_start.push(Box::new(WriteInstructions));
        l.on_session_start.push(Box::new(ClearMemScratch));
        l.on_run.push(Box::new(LaunchWorker));
        l.on_verify.push(Box::new(FloorFold));
        l.on_verify.push(Box::new(RateLimitBackoff));
        l.on_verify.push(Box::new(GitAutoCommit));
        l.on_verify.push(Box::new(SnapshotGoals));
        l.on_verify.push(Box::new(StageSpan));
        l.on_verify.push(Box::new(StageMerge));
        l.on_verify.push(Box::new(RunJudges));
        l.on_gate.push(Box::new(CeilingPoisonGuard));
        l.on_gate.push(Box::new(GateKeepRollback));
        l.on_session_end
            .push(Box::new(ShellHook { label: "on_session_end", cmds: hooks.on_session_end.clone() }));
        l.on_session_end.push(Box::new(Summarize));
        l.on_session_end.push(Box::new(RefineFold));
        l.on_session_end.push(Box::new(CheckRunStop));
        l
    }
}

/// The engine + parsed sequence, assembled from config. Built once, before the loop (and by
/// `agg plan`).
pub struct Assembly {
    pub engine: Engine,
    pub statements: Vec<Statement>,
}

/// Build the run-set engine + parse the sequence from `cfg` (§5.3/§5.4). Refuses at startup:
/// an unknown step name, an all-`skip_judges` sequence (nothing could ever merge), or a judge name
/// that resolves to no file.
pub fn assemble(cfg: &AggConfig, config_base: &Path) -> Result<Assembly> {
    use crate::core::judges;
    use crate::core::model::{Judge, Lifecycle};

    // the standard library must exist before we resolve names against it (§6.1).
    if let Err(e) = judges::ensure_library() {
        eprintln!("  ⚠ could not refresh ~/.agg/judges: {e}");
    }

    let statements = sequence::parse(&cfg.sequence.steps)?;

    // every referenced step name must be a key in `steps:` (§5.4).
    for st in &statements {
        for name in st.step_names() {
            if !cfg.steps.contains_key(name) {
                let defined: Vec<&str> = cfg.steps.keys().map(String::as_str).collect();
                anyhow::bail!(
                    "sequence references unknown step `{name}` — defined steps: {}",
                    defined.join(", ")
                );
            }
        }
    }
    // an all-`skip_judges` sequence never merges, so `done_if` can never fire (§5.7) — refuse.
    let has_judged = statements
        .iter()
        .flat_map(|s| s.step_names())
        .any(|n| cfg.steps.get(n).map(|b| !b.skip_judges).unwrap_or(false));
    if !has_judged {
        anyhow::bail!(
            "every step in the sequence is skip_judges — nothing can ever merge and done_if can \
             never fire (§5.7). At least one judged step is required."
        );
    }

    // DoD-set = done_if ∪ invariants; run-set = DoD ∪ abort_if ∪ every if-condition (§5.3).
    let mut dod: Vec<String> = stop::judge_names(&cfg.sequence.done_if)?;
    for inv in &cfg.sequence.invariants {
        push_unique(&mut dod, inv);
    }
    let mut run_set = dod.clone();
    if let Some(a) = &cfg.sequence.abort_if {
        for n in stop::judge_names(a)? {
            push_unique(&mut run_set, &n);
        }
    }
    for st in &statements {
        if let Some(c) = st.condition() {
            for n in stop::judge_names(c)? {
                push_unique(&mut run_set, &n);
            }
        }
    }

    // resolve every run-set name to a judge FILE (§5.1) — a name with no file is a startup error.
    let mut judges_vec: Vec<Judge> = Vec::with_capacity(run_set.len());
    for name in &run_set {
        let kind = judges::resolve(name, config_base)?;
        judges_vec.push(Judge {
            name: name.clone(),
            kind,
            invariant: cfg.sequence.invariants.iter().any(|i| i == name),
            in_dod: dod.iter().any(|d| d == name),
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        });
    }

    let engine = Engine::new(judges_vec, cfg.sequence.done_if.clone(), cfg.sequence.abort_if.clone())?;
    Ok(Assembly { engine, statements })
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

fn wait_for_resume(bus: &Bus) -> Option<String> {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        for cmd in bus.drain() {
            match cmd {
                Command::Resume => {
                    eprintln!("  [bus] resume → continuing");
                    return None;
                }
                Command::Stop { reason } => {
                    eprintln!("  [bus] stop while paused → {reason}");
                    return Some(reason);
                }
                other => eprintln!("  [bus] (paused) ignoring {other:?} until resume"),
            }
        }
    }
}

struct StopHooks<'a> {
    cmds: Vec<String>,
    dir: &'a Path,
}
impl Drop for StopHooks<'_> {
    fn drop(&mut self) {
        if !self.cmds.is_empty() {
            crate::hooks::run("on_stop", &self.cmds, self.dir);
        }
    }
}

struct RunPidGuard<'a> {
    dir: &'a Path,
}
impl Drop for RunPidGuard<'_> {
    fn drop(&mut self) {
        crate::os::detach::clear_run_pid(self.dir);
    }
}

/// Everything one step of the loop reads and writes.
struct LoopState<'a> {
    cfg: &'a AggConfig,
    /// the RULER — LLM judges + summarizer. Immutable across the run (§4).
    ruler: &'static dyn AgentBackend,
    /// the ruler model (`judge.model`, resolved).
    judge_model: String,
    /// EVERY judge's timeout (`judge.timeout`).
    judge_timeout: u64,
    dir: &'a Path,
    config_base: &'a Path,
    /// `prompt_includes` fragments, composed once at launch.
    prompt_prefix: String,

    eng: Engine,
    /// the sequence cursor — yields the next step name each cycle.
    cursor: Cursor,
    /// the step being run THIS cycle (set by INJECT).
    cur_step: Option<ResolvedStep>,

    dash: DashboardState,
    live: LiveState,
    ledger: crate::project::RunLedger,
    bus: Option<Bus>,

    budget_total: Option<u64>,
    cost_limit: Option<f64>,
    max_iter: Option<u32>,
    max_sessions: u32,
    gate_regressions: bool,

    loop_start: Instant,
    lifetime_base: u32,

    session: u32,
    tokens_spent: u64,
    cost_spent: f64,
    /// per-agent token + cost tally (§7.4), attributed at each spend site (worker / ruler judges /
    /// summarizer). Sums to `tokens_spent`/`cost_spent`; makes a mixed run's totals interpretable.
    per_agent: std::collections::BTreeMap<String, crate::state::AgentUsage>,

    pending_instruction: Option<String>,
    last_session: String,
    cumulative: String,
    last_summary: Instant,
    dud_streak: u32,

    // ── per-session git isolation + spans ──
    iso_base: String,
    /// this session's branch (cut by INJECT, resolved by GATE).
    session_branch: Option<String>,
    /// the branch the NEXT session cuts off — the TIP of the staged span, or `None` = base (§5.7).
    span_tip: Option<String>,
    /// staged span branches accumulated by `skip_judges` steps (for reporting on abort).
    span_branches: Vec<String>,
    /// content of the step's state file at session start, to warn if the agent never touched it.
    state_before: Option<String>,
    /// per-session channel between stage-handlers; reset each session at the loop top (§8).
    scratch: Scratch,
}

impl LoopState<'_> {
    fn emit(&mut self, event: LifecycleEvent) {
        self.dash.phase = event.phase();
        if let LifecycleEvent::Finished { reason, ledger_tag } = &event {
            self.dash.finished = true;
            self.dash.finish_reason = reason.clone();
            let (gm, gt) = self.eng.tally();
            self.ledger.update(self.session, self.tokens_spent, gm, gt);
            self.ledger.finish(now_epoch(), ledger_tag);
        }
        self.publish();
    }

    /// Attribute one spend to an agent's running tally (§7.4). A `None` cost is an agent that cannot
    /// report a price — it never fabricates a `0`, so that agent's cost stays `None` (rendered "—")
    /// until a real price arrives, then it accumulates only the reported part.
    fn charge(&mut self, agent: &str, tokens: u64, cost: Option<f64>) {
        let e = self.per_agent.entry(agent.to_string()).or_default();
        e.tokens += tokens;
        if let Some(c) = cost {
            e.cost = Some(e.cost.unwrap_or(0.0) + c);
        }
    }

    fn publish(&mut self) {
        self.dash.up_secs = self.loop_start.elapsed().as_secs();
        self.dash.tokens_spent = self.tokens_spent;
        self.dash.cost_spent = self.cost_spent;
        self.dash.per_agent = self.per_agent.clone();
        // Surface the current step + its agent/model so a mixed run is interpretable from state.json
        // (§7.4). Pure display copy — never touches control flow or accounting.
        if let Some(cs) = &self.cur_step {
            self.dash.step = cs.name.clone();
            self.dash.step_agent = cs.agent.clone();
            // the RESOLVED model (step override, else the agent's default) — what actually ran.
            self.dash.step_model = cs.backend().map(|b| cs.model(b).to_string()).unwrap_or_default();
        }
        let (m, t) = self.eng.tally();
        self.dash.goals_met = m;
        self.dash.goals_total = t;
        self.dash.goals = DashboardState::goals_from_engine(&self.eng, &self.dash.goals);
        self.dash.judges = DashboardState::judges_from_engine(&self.eng, &self.dash.judges);
        let snapshot = self.dash.clone();
        self.live.update(|s| {
            let now = std::mem::take(&mut s.now);
            let think = std::mem::take(&mut s.think);
            let recent = std::mem::take(&mut s.recent);
            let idle_secs = s.idle_secs;
            let seq = s.seq;
            *s = snapshot;
            s.now = now;
            s.think = think;
            s.recent = recent;
            s.idle_secs = idle_secs;
            s.seq = seq;
        });
    }

    fn run_state(&self) -> RunState {
        RunState {
            tokens_spent: self.tokens_spent,
            budget_total: self.budget_total,
            cost_spent: self.cost_spent,
            cost_limit: self.cost_limit,
            sessions_done: self.session,
            max_sessions: self.max_iter,
            wall_hours: self.loop_start.elapsed().as_secs_f64() / 3600.0,
        }
    }

    fn over_max_sessions(&mut self) -> Option<RunOutcome> {
        if self.max_sessions == 0 || self.session < self.max_sessions {
            return None;
        }
        let max_sessions = self.max_sessions;
        eprintln!("→ reached max_sessions={max_sessions}; stopping (DoD not met).");
        let (gm, gt) = self.eng.tally();
        self.emit(LifecycleEvent::Finished {
            reason: format!("reached max_sessions={max_sessions} ({gm}/{gt} goals met)"),
            ledger_tag: "max-sessions".into(),
        });
        Some(RunOutcome::MaxSessions)
    }

    fn stopped_via_bus(&mut self, reason: String) -> RunOutcome {
        eprintln!("  [bus] stop → {reason}");
        self.emit(LifecycleEvent::Finished {
            reason: format!("stopped via bus: {reason}"),
            ledger_tag: "stopped".into(),
        });
        RunOutcome::Stopped
    }

    /// The branch a JUDGED step's regression check reads / the branch cut. Base is the resolved
    /// isolation base.
    fn base_ref(&self) -> &str {
        self.span_tip.as_deref().unwrap_or(&self.iso_base)
    }

    /// Compose the worker's whole brief into `agg/state/INSTRUCTIONS.md`, then return the tiny
    /// fixed pointer that becomes the actual `-p` value (§2/§3). The order is highest-priority
    /// first — operator steering, then the task (role framing + the step's `prompt:`), then the
    /// context pointers/excerpts (memory tail → STATE pointer → AGG.md pointer → wiki), then the
    /// standing footer. Long files are POINTED at (STATE, AGG.md, LOG's older history) or excerpted
    /// (LOG's recent tail via `read_block`) so agg keeps the context budget bounded even though the
    /// worker can open the full files itself.
    ///
    /// If the file write fails (rare best-effort disk error), fall back to returning the composed
    /// content directly so the session still runs, arg-safe against a leading dash.
    fn compose_prompt(&mut self, step: &ResolvedStep) -> String {
        let mut s = String::new();
        s.push_str(
            "<!-- agg/state/INSTRUCTIONS.md — WRITTEN BY agg, REGENERATED every session. Do not edit; it is overwritten. -->\n\n",
        );
        let agent = &step.agent;
        s.push_str(&format!(
            "# Session {} · step `{}` · agent `{agent}`\n",
            self.session, step.name
        ));

        // ── operator steering — highest priority, act on it FIRST. The banner keeps the phrase
        //    "HIGH-PRIORITY OPERATOR INSTRUCTION" so the memory sanitizer (`looks_like_marker`) still
        //    de-fangs a worker note that tries to forge it. ──
        if let Some(instr) = self.pending_instruction.take() {
            s.push_str(&format!(
                "\n## ⚠ HIGH-PRIORITY OPERATOR INSTRUCTION — do this FIRST (it overrides the default plan)\n{instr}\n"
            ));
        }
        if let Some(status) = crate::os::spawns::summary_for_prompt(self.dir) {
            s.push_str(&format!("\n{status}\n"));
        }

        // ── the task: the step's ROLE framing (config-driven, §4) + its specific `prompt:` ──
        if let Some(rp) = &step.role_prompt {
            if !rp.trim().is_empty() {
                s.push_str(&format!("\n## Your role this session\n{}\n", rp.trim()));
            }
        }
        if let Some(p) = &step.prompt {
            if !p.trim().is_empty() {
                s.push_str(&format!("\n## This session — do ONE focused chunk\n{}\n", p.trim()));
            }
        }
        if !self.prompt_prefix.is_empty() {
            s.push_str(&format!("\n{}\n", self.prompt_prefix.trim()));
        }

        // ── context: memory recent-tail excerpt + a conditional pointer to the full LOG ──
        if self.cfg.memory.enabled {
            let mem = crate::core::memory::read_block(self.dir, &self.last_session, self.cfg.memory.inject_kb);
            if !mem.trim().is_empty() {
                s.push_str(&format!("\n## What's been tried\n{}\n", mem.trim()));
                s.push_str(
                    "Full history in `agg/state/LOG.md` — read it ONLY if you need older detail; it is long, don't load it all.\n",
                );
            }
        }

        // ── STATE → a POINTER, not an excerpt (it is crisp by design; read the whole small file) ──
        if let Ok(st) = std::fs::read_to_string(self.config_base.join(&step.state)) {
            if !st.trim().is_empty() {
                s.push_str(&format!(
                    "\n## Where things stand\nRead `agg/{}` — your predecessor's forward advice (kept short; read it in full).\n",
                    step.state
                ));
            }
        }

        // ── AGG.md → a POINTER (the standing project instructions; scope/goals/architecture/rules,
        //    the CLAUDE.md-analog for the agg loop) ──
        if crate::paths::config_base(self.dir).join("AGG.md").exists() {
            s.push_str("\n## Project instructions\nRead `agg/AGG.md` — the standing scope, architecture, and rules for this project.\n");
        }

        // ── the LLM wiki — list its pages if any exist (the footer names it regardless, since a
        //    multi-session PLAN belongs there) ──
        let wiki = crate::paths::wiki_dir(self.dir);
        if wiki.exists() {
            let pages = wiki_pages(&wiki);
            if !pages.is_empty() {
                s.push_str(&format!(
                    "\n## Knowledge base\nConsult and maintain the durable wiki at `agg/state/wiki/` (start with {}).\n",
                    pages.join(", ")
                ));
            }
        }

        // ── standing footer (from plugin/scaffold/exit_footer.md; no git tutorial — agg owns git,
        //    §3 remark 3). The STATE path is filled from `step.state` (NOT hardcoded) so an overridden
        //    `state:` still names the file agg actually reads/points-at. The wiki/OKF guidance is
        //    SELF-CONTAINED (rules + a concrete template) so the worker needn't know "OKF" — a
        //    June-2026 spec many models predate. ──
        s.push('\n');
        s.push_str(&EXIT_FOOTER.replace("{{STATE}}", &step.state));

        // write the composed brief to disk; the worker's actual `-p` is the tiny pointer.
        let path = crate::paths::instructions_md(self.dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &s) {
            Ok(()) => INSTRUCTIONS_POINTER.to_string(),
            Err(e) => {
                // degraded mode: could not write the brief — pass it inline so the session still
                // runs. Guard a leading dash so no backend's arg-parser eats it as a flag.
                eprintln!("  ⚠ could not write {} ({e}); passing the brief inline this session", path.display());
                if s.starts_with('-') { format!("\n{s}") } else { s }
            }
        }
    }

    fn worker_is_broken(&self) -> Option<anyhow::Error> {
        const LIMIT: u32 = 3;
        (self.dud_streak >= LIMIT).then(|| {
            let agent = self.cur_step.as_ref().map(|s| s.agent.as_str()).unwrap_or("worker");
            anyhow::anyhow!(
                "the `{agent}` worker failed to start {LIMIT} times in a row — non-zero exit, ZERO \
                 tokens, every time.\n\
                 That means the agent CLI rejected the invocation itself; it never reached the model. \
                 Retrying cannot help: each session builds the same command.\n\
                 Run `agg doctor`, and try the worker by hand to see the CLI's own error."
            )
        })
    }

    fn finish_interrupted(&mut self) -> RunOutcome {
        eprintln!("\n⚠ interrupted (SIGINT/SIGTERM) — stopping after the current session; worker killed, base untouched.");
        self.emit(LifecycleEvent::Finished {
            reason: "interrupted (SIGINT/SIGTERM)".into(),
            ledger_tag: "interrupted".into(),
        });
        RunOutcome::Stopped
    }

    fn abort_now(&mut self, reason: &str) -> RunOutcome {
        eprintln!("\n⚠ {reason}");
        self.emit(LifecycleEvent::Finished {
            reason: reason.to_string(),
            ledger_tag: format!("abort:{reason}"),
        });
        RunOutcome::Halt
    }

    /// The early ENFORCED memory floor (so the session's facts survive a later panic).
    fn fold_memory_floor(&mut self, outcome: &SessionOutcome) -> bool {
        if self.cfg.memory.enabled && !outcome.rate_limited {
            let scoreboard_now = self.eng.scoreboard();
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
            self.dash.memory_bytes = crate::core::memory::append_entry(
                self.dir,
                self.session,
                "session-start",
                &body,
                self.cfg.memory.max_kb,
            );
            self.publish();
            true
        } else {
            false
        }
    }

    /// On abort with a span still staged, leave the branches and print them (§5.7).
    fn report_stranded_span(&self) {
        if !self.span_branches.is_empty() {
            eprintln!(
                "  [span] {} staged branch(es) left un-merged for inspection: {}",
                self.span_branches.len(),
                self.span_branches.join(", ")
            );
        }
    }
}

pub fn run(
    cfg: AggConfig,
    assembly: Assembly,
    dir: &Path,
    config_base: &Path,
    max_sessions_flag: u32,
) -> Result<RunOutcome> {
    let Assembly { engine: eng, statements } = assembly;

    // ── double-run guard ──
    if let Some(pid) = crate::os::detach::live_pid(dir) {
        if pid != std::process::id() {
            anyhow::bail!(
                "a loop is already running in this project (pid {pid}).\n  \
                 watch it:   agg dashboard\n  stop it:    agg stop\n  \
                 (if you're sure it's dead, remove agg/state/run.pid and retry.)"
            );
        }
    }
    crate::os::detach::write_run_pid(dir);
    let _run_pid_guard = RunPidGuard { dir };
    crate::os::signals::install();

    let ruler = cfg.ruler_backend()?;
    let judge_model = cfg.judge_model(ruler);
    let judge_timeout = cfg.judge.timeout;

    // ── session isolation (MANDATORY) ──
    let iso = &cfg.session_isolation;
    if crate::git::is_repo(dir) {
        crate::git::recover_stranded_merge(dir, &iso.branch_prefix);
    }
    if !crate::git::is_repo(dir) {
        anyhow::bail!(
            "session isolation is mandatory, but this is not a git repository.\n  \
             fix:  git init && git add -A && git commit -m 'agg baseline'"
        );
    }
    if !crate::git::is_clean(dir) {
        anyhow::bail!(
            "session isolation is mandatory, but the work tree has uncommitted tracked changes.\n  \
             fix:  commit or stash your changes first  (git status shows them)"
        );
    }
    crate::git::ensure_agg_gitignored(dir);
    let iso_base: String = if iso.base_branch.is_empty() {
        match crate::git::current_branch(dir) {
            Some(b) => b,
            None => anyhow::bail!(
                "session isolation is mandatory, but HEAD is detached.\n  \
                 fix:  git switch -c <branch>"
            ),
        }
    } else {
        iso.base_branch.clone()
    };
    eprintln!("  [iso] per-session branch isolation ON — base branch '{iso_base}'");

    #[cfg(not(unix))]
    eprintln!("  ⚠ Windows: unix-first build — the CPU-flat watchdog and process-group spawn protection are NOT active here.");

    crate::hooks::run("on_start", &cfg.hooks.on_start, dir);
    let _stop_hooks = StopHooks { cmds: cfg.hooks.on_stop.clone(), dir };
    let prompt_prefix = crate::hooks::gather_prompt_includes(&cfg.prompt_includes, dir);

    let loop_start = Instant::now();
    let (m, t) = eng.tally();
    eprintln!(
        "════════════════════════════════════════════════════════════\n\
         AgenticGoGo — project {}\n\
         goals {m}/{t}  done_if: {}\n\
         ════════════════════════════════════════════════════════════\n\
         ▶ watch live:  run `agg dashboard` in another terminal\n\
         ⏹ stop anytime: `agg stop`",
        cfg.project, eng.done_if
    );

    // max_sessions: the CLI flag WINS when passed (§4.1), else the config key. The flag keeps its
    // 0=unlimited convention (clap default); `limits.sessions` is None=unlimited — map None→0 so the
    // loop's internal 0-sentinel (over_max_sessions, max_iter) is unchanged.
    let max_sessions =
        if max_sessions_flag > 0 { max_sessions_flag } else { cfg.sequence.limits.sessions.unwrap_or(0) };

    let worker_model_display = cfg
        .defaults
        .model
        .clone()
        .unwrap_or_else(|| cfg.worker_backend().map(|b| b.default_model().to_string()).unwrap_or_default());
    let dash = DashboardState {
        project: cfg.project.clone(),
        model: worker_model_display,
        stop_when: eng.done_if.clone(),
        halt_when: eng.abort_if.clone().unwrap_or_default(),
        budget_total: cfg.sequence.limits.tokens,
        cost_limit: cfg.sequence.limits.cost,
        phase: Phase::Starting,
        ..Default::default()
    };
    let live = LiveState::new(dir, loop_start, dash.clone());

    let ledger = crate::project::RunLedger::begin(dir, &cfg.project, std::process::id(), now_epoch());
    let lifetime_base = ledger.prior_lifetime_sessions();

    let mut st = LoopState {
        cfg: &cfg,
        ruler,
        judge_model,
        judge_timeout,
        dir,
        config_base,
        prompt_prefix,
        eng,
        cursor: Cursor::new(statements),
        cur_step: None,
        dash,
        live,
        ledger,
        bus: None,
        budget_total: cfg.sequence.limits.tokens,
        cost_limit: cfg.sequence.limits.cost,
        max_iter: if max_sessions == 0 { None } else { Some(max_sessions) },
        max_sessions,
        gate_regressions: cfg.sequence.gate_regressions,
        loop_start,
        lifetime_base,
        session: 0,
        tokens_spent: 0,
        cost_spent: 0.0,
        per_agent: std::collections::BTreeMap::new(),
        pending_instruction: None,
        last_session: String::new(),
        dud_streak: 0,
        cumulative: String::new(),
        last_summary: loop_start,
        iso_base,
        session_branch: None,
        span_tip: None,
        span_branches: Vec::new(),
        state_before: None,
        scratch: Scratch::default(),
    };
    st.publish();
    st.dash.lifetime_session = lifetime_base;

    // agg's built-in hook registration (HOOK_REDESIGN §5). Owned here, passed alongside `st`.
    let lifecycle = Lifecycle::default_pipeline(&cfg);

    // ── run-start hooks: spawn the user's background watchers, then the baseline pass (§5.5.1) —
    //    both are registry handlers now (the `background` / `on_run_start` hooks). Baseline's two
    //    launch-time early exits come back as End::Stop and finish the run. ──
    run_hook(&lifecycle.background, &mut st)?;
    if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_run_start, &mut st)? {
        return Ok(outcome);
    }

    st.last_summary = Instant::now() - Duration::from_secs(cfg.summary.min_interval_secs);
    if cfg.memory.enabled {
        crate::core::memory::ensure_scratch_dir(dir);
        crate::core::memory::sweep_scratch(dir);
    }
    st.bus = Bus::open(dir).ok();

    // ── the deterministic outer loop, one step at a time ──
    loop {
        if let Some(outcome) = st.over_max_sessions() {
            return Ok(outcome);
        }
        // reset the per-session channel so no field (esp. `prompt`) leaks across sessions.
        st.scratch = Scratch::default();
        st.emit(LifecycleEvent::Inject);
        match run_hook(&lifecycle.on_session_start, &mut st)? {
            Some(End::Stop(outcome)) => return Ok(outcome),
            Some(End::NextSession) => continue,
            None => {}
        }
        st.emit(LifecycleEvent::Run);
        match run_hook(&lifecycle.on_run, &mut st)? {
            Some(End::Stop(outcome)) => return Ok(outcome), // SIGINT → finish_interrupted → Stopped
            Some(End::NextSession) => continue,
            None => {}
        }
        if let Some(e) = st.worker_is_broken() {
            return Err(e);
        }
        match run_hook(&lifecycle.on_verify, &mut st)? {
            Some(End::NextSession) => continue, // rate-limited: incomplete session — go round again
            Some(End::Stop(outcome)) => return Ok(outcome), // ceiling tripped during backoff → Halt
            None => {}
        }
        // GATE keep/rollback → poison-pill Halt short-circuits here (CeilingPoisonGuard).
        if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_gate, &mut st)? {
            return Ok(outcome);
        }
        // session-end work (shell hook, summary, memory fold) then the run-stop check (CheckRunStop).
        if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_session_end, &mut st)? {
            return Ok(outcome);
        }
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}

/// The wiki's page names (up to a handful), sorted, for the INSTRUCTIONS "start with …" hint. A
/// pure listing — an empty/absent dir yields no names and the hint is dropped. ponytail: caps at 5
/// so a large wiki can't bloat the pointer; the worker sees the rest by opening the dir.
fn wiki_pages(wiki: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(wiki) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".md").then(|| format!("`wiki/{n}`"))
        })
        .collect();
    names.sort();
    names.truncate(5);
    names
}

use crate::util::now_epoch;

#[cfg(test)]
mod tests {
    use super::*;

    /// The pointer that becomes the worker's actual `-p` must be short and dash-free — the whole
    /// point of moving the brief into `INSTRUCTIONS.md` (§2) is that the argv value can never again
    /// hit the size ceiling or be parsed as a flag.
    #[test]
    fn the_instructions_pointer_is_tiny_and_arg_safe() {
        assert!(!INSTRUCTIONS_POINTER.starts_with('-'), "pointer must never look like a flag");
        assert!(INSTRUCTIONS_POINTER.len() < 200, "pointer must stay tiny (no argv ceiling)");
        assert!(INSTRUCTIONS_POINTER.contains("agg/state/INSTRUCTIONS.md"), "pointer names the brief file");
    }

    /// `wiki_pages` lists only `.md` files, sorted, capped, and formatted as `` `wiki/<name>` ``;
    /// an absent dir yields nothing (so the KB hint is dropped).
    #[test]
    fn wiki_pages_lists_markdown_only() {
        let d = std::env::temp_dir().join(format!("agg-wiki-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(wiki_pages(&d).is_empty(), "absent dir → no pages");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("parser.md"), "x").unwrap();
        std::fs::write(d.join("dead-ends.md"), "x").unwrap();
        std::fs::write(d.join("notes.txt"), "x").unwrap(); // not markdown → excluded
        let pages = wiki_pages(&d);
        assert_eq!(pages, vec!["`wiki/dead-ends.md`".to_string(), "`wiki/parser.md`".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// R3 (HOOK_STAGE_PLAN): `on_session_start` MUST dispatch in exactly this order — the user shell
    /// hook BEFORE `WriteInstructions` (so a hook sees the PREVIOUS session's brief), and
    /// `BusDrain`→`PickStep` before the branch cut / compose. This order is outcome-invisible (the
    /// e2e asserts outcomes, not order), so pin it here where a reorder would otherwise pass the gate.
    #[test]
    fn on_session_start_registration_order_is_the_spec() {
        let l = Lifecycle::with_hooks(&crate::core::config::Hooks::default());
        let names: Vec<&str> = l.on_session_start.iter().map(|h| h.name()).collect();
        assert_eq!(
            names,
            [
                "BusDrain",
                "PickStep",
                "SessionBranchCut",
                "on_session_start",
                "WriteInstructions",
                "ClearMemScratch",
            ]
        );
        // on_run is a SINGLE handler (the unknown-agent + SIGINT early returns forbid splitting).
        let run_names: Vec<&str> = l.on_run.iter().map(|h| h.name()).collect();
        assert_eq!(run_names, ["LaunchWorker"]);
    }
}
