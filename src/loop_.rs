//! The harness loop: `agg run`.
//!
//! # A deterministic outer loop around a stochastic inner loop
//!
//! agg is the OUTER loop. It is plain Rust, and its control flow is DETERMINISTIC: no model
//! decides what happens next — given the same state and the same verdicts, the same code path
//! always runs. Four stages per cycle:
//!
//! ```text
//!   INJECT  state + steering → the worker's prompt (resume prompt + AGG_MEMORY.md + bus commands)
//!   RUN     the fresh `claude -p` worker — the ONE stochastic step, an opaque black box
//!   VERIFY  agg runs the judges itself, externally, against the filesystem
//!   GATE    keep or roll back the merge · check stop/halt · carry state forward → repeat
//! ```
//!
//! Those four stages are [`LoopState::inject`], [`LoopState::run`], [`LoopState::verify`] and
//! [`LoopState::gate`] — one method each, in that order, and the body of [`run`] is little more
//! than the four calls.
//!
//! The INNER loop is whatever the worker does inside RUN — plan, act, observe, reason; ReAct,
//! NVIDIA's Context–Observe–Reason–Act, a DISCOVER→PLAN→EXECUTE cycle — it is STOCHASTIC and agg
//! neither sees nor cares. "Keep the LLM out of the loop" means exactly this: the LLM lives inside
//! RUN only; INJECT/VERIFY/GATE are code.
//!
//! Why this split is the whole thesis: a deterministic outer loop is only trustworthy if VERIFY is
//! deterministic too — judges that execute against the filesystem, never the agent grading its own
//! homework. The determinism of the GATE is what makes it safe to trust a stochastic worker.

use crate::bus::{Bus, Command};
use crate::config::AggConfig;
use crate::engine::{CycleResult, Engine, GoalRuntime, RunState};
use crate::state::{DashboardState, LiveState, Phase};
use crate::summary;
use crate::worker::{self, SessionOutcome};
use anyhow::Result;
use std::path::Path;
use std::time::{Duration, Instant};

/// How the loop ended — mapped to a process exit code in `main` so automation can branch on the
/// outcome (`agg run && deploy` must NOT proceed after a HALT). See [`RunOutcome::exit_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The success stop condition was met (or was already satisfied at launch).
    GoalsMet,
    /// A guard fired (invariant regressed, budget/cost/iteration/wall ceiling) — NOT success.
    Halt,
    /// The `--max-sessions` cap was reached with goals not all met.
    MaxSessions,
    /// The operator stopped the run (`agg stop`, incl. while paused).
    Stopped,
}

impl RunOutcome {
    /// The process exit code. 0 = success; non-zero = "did not reach the goal", with distinct
    /// codes so scripts can tell WHY. Avoids clap's usage-error code 2. Hard errors (the `?`
    /// paths) are surfaced by `main` as 1, separately.
    pub fn exit_code(self) -> u8 {
        match self {
            RunOutcome::GoalsMet => 0,
            RunOutcome::Stopped => 0, // an operator stop is a clean, intended end
            RunOutcome::Halt => 3,
            RunOutcome::MaxSessions => 4,
        }
    }
}

/// What INJECT produced: the prompt for this session, or a graceful end (the operator stopped the
/// run over the bus — possibly while it was paused — before any worker was launched).
enum Injected {
    Prompt(String),
    Stop(RunOutcome),
}

/// What VERIFY produced: the judged cycle, plus everything GATE needs to keep or undo it.
struct Verified {
    /// the judges' verdicts folded into stop/halt + this cycle's goal deltas. GATE may REPLACE
    /// this wholesale when it rolls the merge back.
    res: CycleResult,
    /// `Some` only on the rollback-gate path: the session branch + whether its merge is staged.
    staged: Option<(String, crate::git::StagedSession)>,
    /// pre-cycle (base) goal truth, taken BEFORE the judges ran, so a rollback can restore it (W5).
    pre_cycle_goals: Vec<GoalRuntime>,
    /// the enforced early memory fold happened, so GATE's post-judge fold may supersede it.
    mem_folded: bool,
}

/// What GATE decided: go round again, or end the run with this outcome.
enum GateDecision {
    Loop,
    Stop(RunOutcome),
}

/// Block until a `resume` or `stop` command arrives on the bus (poll every 2s).
/// Returns `None` on resume, `Some(reason)` if a `stop` arrived while paused — the caller then
/// takes its normal graceful-stop path so the Drop guards (on_stop hooks, run.pid, ledger) run.
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

/// Runs `on_stop` hooks exactly once, on ANY exit from the loop (success, halt, bus-stop,
/// max-sessions, or an early `?` error) — via Drop, so we don't have to thread it through
/// every return site.
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

/// Clears `.agg/run.pid` on any loop exit (clean, error, or panic) so a later `agg stop`
/// never targets a dead pid and the double-run guard never falsely reports a live loop.
struct RunPidGuard<'a> {
    dir: &'a Path,
}
impl Drop for RunPidGuard<'_> {
    fn drop(&mut self) {
        crate::detach::clear_run_pid(self.dir);
    }
}

/// Everything one cycle of the loop reads and writes.
///
/// The four stages all mutate the same handful of values — counters, ceilings, the carry-forward
/// strings, the engine, the dashboard/ledger handles. Threading those as a dozen locals is what
/// made `run()` a single 700-line function; owning them here lets each stage be a method that
/// takes `&mut self` instead of twelve parameters.
///
/// **Borrow discipline** — the one rule this shape rests on: never write an expression that both
/// `&mut`-borrows a field and `&`-borrows `self` through a method. Bind first, then call:
///
/// ```ignore
/// let rs = self.run_state();                              // &self released here
/// let res = self.eng.evaluate_cycle(dir, cb, &rs);        // &mut self.eng only
/// ```
///
/// [`LoopState::publish`] applies the same rule: it clones `dash` into a local before handing it
/// to `live.update`, so `&self.dash` and `&self.live` are never live at once.
struct LoopState<'a> {
    // ── borrowed inputs (one shared lifetime; all read-only) ──
    cfg: &'a AggConfig,
    dir: &'a Path,
    config_base: &'a Path,
    /// read once at launch, re-used to build every prompt
    resume_prompt: String,
    /// `prompt_includes` fragments, composed once at launch
    prompt_prefix: String,

    // ── owned engine + handles ──
    eng: Engine,
    /// the loop's working copy of the dashboard fields IT owns; `publish()` folds them into `live`
    dash: DashboardState,
    live: LiveState,
    ledger: crate::project::RunLedger,
    bus: Option<Bus>,

    // ── ceilings (`budget_total` is steerable mid-run via the bus) ──
    budget_total: Option<u64>,
    cost_limit: Option<f64>,
    /// `None` when `max_sessions == 0` (unlimited), so `over_iterations` never trips
    max_iter: Option<u32>,
    max_sessions: u32,

    // ── clock + lifetime base ──
    loop_start: Instant,
    lifetime_base: u32,

    // ── counters ──
    session: u32,
    tokens_spent: u64,
    cost_spent: f64,

    // ── carry-forward (written in one cycle, read in the next) ──
    pending_instruction: Option<String>,
    last_session: String,
    last_session_id: Option<String>,
    cumulative: String,
    last_summary: Instant,

    // ── per-session git isolation ──
    iso_base: Option<String>,
    /// cut by INJECT, resolved by VERIFY
    session_branch: Option<String>,
}

impl LoopState<'_> {
    /// Publish ALL loop-owned dashboard fields to the shared `LiveState`.
    ///
    /// Single-writer-under-lock: ONE shared `LiveState` is mutated by both the loop (boundary
    /// updates: phase/session/goals/summaries) and the worker's reader thread (live updates:
    /// now/think/recent/idle, mid-session). We assign `dash` wholesale and preserve only the
    /// worker-owned live fields, so a NEW loop-owned field is published automatically instead of
    /// being silently dropped (the old hand-copied list had already lost `lifetime_session`, so
    /// `agg status`/the dashboard showed "#0 lifetime" — that class of bug is now structurally
    /// impossible).
    ///
    /// What it publishes depends on WHERE it is called: it reads `tokens_spent`/`cost_spent`/`eng`
    /// live. Moving a `publish()` across a mutation changes what lands in `state.json`.
    fn publish(&mut self) {
        self.dash.up_secs = self.loop_start.elapsed().as_secs();
        self.dash.tokens_spent = self.tokens_spent;
        self.dash.cost_spent = self.cost_spent;
        let (m, t) = self.eng.tally();
        self.dash.goals_met = m;
        self.dash.goals_total = t;
        self.dash.goals = DashboardState::goals_from_engine(&self.eng, &self.dash.goals);
        // bind the snapshot first: the closure must not hold `&self.dash` while `self.live` is
        // borrowed (see the borrow discipline on the struct).
        let snapshot = self.dash.clone();
        self.live.update(|s| {
            let now = std::mem::take(&mut s.now);
            let think = std::mem::take(&mut s.think);
            let recent = std::mem::take(&mut s.recent);
            let idle_secs = s.idle_secs;
            let seq = s.seq; // monotonic; `publish()` bumps it — don't reset from stale `dash`
            *s = snapshot;
            s.now = now;
            s.think = think;
            s.recent = recent;
            s.idle_secs = idle_secs;
            s.seq = seq;
        });
    }

    /// The run-level accounting the engine's ceiling guards evaluate against. Returns an OWNED
    /// `RunState` so it never holds a borrow of `self` past the binding.
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

    /// The `--max-sessions` cap, measured on the PRE-increment session count at the top of every
    /// cycle (before INJECT bumps it). `Some` = the cap is reached and the run is over.
    fn over_max_sessions(&mut self) -> Option<RunOutcome> {
        if self.max_sessions == 0 || self.session < self.max_sessions {
            return None;
        }
        let max_sessions = self.max_sessions;
        eprintln!("→ reached max_sessions={max_sessions}; stopping (goals not all met).");
        // set the finished state + publish so `agg status`/the dashboard reflect the outcome
        // (previously this path broke without updating either — a never-"finished" run).
        let (gm, gt) = self.eng.tally();
        self.dash.phase = Phase::Done;
        self.dash.finished = true;
        self.dash.finish_reason = format!("reached max_sessions={max_sessions} ({gm}/{gt} goals met)");
        self.ledger.update(self.session, self.tokens_spent, gm, gt);
        self.ledger.finish(now_epoch(), "max-sessions");
        self.publish();
        Some(RunOutcome::MaxSessions)
    }

    /// The one graceful bus-stop path, shared by `stop` and `stop`-while-`pause`d. Returns through
    /// the caller so the Drop guards (on_stop hooks, run.pid cleanup, ledger finalize) all run —
    /// this used to be a `std::process::exit(0)`, which skipped every one of them.
    fn stopped_via_bus(&mut self, reason: String) -> RunOutcome {
        eprintln!("  [bus] stop → {reason}");
        self.dash.phase = Phase::Done;
        self.dash.finished = true;
        self.dash.finish_reason = format!("stopped via bus: {reason}");
        let (gm, gt) = self.eng.tally();
        self.ledger.update(self.session, self.tokens_spent, gm, gt);
        self.ledger.finish(now_epoch(), "stopped");
        self.publish();
        RunOutcome::Stopped
    }

    /// **INJECT** — state + steering → the worker's prompt.
    ///
    /// Drains the bus (the only safe injection point for a headless worker), opens the session,
    /// cuts its isolation branch, and layers the prompt: operator instruction, spawn status,
    /// `prompt_includes`, resume prompt, then the durable memory block as the lowest-priority
    /// tail. Nothing here calls a model.
    fn inject(&mut self) -> Injected {
        // Publish the stage BEFORE the drain: a `pause` blocks in here, and a paused loop that
        // still reads "verify" would be lying about where it is waiting.
        self.dash.phase = Phase::Inject;
        self.publish();

        // ── drain the bus at the session boundary; apply steering commands ──
        // `drain()` hands back an owned Vec, so the `&self.bus` borrow ends here and the arms
        // below are free to take `&mut self` (publish/ledger).
        let cmds = match &self.bus {
            Some(bus) => bus.drain(),
            None => Vec::new(),
        };
        for cmd in cmds {
            match cmd {
                Command::InjectInstruction { text } => {
                    eprintln!("  [bus] inject-instruction → prepended to next session");
                    self.pending_instruction = Some(match self.pending_instruction.take() {
                        Some(prev) => format!("{prev}\n\n{text}"),
                        None => text,
                    });
                }
                Command::SetBudget { total } => {
                    eprintln!("  [bus] set-budget → {:?}", total);
                    self.budget_total = total;
                }
                Command::Pause => {
                    eprintln!("  [bus] pause → waiting for resume/stop…");
                    // bind first: `wait_for_resume` borrows `self.bus`, `stopped_via_bus` needs
                    // `&mut self`. A `None` here means "resumed" — fall through and keep draining.
                    let stopped = match &self.bus {
                        Some(bus) => wait_for_resume(bus),
                        None => None,
                    };
                    if let Some(reason) = stopped {
                        return Injected::Stop(self.stopped_via_bus(reason));
                    }
                }
                Command::Resume => { /* a stray resume with no pause: ignore */ }
                Command::Stop { reason } => return Injected::Stop(self.stopped_via_bus(reason)),
                Command::Note { text } => eprintln!("  [bus] note: {text}"),
            }
        }

        self.session += 1;
        self.dash.session = self.session;
        // bump + persist this run's record so a later `agg run` continues the count
        // and the dashboard's lifetime total stays live across restarts.
        self.dash.lifetime_session = self.lifetime_base + self.session;
        let (gm, gt) = self.eng.tally();
        self.ledger.update(self.session, self.tokens_spent, gm, gt);
        let up = self.loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{} (#{} lifetime)  (up {}h{:02}m)  goals {}/{} ────",
            self.session,
            self.dash.lifetime_session,
            up / 3600,
            (up % 3600) / 60,
            self.eng.tally().0,
            self.eng.tally().1
        );

        // ── isolation: cut this session's branch off the base + clear any stale red veto ──
        // `session_branch` is Some(name) only when isolation is active AND the branch was
        // created cleanly; otherwise the session runs on the current branch as before.
        let iso = &self.cfg.session_isolation;
        let session_branch: Option<String> = match &self.iso_base {
            Some(base) => {
                let br = crate::git::session_branch(&iso.branch_prefix, &self.cfg.project, self.session);
                crate::git::remove_file(self.dir, &iso.red_file); // clear stale veto before the run
                if crate::git::create_branch(self.dir, &br, base) {
                    eprintln!("  [iso] session #{} on branch {br} (off {base})", self.session);
                    Some(br)
                } else {
                    eprintln!("  [iso] could not create session branch — running on {base}");
                    None
                }
            }
            None => None,
        };
        self.session_branch = session_branch;

        // on_session_start hooks (e.g. incremental refresh of a code graph / cache).
        crate::hooks::run("on_session_start", &self.cfg.hooks.on_session_start, self.dir);

        // Layer-3 spawn scanner: flip finished long-tasks to "done", prune stale entries.
        // Runs every boundary (the harness's natural tick) — autonomous-safe, only updates
        // liveness of tasks WE registered; never kills a process it can't prove is ours.
        if let Some(change) = crate::spawns::scan(self.dir) {
            eprintln!("  [spawn] {change}");
        }

        // build the effective prompt: [operator instruction] + [spawn status] +
        // [prompt_includes] + resume. The operator instruction (if any) is consumed once;
        // the spawn status tells this session about background tasks left running by a
        // prior session (so it polls instead of relaunching); the prompt_includes prefix
        // is the user's reusable tooling/guidance fragments.
        let base = if self.prompt_prefix.is_empty() {
            self.resume_prompt.clone()
        } else {
            format!("{}\n\n{}", self.prompt_prefix, self.resume_prompt)
        };
        // prepend any tracked background-task status so the worker sees what is pending + why.
        let base = match crate::spawns::summary_for_prompt(self.dir) {
            Some(status) => format!("{status}\n{base}"),
            None => base,
        };
        // institutional memory (#3): APPEND the bounded durable slice + LAST SESSION block as the
        // LOWEST-priority tail of `base` (below the operator instruction, spawn status, and
        // prompt_includes — those keep their position). Pure code, runs every prompt, never an LLM
        // call. Empty string when there's nothing yet (fresh project), so the prompt is unchanged.
        let base = if self.cfg.memory.enabled {
            let mem = crate::memory::read_block(self.dir, &self.last_session, self.cfg.memory.inject_kb);
            if mem.is_empty() {
                base
            } else {
                format!("{base}\n\n{mem}")
            }
        } else {
            base
        };
        let effective_prompt = match self.pending_instruction.take() {
            Some(instr) => format!(
                "═══ HIGH-PRIORITY OPERATOR INSTRUCTION (act on this FIRST, it overrides the default plan) ═══\n\
                 {instr}\n\n{base}"
            ),
            None => base,
        };
        // NOTE: an `ultracode` prompt prefix was tried (to let the headless worker
        // spawn subagent Workflows) and REMOVED 2026-06-10. In `claude -p` headless
        // mode the worker fired an async Workflow then PARKED itself waiting for a
        // re-invoke that never comes (Workflow returns a task-id immediately), going
        // idle ~0% CPU until the watchdog killed it — a pure delegate-and-wait stall
        // for zero output. The work here is single-instance + sequential and does
        // not need fan-out, so the worker does it DIRECTLY (inline) instead.
        self.dash.phase = Phase::Run;
        self.publish();
        // memory: clear any stale scratch note for THIS session number left by a prior run, so a
        // worker note from a different run can never be folded as this session's learning.
        if self.cfg.memory.enabled {
            crate::memory::clear_scratch(self.dir, self.session);
        }
        Injected::Prompt(effective_prompt)
    }

    /// **RUN** — the fresh `claude -p` worker. The ONE stochastic step; agg treats it as an opaque
    /// black box and only records what it spent.
    ///
    /// `None` = a SIGINT/SIGTERM landed during the session (the worker's process group is already
    /// killed). The caller must return via [`LoopState::finish_interrupted`] — nothing is staged
    /// yet, so the base branch is untouched and there is nothing to judge. The token/cost
    /// accumulation is the ONLY thing that happens between the worker returning and that check,
    /// so the interrupted run still reports what this session spent.
    fn run(&mut self, prompt: &str) -> Option<SessionOutcome> {
        // --resume continuity (opt-in): continue the prior session's context. Default
        // is fresh-context-per-session (the core no-runaway-cost discipline).
        let resume_id = if self.cfg.resume_sessions { self.last_session_id.as_deref() } else { None };
        let outcome = worker::run_session(self.cfg, prompt, self.dir, self.session, resume_id, &self.live);
        self.last_session_id = outcome.session_id.clone();
        self.tokens_spent += outcome.output_tokens;
        self.cost_spent += outcome.cost_usd;

        if crate::signals::interrupted() {
            return None;
        }
        // (run_session now reaps any straggler in the worker's process group on exit, and the
        // worker's reader thread already streamed `now`/`think`/`recent` live — nothing to do here.)
        eprintln!(
            "  session #{} exited (code {:?}) after {}s{}{}  (+{} out-tok, {} total; +${:.4}, ${:.4} total)",
            self.session,
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            if outcome.killed_by_watchdog { "  [WATCHDOG-KILLED: hung worker]" } else { "" },
            outcome.output_tokens,
            self.tokens_spent,
            outcome.cost_usd,
            self.cost_spent,
        );
        Some(outcome)
    }

    /// The graceful SIGINT/SIGTERM exit, taken straight after RUN. Returns through the caller so
    /// the Drop guards run (run.pid cleared, on_stop hooks, ledger finalized) instead of the loop
    /// dying uncleaned — and so a killed session is never judged.
    fn finish_interrupted(&mut self) -> RunOutcome {
        eprintln!("\n⚠ interrupted (SIGINT/SIGTERM) — stopping after the current session; worker killed, base untouched.");
        self.dash.phase = Phase::Done;
        self.dash.finished = true;
        self.dash.finish_reason = "interrupted (SIGINT/SIGTERM)".into();
        let (gm, gt) = self.eng.tally();
        self.ledger.update(self.session, self.tokens_spent, gm, gt);
        self.ledger.finish(now_epoch(), "interrupted");
        self.publish();
        RunOutcome::Stopped
    }

    /// **VERIFY** — agg runs the judges itself, externally, against the filesystem. The worker
    /// never grades its own homework; this is the whole moat.
    ///
    /// Folds the enforced memory floor first (so the session's facts survive a later panic),
    /// stages the session merge so the judges test the MERGED tree, then evaluates the cycle.
    ///
    /// `None` = the session was rate-limited, i.e. incomplete: it is NOT staged, NOT merged and
    /// NOT judged, it leaves no durable memory entry, and the caller simply starts the next cycle.
    fn verify(&mut self, outcome: &SessionOutcome) -> Option<Verified> {
        // ── institutional memory (#3): ENFORCED early fold ─────────────────────────────────────
        // Fold a mechanical "session-start" floor entry RIGHT NOW, before judging/summary, so the
        // session's facts survive even if a later step in this cycle panics. This is the
        // enforcement floor: no I/O needed to produce content, no worker cooperation. GATE's
        // post-judge fold SUPERSEDES this same entry in place with goal deltas + the optional
        // worker note — so a normally-completing session leaves exactly ONE entry.
        // Skipped on a rate-limited session: that session is incomplete (we bail without judging
        // just below), so it must not leave a durable learning entry.
        let mut mem_folded = false;
        if self.cfg.memory.enabled && !outcome.rate_limited {
            let scoreboard_now = self.eng.scoreboard();
            let body = crate::memory::mechanical_note(
                outcome.exit_code,
                outcome.killed_by_watchdog,
                outcome.rate_limited,
                outcome.duration_secs,
                &scoreboard_now,
                &[], // no deltas yet — judging hasn't run; superseded below if we get there.
            );
            self.dash.memory_bytes =
                crate::memory::append_entry(self.dir, self.session, "session-start", &body, self.cfg.memory.max_kb);
            mem_folded = true;
            self.publish();
        }

        // rate-limit backoff (exit-code + terminal-event gated). NOTE: checked BEFORE merging —
        // a rate-limited session is incomplete, so we don't resolve/merge its branch at all
        // (it stays for the next attempt). (In the eager path below, resolve happens after this.)
        if outcome.rate_limited {
            let secs = self.cfg.ratelimit_backoff_secs;
            eprintln!("  rate limit detected — backing off {secs}s");
            self.dash.phase = Phase::Backoff;
            // memory: a rate-limited session is incomplete — no durable entry was written (the
            // early fold skips when rate_limited). Just clean up any scratch the worker left.
            if self.cfg.memory.enabled {
                crate::memory::clear_scratch(self.dir, self.session);
            }
            self.publish();
            std::thread::sleep(Duration::from_secs(secs));
            return None; // don't judge on a rate-limited (incomplete) session
        }

        // ── isolation: resolve the session branch ────────────────────────────────────────────
        // Default is merge-back-to-base unless the worker vetoed (red file). With the ROLLBACK
        // GATE on (rollback_on_regression, default), we STAGE the merge (uncommitted) here, GATE
        // judges the merged tree and then commits or rolls back based on whether a goal
        // REGRESSED. With the gate off, we eager-commit here exactly as before.
        let iso = &self.cfg.session_isolation;
        let staged = match (&self.iso_base, &self.session_branch) {
            (Some(base), Some(br)) if iso.rollback_on_regression => {
                Some((br.clone(), crate::git::stage_session(self.dir, base, br, &iso.red_file)))
            }
            (Some(base), Some(br)) => {
                crate::git::resolve_session(self.dir, base, br, &iso.red_file, self.session);
                None
            }
            _ => None,
        };

        // run judges, fold verdicts, evaluate conditions (incl. budget/wall guards). When a
        // merge is staged, the judges re-test the MERGED (uncommitted) working tree — the gate.
        // Snapshot goal state FIRST: if the gate rolls the merge back, we restore this so the
        // engine reflects base truth and never reports success on discarded work (W5).
        eprintln!("  running judges…");
        self.dash.phase = Phase::Verify;
        self.publish();
        let pre_cycle_goals = self.eng.snapshot_goal_state();
        let rs = self.run_state();
        let res = self.eng.evaluate_cycle(self.dir, self.config_base, &rs);
        eprint!("{}", indent(&self.eng.scoreboard()));
        self.publish();

        Some(Verified { res, staged, pre_cycle_goals, mem_folded })
    }

    /// **GATE** — keep or roll back the merge · check stop/halt · carry state forward.
    ///
    /// The deterministic decision that makes it safe to trust a stochastic worker: a staged merge
    /// is committed only if THIS cycle regressed no goal, and everything downstream (memory,
    /// summary, stop/halt) sees the post-gate truth.
    fn gate(&mut self, v: Verified, outcome: &SessionOutcome) -> GateDecision {
        let Verified { mut res, staged, pre_cycle_goals, mem_folded } = v;

        // GATE's own publish: the summarizer below can take seconds, and every publish after this
        // point is conditional — without this the dashboard would sit on "verify" through the
        // whole gate.
        self.dash.phase = Phase::Gate;
        self.publish();

        // ── rollback gate: keep the staged merge unless THIS cycle caused a regression ─────────
        // A goal REGRESSED this cycle iff a delta went from a met state to a not-met state AND the
        // judge actually RAN. The "judge ran" gate is LOAD-BEARING: a judge that merely couldn't
        // run (rate-limited/timeout/spawn-fail/bad-JSON → Verdict::failed with error:Some →
        // Goal::apply marks a previously-met goal Regressed) must NOT count as a regression, or a
        // transient flake would discard a good session's work.
        //
        // We rely ONLY on the per-cycle delta — NOT on `g.state == Regressed`. `Regressed` is
        // sticky (recomputed every cycle while unmet), so an engine-state clause vetoed every
        // future merge after one regression and cascaded after a rollback (W4). The delta clause,
        // gated on judge-ran, is necessary and sufficient once engine state is kept base-true
        // (which the rollback branch below now guarantees).
        let mut rolled_back = false;
        if let Some((br, crate::git::StagedSession::Staged)) = &staged {
            let judge_ran = |id: &str| {
                self.eng
                    .goals
                    .iter()
                    .find(|g| g.id == id)
                    .and_then(|g| g.last_verdict.as_ref())
                    .map(|v| v.error.is_none())
                    .unwrap_or(false)
            };
            let regressed = res.deltas.iter().any(|d| {
                d.before_state == crate::model::Lifecycle::Met
                    && d.after_state != crate::model::Lifecycle::Met
                    && judge_ran(&d.id)
            });
            let keep = !regressed;
            crate::git::finalize_session(self.dir, br, self.session, keep);
            if !keep {
                // W5: the merged tree was discarded. Restore engine state to pre-cycle (base)
                // truth so we never (a) report success on discarded work, (b) latch a once_met
                // goal that was only met on the discarded tree, or (c) show a phantom Met→Regressed
                // next cycle that would roll back the NEXT session. Then recompute stop/halt
                // against base — cheaply, no judges re-run — and blank the deltas so the memory
                // fold below records the regression fact without phantom "met" deltas.
                rolled_back = true;
                self.eng.restore_goal_state(&pre_cycle_goals);
                eprint!("{}", indent(&self.eng.scoreboard()));
                let rs = self.run_state();
                let recomputed = self.eng.conditions_only(&rs);
                res = CycleResult {
                    stop: recomputed.stop,
                    halt: recomputed.halt,
                    halt_reason: recomputed.halt_reason,
                    deltas: Vec::new(),
                };
            }
            self.publish();
        }

        // on_session_end hooks run AFTER judging, so they see the post-cycle state (e.g.
        // persist a memory note, update an index, refresh a graph for the next session).
        crate::hooks::run("on_session_end", &self.cfg.hooks.on_session_end, self.dir);

        // DATA-C1: only reuse the windowed summary for memory if it was FRESHLY computed this
        // cycle — never the stale persistent `dash.summary_windowed` from an earlier cycle.
        let mut summarized_this_cycle = false;

        // LLM summary (cumulative + windowed), rate-limited by min_interval_secs.
        // Best-effort: a summarizer failure NEVER breaks the loop.
        if self.cfg.summary.enabled
            && self.last_summary.elapsed().as_secs() >= self.cfg.summary.min_interval_secs
        {
            if let Some(s) = summary::summarize(
                &self.cfg.summary.model,
                &self.cumulative,
                &outcome.thoughts,
                &res.deltas,
                120,
            ) {
                eprintln!("  [SUMMARY cumulative] {}", s.cumulative);
                eprintln!("  [SUMMARY windowed]   {}", s.windowed);
                self.cumulative = s.cumulative.clone();
                self.dash.summary_cumulative = s.cumulative;
                self.dash.summary_windowed = s.windowed;
                self.last_summary = Instant::now();
                summarized_this_cycle = true;
                self.publish();
            }
        }

        // institutional memory (#3) — post-judge REFINEMENT of this session's entry. VERIFY's
        // early fold already guaranteed a mechanical note exists; here we add the richer,
        // delta-aware entry (and the optional worker note). First tier that yields content
        // wins. All I/O best-effort. Skipped only if memory is disabled or somehow not yet
        // folded (defensive — `mem_folded` is set whenever memory is enabled and VERIFY did
        // not bail on a rate limit).
        if self.cfg.memory.enabled && mem_folded {
            let scoreboard = self.eng.scoreboard();
            // the mechanical fact is ALWAYS recorded — the worker note (if any) is appended as a
            // clearly-fenced, lower-trust hint, never allowed to stand alone (so a poisoned or
            // over-confident note can't masquerade as the authoritative session record).
            let mut mech = crate::memory::mechanical_note(
                outcome.exit_code, outcome.killed_by_watchdog, outcome.rate_limited,
                outcome.duration_secs, &scoreboard, &res.deltas,
            );
            // W5: if the merge was rolled back, the work is NOT on base — say so, so the durable
            // record (which the worker is told to trust) can't be read as "this landed".
            if rolled_back {
                mech = format!(
                    "session ROLLED BACK — a goal regressed on the staged merge; the work below is \
                     NOT on the base branch (kept on the session branch for inspection).\n{mech}"
                );
            }
            // 3a: optional worker note (sanitized + size-capped + de-fanged in read_worker_note);
            //     here we additionally FENCE it so its body can never be read as live markdown.
            let worker_note = crate::memory::read_worker_note(self.dir, self.session);
            let (source, body) = match worker_note {
                Some(note) => (
                    "mechanical+worker",
                    format!(
                        "{mech}\n\n[worker note — UNTRUSTED hint, not authoritative]\n```text\n{note}\n```"
                    ),
                ),
                // 3b: reuse the windowed summary ONLY if freshly computed this cycle (no LLM call).
                None if summarized_this_cycle && !self.dash.summary_windowed.trim().is_empty() => (
                    "mechanical+summary",
                    format!("{mech}\n\nsummary: {}", self.dash.summary_windowed.trim()),
                ),
                // 3c: mechanical facts alone — cannot fail to produce content.
                None => ("mechanical", mech),
            };
            // SUPERSEDE the early "session-start" floor entry in place → exactly ONE entry per
            // completed session (no double-fold).
            self.dash.memory_bytes =
                crate::memory::fold_entry(self.dir, self.session, source, &body, self.cfg.memory.max_kb, true);
            // delete the scratch note now that it's folded (bounds `.agg/memory/` growth; prevents
            // a cross-run re-fold).
            crate::memory::clear_scratch(self.dir, self.session);
            // carry the always-on LAST SESSION block into the NEXT prompt's READ block.
            self.last_session = crate::memory::last_session_block(&res.deltas, &scoreboard);
            eprintln!("  [memory] session #{} folded ({source}); AGG_MEMORY.md {} B", self.session, self.dash.memory_bytes);
            self.publish();
        }

        if res.halt {
            let reason = res.halt_reason.unwrap_or_default();
            eprintln!(
                "\n⚠ HALT — guard condition true: {reason}\n  stopping the loop (this is a guard, not success)."
            );
            self.dash.phase = Phase::Done;
            self.dash.finished = true;
            self.dash.finish_reason = format!("HALT: {reason}");
            let (gm, gt) = self.eng.tally();
            self.ledger.update(self.session, self.tokens_spent, gm, gt);
            self.ledger.finish(now_epoch(), &format!("halt:{reason}"));
            self.publish();
            return GateDecision::Stop(RunOutcome::Halt);
        }
        if res.stop {
            let (mt, tt) = self.eng.tally();
            eprintln!("\n✔ STOP condition satisfied — {mt}/{tt} goals met. Done after {} session(s).", self.session);
            self.dash.phase = Phase::Done;
            self.dash.finished = true;
            self.dash.finish_reason = format!("{mt}/{tt} goals met after {} session(s)", self.session);
            self.ledger.update(self.session, self.tokens_spent, mt, tt);
            self.ledger.finish(now_epoch(), "goals-met");
            self.publish();
            return GateDecision::Stop(RunOutcome::GoalsMet);
        }
        GateDecision::Loop
    }
}

pub fn run(
    cfg: AggConfig,
    eng: Engine,
    dir: &Path,
    config_base: &Path,
    max_sessions: u32,
) -> Result<RunOutcome> {
    // ── double-run guard (BOTH foreground and detached) ──────────────────────────────────
    // Refuse to start a second loop over the same project: two loops would launch competing
    // workers that fight over the repo, and `agg stop` could only target one. `live_pid`
    // returns Some(pid) only if run.pid names a process that is actually alive (a stale
    // pidfile from a crashed loop is cleaned up and ignored). We exempt our OWN pid because
    // the detached child re-runs `agg run` after `spawn_detached` already wrote the child's
    // pid to run.pid — so the child legitimately finds its own pid here and must NOT bail.
    if let Some(pid) = crate::detach::live_pid(dir) {
        if pid != std::process::id() {
            anyhow::bail!(
                "a loop is already running in this project (pid {pid}).\n  \
                 watch it:   agg dashboard\n  \
                 stop it:    agg stop\n  \
                 (if you're sure it's dead, remove .agg/run.pid and retry.)"
            );
        }
    }
    // Record THIS process as the live loop so `agg stop` / the double-run guard read a
    // current pid. Covers BOTH foreground `agg run` and the detached child (which re-runs
    // `agg run` for real) — the child overwrites the launcher's pid with its own.
    crate::detach::write_run_pid(dir);
    let _run_pid_guard = RunPidGuard { dir };

    // Install SIGINT/SIGTERM handling: a Ctrl-C now kills the worker's process group (no orphan)
    // and lets the loop return through its Drop guards instead of dying uncleaned. Checked at the
    // phase boundaries below via signals::interrupted().
    crate::signals::install();

    // the resume prompt sits next to agg.yaml → resolve against config_base (the `agg/` folder
    // when in use, else the project root).
    let resume_prompt = read_resume_prompt(config_base, &cfg.resume_prompt)?;

    // Honesty notice: AgenticGoGo is unix-first. On Windows the core loop (launch → judge →
    // stop) works, but two safety features degrade and we say so rather than pretend otherwise:
    //   • the watchdog can't detect a CPU-flat hang (no `ps -o time`), so a wedged worker is
    //     only caught by max-sessions / your own stop, not the idle+cpu-flat watchdog;
    //   • `agg spawn` protection + straggler reaping rely on POSIX process groups, which
    //     Windows lacks — a leaked background child may not be swept.
    #[cfg(not(unix))]
    eprintln!(
        "  ⚠ Windows: unix-first build — the CPU-flat watchdog and process-group spawn\n    \
         protection/reaping are NOT active here. The core loop runs; use `max_sessions` and\n    \
         `agg stop` as your guards. (Full Windows support is not implemented.)"
    );

    // ---- lifecycle hooks (tool-agnostic): on_start once, background watchers spawned now,
    //      on_stop guaranteed on any exit via the Drop guard. ----
    crate::hooks::run("on_start", &cfg.hooks.on_start, dir);
    crate::hooks::spawn_background(&cfg.hooks.background, dir);
    let _stop_hooks = StopHooks { cmds: cfg.hooks.on_stop.clone(), dir };
    // prompt-include fragments, composed once (the resume prompt is read once at launch too).
    let prompt_prefix = crate::hooks::gather_prompt_includes(&cfg.prompt_includes, dir);

    let loop_start = Instant::now();
    let (m, t) = eng.tally();
    eprintln!(
        "════════════════════════════════════════════════════════════\n\
         AgenticGoGo — project {}  model {}\n\
         goals {m}/{t}  stop_when: {}\n\
         ════════════════════════════════════════════════════════════\n\
         ▶ watch live:  run `agg dashboard` in another terminal\n\
         ⏱ first session may take a minute — the worker is warming up, not hung\n\
         ⏹ stop anytime: `agg stop`",
        cfg.project, cfg.model, eng.stop_when
    );

    // ---- dashboard state: the loop + worker publish a compact snapshot to
    //      .agg/state.json; `agg dashboard` renders it. Two-stream discipline:
    //      the stdout log above stays the source of truth; this is just a view. ----
    let dash = DashboardState {
        project: cfg.project.clone(),
        model: cfg.model.clone(),
        stop_when: eng.stop_when.clone(),
        halt_when: eng.halt_when.clone().unwrap_or_default(),
        budget_total: cfg.budget.total,
        cost_limit: cfg.cost.total,
        phase: Phase::Starting,
        ..Default::default()
    };
    let live = LiveState::new(dir, loop_start, dash.clone());

    // Persistent project run-history ledger (.agg/project.json): append an in-flight record for
    // THIS run, finalized on any exit via its Drop guard. Created BEFORE the baseline evaluation
    // so that even a run that is already-satisfied (or halts) at launch still leaves a history
    // record — `agg history` would otherwise miss zero-session runs. The lifetime session total
    // (shown on the dashboard so a restart doesn't look like the work started over) is derived
    // from prior runs; `session` (per-run) still drives --resume/labels.
    let ledger =
        crate::project::RunLedger::begin(dir, &cfg.project, std::process::id(), now_epoch());
    let lifetime_base = ledger.prior_lifetime_sessions();

    // `st` is declared AFTER `_stop_hooks`/`_run_pid_guard` so it drops FIRST: the ledger's Drop
    // (which finalizes the run record) must fire before the on_stop hooks and the run.pid clear,
    // exactly as it did when these were separate locals.
    let mut st = LoopState {
        cfg: &cfg,
        dir,
        config_base,
        resume_prompt,
        prompt_prefix,
        eng,
        dash,
        live,
        ledger,
        bus: None, // opened after the baseline evaluation, below
        // run-level accounting for the ceiling guards. The iteration cap is `max_sessions`
        // (0 = unlimited → None so `over_iterations` never trips).
        budget_total: cfg.budget.total,
        cost_limit: cfg.cost.total,
        max_iter: if max_sessions == 0 { None } else { Some(max_sessions) },
        max_sessions,
        loop_start,
        lifetime_base,
        session: 0,
        tokens_spent: 0,
        cost_spent: 0.0,
        pending_instruction: None,
        last_session: String::new(),
        last_session_id: None,
        cumulative: String::new(),
        last_summary: loop_start, // real value set below, after the baseline evaluation
        iso_base: None,
        session_branch: None,
    };
    st.publish();
    st.dash.lifetime_session = lifetime_base;

    // Evaluate the goals ONCE up front (run the judges) — maybe we're already done,
    // or an invariant is already broken, before burning a single session.
    eprintln!("  baseline: running judges once before the first session…");
    st.dash.phase = Phase::Verify;
    st.publish();
    let rs = st.run_state();
    let pre = st.eng.evaluate_cycle(dir, config_base, &rs);
    eprint!("{}", indent(&st.eng.scoreboard()));
    st.publish();
    if pre.halt {
        eprintln!("⚠ HALT at baseline — guard already true: {}", pre.halt_reason.clone().unwrap_or_default());
        st.dash.phase = Phase::Done;
        st.dash.finished = true;
        st.dash.finish_reason = format!("HALT at baseline: {}", pre.halt_reason.clone().unwrap_or_default());
        let (gm, gt) = st.eng.tally();
        st.ledger.update(0, 0, gm, gt);
        st.ledger.finish(now_epoch(), &format!("halt-at-baseline:{}", pre.halt_reason.unwrap_or_default()));
        st.publish();
        return Ok(RunOutcome::Halt);
    }
    if pre.stop {
        eprintln!("✔ stop condition already satisfied at launch — nothing to do.");
        st.dash.phase = Phase::Done;
        st.dash.finished = true;
        st.dash.finish_reason = "already satisfied at launch".into();
        let (gm, gt) = st.eng.tally();
        st.ledger.update(0, 0, gm, gt);
        st.ledger.finish(now_epoch(), "already-satisfied");
        st.publish();
        return Ok(RunOutcome::GoalsMet);
    }

    // summarizer state: the rolling cumulative summary + last-summary timestamp.
    st.last_summary = Instant::now() - Duration::from_secs(cfg.summary.min_interval_secs);

    // institutional memory (#3): the LAST SESSION block carried into the NEXT prompt's READ
    // injection (prior cycle's deltas + scoreboard). Empty on session 1 of THIS invocation; the
    // durable file's newest entry is the cross-RUN carry. Make memory work WITHOUT git isolation
    // by creating `.agg/memory/` ourselves (not via the isolation-only gitignore path).
    if cfg.memory.enabled {
        crate::memory::ensure_scratch_dir(dir);
        // sweep any scratch notes left by a prior run (crash / forged filename) — the durable
        // AGG_MEMORY.md is the only legitimate cross-run carrier, so they are all stale.
        crate::memory::sweep_scratch(dir);
    }

    // ---- bus: operator/outer-Claude steering, drained at each session boundary
    //      (the only safe injection point for headless workers). ----
    st.bus = Bus::open(dir).ok();

    // ── per-session git isolation (opt-in) ────────────────────────────────────────────────
    // Capture the base branch ONCE at startup. Each session runs on its own branch off this
    // base and is merged back UNLESS the worker vetoed it (red file). Disabled cleanly if the
    // repo isn't in a usable state (not a repo / detached HEAD / dirty tree) — isolation is an
    // enhancement, never a correctness requirement.
    let iso = &cfg.session_isolation;
    st.iso_base = if iso.enabled {
        // Recover a staged merge stranded by an interrupted previous run (Ctrl-C/crash/kill during
        // the rollback-gate judging window) BEFORE the is_clean check — otherwise the leftover
        // MERGE_HEAD makes is_clean false and silently disables isolation. No-op if not a repo.
        if crate::git::is_repo(dir) {
            crate::git::recover_stranded_merge(dir, &iso.branch_prefix);
        }
        if !crate::git::is_repo(dir) {
            eprintln!("  [iso] session_isolation enabled but not a git repo — running on current branch");
            None
        } else if !crate::git::is_clean(dir) {
            eprintln!("  [iso] session_isolation enabled but work tree has tracked changes — commit/stash first; running on current branch");
            None
        } else {
            // keep agg's runtime state out of git so it never lands on session branches / base.
            crate::git::ensure_agg_gitignored(dir);
            let base = if iso.base_branch.is_empty() {
                crate::git::current_branch(dir)
            } else {
                Some(iso.base_branch.clone())
            };
            match &base {
                Some(b) => eprintln!("  [iso] per-session branch isolation ON — base branch '{b}', merge unless '{}' present", iso.red_file),
                None => eprintln!("  [iso] session_isolation enabled but HEAD is detached — running on current branch"),
            }
            base
        }
    } else {
        None
    };

    // ── the deterministic outer loop ──────────────────────────────────────────────────────
    // Four stages, in order, every cycle. The `max_sessions` cap is a pre-check on the
    // pre-increment count, so it fires before INJECT opens another session.
    loop {
        if let Some(outcome) = st.over_max_sessions() {
            return Ok(outcome);
        }

        let prompt = match st.inject() {
            // INJECT
            Injected::Prompt(p) => p,
            Injected::Stop(outcome) => return Ok(outcome), // `agg stop`, incl. while paused
        };

        let Some(outcome) = st.run(&prompt) else {
            // RUN
            return Ok(st.finish_interrupted()); // SIGINT/SIGTERM — nothing staged, nothing judged
        };

        let Some(verified) = st.verify(&outcome) else {
            // VERIFY
            continue; // rate-limited: incomplete session, not judged — just go round again
        };

        match st.gate(verified, &outcome) {
            // GATE
            GateDecision::Loop => continue,
            GateDecision::Stop(outcome) => return Ok(outcome),
        }
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}

/// Read the resume prompt from `base`. If it's missing but a sibling `<name>.template` exists
/// (the convention the bundled examples ship — the real prompt is gitignored so the user
/// personalises it), fail with the EXACT `cp` to run rather than a bare "No such file". This
/// is the example-footgun fix: `cd examples/hello-agg && agg run` used to fail cryptically.
fn read_resume_prompt(base: &Path, name: &str) -> Result<String> {
    let path = base.join(name);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) => {
            let template = base.join(format!("{name}.template"));
            if template.exists() {
                anyhow::bail!(
                    "resume prompt `{name}` is missing, but `{name}.template` is here.\n  \
                     copy it and edit for your run:\n    cp {} {}\n  \
                     (the real prompt is gitignored on purpose — it's yours to personalise.)",
                    template.display(),
                    path.display()
                );
            }
            Err(anyhow::Error::new(e).context(format!("reading resume prompt {name}")))
        }
    }
}

use crate::util::now_epoch;

#[cfg(test)]
mod tests {
    use super::read_resume_prompt;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agg-loop-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_an_existing_resume_prompt() {
        let d = tmpdir("present");
        std::fs::write(d.join("AGG_RESUME.md"), "do the thing").unwrap();
        assert_eq!(read_resume_prompt(&d, "AGG_RESUME.md").unwrap(), "do the thing");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn missing_with_template_gives_cp_hint() {
        let d = tmpdir("template");
        std::fs::write(d.join("AGG_RESUME.md.template"), "starter").unwrap();
        let err = read_resume_prompt(&d, "AGG_RESUME.md").unwrap_err().to_string();
        assert!(err.contains(".template"), "should mention the template: {err}");
        assert!(err.contains("cp "), "should give a cp command: {err}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn missing_without_template_is_a_plain_error() {
        let d = tmpdir("absent");
        let err = read_resume_prompt(&d, "AGG_RESUME.md").unwrap_err().to_string();
        assert!(err.contains("reading resume prompt"), "plain read error: {err}");
        assert!(!err.contains("cp "), "no spurious cp hint when no template: {err}");
        std::fs::remove_dir_all(&d).ok();
    }
}
