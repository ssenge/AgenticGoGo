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

/// What a session is being ASKED to do — the axis the worker's prompt varies on (§5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Carry the standing instructions forward.
    Continue,
    /// Step back and re-examine the plan (§3). Selected for a `skip_judges` step, whose own
    /// `prompt:` carries the red-team framing; the arm is additive so the seam is real without
    /// duplicating that prompt.
    Reconsider,
}

/// What INJECT produced.
enum Injected {
    Prompt(String),
    Stop(RunOutcome),
}

/// What VERIFY produced: the judged (or skipped) step, plus everything GATE needs.
struct Verified {
    res: CycleResult,
    /// the staged git merge for a JUDGED step (`None` for a skip step or a non-staged disposition).
    staged: Option<(String, crate::git::StagedSession)>,
    /// pre-step (base) judge truth, so a rollback can restore it (W5).
    pre_cycle_goals: Vec<GoalRuntime>,
    mem_folded: bool,
    /// this step ran no judges (§5.7) — its work STAGES on the span.
    skip: bool,
}

enum GateDecision {
    Loop,
    Stop(RunOutcome),
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

    /// **INJECT** — next step → state + steering → the worker's prompt. Cuts the session branch off
    /// the span tip (or base) and composes the prompt in the §5.6 order.
    fn inject(&mut self) -> Injected {
        self.emit(LifecycleEvent::Inject);

        // ── drain the bus at the session boundary ──
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
                    let stopped = match &self.bus {
                        Some(bus) => wait_for_resume(bus),
                        None => None,
                    };
                    if let Some(reason) = stopped {
                        return Injected::Stop(self.stopped_via_bus(reason));
                    }
                }
                Command::Resume => {}
                Command::Stop { reason } => return Injected::Stop(self.stopped_via_bus(reason)),
                Command::Note { text } => eprintln!("  [bus] note: {text}"),
            }
        }

        // ── pick the next step from the sequence (branch conditions read current judge state) ──
        let step_name = {
            let rs = self.run_state();
            let eng = &self.eng;
            let picked = self.cursor.next_step(&mut |cond| {
                let ctx = StopContext {
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
                stop::evaluate(cond, &ctx)
            });
            match picked {
                Ok(n) => n,
                Err(e) => return Injected::Stop(self.abort_now(&format!("sequence error: {e}"))),
            }
        };
        let step = match self.cfg.resolve_step(&step_name) {
            Ok(s) => s,
            Err(e) => return Injected::Stop(self.abort_now(&format!("{e}"))),
        };

        self.session += 1;
        self.dash.session = self.session;
        self.dash.lifetime_session = self.lifetime_base + self.session;
        let (gm, gt) = self.eng.tally();
        self.ledger.update(self.session, self.tokens_spent, gm, gt);
        let up = self.loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{} (#{} lifetime)  step `{}` [{}]  (up {}h{:02}m)  goals {gm}/{gt} ────",
            self.session,
            self.dash.lifetime_session,
            step.name,
            step.agent,
            up / 3600,
            (up % 3600) / 60,
        );

        // ── isolation: cut this session's branch off the span tip (or base) ──
        let iso = &self.cfg.session_isolation;
        let base_ref = self.base_ref().to_string();
        let br = crate::git::session_branch(&iso.branch_prefix, &self.cfg.project, self.session);
        crate::git::remove_file(self.dir, &iso.red_file); // clear a stale veto
        self.session_branch = if crate::git::create_branch(self.dir, &br, &base_ref) {
            eprintln!("  [iso] session #{} on branch {br} (off {base_ref})", self.session);
            Some(br)
        } else {
            eprintln!("  [iso] could not create session branch — running on {base_ref}");
            None
        };

        crate::hooks::run("on_session_start", &self.cfg.hooks.on_session_start, self.dir);
        if let Some(change) = crate::os::spawns::scan(self.dir) {
            eprintln!("  [spawn] {change}");
        }

        // capture the state file so we can warn if the agent never updated it (§5.6).
        let state_path = self.config_base.join(&step.state);
        self.state_before = std::fs::read_to_string(&state_path).ok();

        let role = if step.skip_judges { Role::Reconsider } else { Role::Continue };
        let prompt = self.compose_prompt(&step, role);
        self.cur_step = Some(step);

        self.emit(LifecycleEvent::Run);
        if self.cfg.memory.enabled {
            crate::core::memory::clear_scratch(self.dir, self.session);
        }
        Injected::Prompt(prompt)
    }

    /// Compose the worker prompt in the §5.6 order (highest priority first): operator instruction,
    /// spawn status, the step's ADDITIVE `prompt:`, prompt_includes, injected memory, then the
    /// state file (`AGG_STATE.md`) as the lowest-priority tail.
    fn compose_prompt(&mut self, step: &ResolvedStep, role: Role) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(instr) = self.pending_instruction.take() {
            parts.push(format!(
                "═══ HIGH-PRIORITY OPERATOR INSTRUCTION (act on this FIRST, it overrides the default plan) ═══\n{instr}"
            ));
        }
        if let Some(status) = crate::os::spawns::summary_for_prompt(self.dir) {
            parts.push(status);
        }
        // the Reconsider framing is additive; the step's own `prompt:` carries the specifics.
        if role == Role::Reconsider {
            parts.push(
                "═══ RECONSIDER — step back before pushing forward ═══\n\
                 Do NOT just continue the current approach. Re-examine whether it is the right one."
                    .to_string(),
            );
        }
        if let Some(p) = &step.prompt {
            parts.push(p.clone());
        }
        if !self.prompt_prefix.is_empty() {
            parts.push(self.prompt_prefix.clone());
        }
        if self.cfg.memory.enabled {
            let mem = crate::core::memory::read_block(self.dir, &self.last_session, self.cfg.memory.inject_kb);
            if !mem.is_empty() {
                parts.push(mem);
            }
        }
        // the forward state file — lowest priority, the agent maintains it (§5.6).
        if let Ok(s) = std::fs::read_to_string(self.config_base.join(&step.state)) {
            if !s.trim().is_empty() {
                parts.push(s);
            }
        }
        parts.join("\n\n")
    }

    /// **RUN** — the fresh worker for THIS step's (agent, model, effort). `None` = interrupted.
    fn run(&mut self, prompt: &str) -> Option<SessionOutcome> {
        let step = self.cur_step.clone().expect("inject set cur_step");
        let agent = match step.backend() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  step `{}` names an unknown agent: {e}", step.name);
                self.dud_streak += 1;
                return Some(SessionOutcome {
                    exit_code: None,
                    duration_secs: 0,
                    rate_limited: false,
                    killed_by_watchdog: false,
                    output_tokens: 0,
                    cost_usd: 0.0,
                    thoughts: vec![],
                    session_id: None,
                });
            }
        };
        let model = step.model(agent).to_string();
        let effort = step.effort(agent).to_string();
        let outcome = worker::run_session(
            self.cfg,
            agent,
            &model,
            &effort,
            &step.worker_args,
            prompt,
            self.dir,
            self.session,
            &self.live,
        );
        self.tokens_spent += outcome.output_tokens;
        self.cost_spent += outcome.cost_usd;
        self.charge(&step.agent, outcome.output_tokens, Some(outcome.cost_usd));

        if crate::os::signals::interrupted() {
            return None;
        }
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
        // warn (loudly) if the agent never touched its forward state file (§5.6 / OQ3).
        if let (Some(step), false) = (&self.cur_step, outcome.rate_limited) {
            let now = std::fs::read_to_string(self.config_base.join(&step.state)).ok();
            if now == self.state_before {
                eprintln!("  ⚠ the worker did not update `{}` this session — the next session inherits stale forward-state.", step.state);
            }
        }

        let dud = !outcome.rate_limited && outcome.exit_code != Some(0) && outcome.output_tokens == 0;
        self.dud_streak = if dud { self.dud_streak + 1 } else { 0 };
        Some(outcome)
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
            let body = crate::core::memory::mechanical_note(
                outcome.exit_code,
                outcome.killed_by_watchdog,
                outcome.rate_limited,
                outcome.duration_secs,
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

    /// **VERIFY** — run the run-set judges (unless `skip_judges`) against the staged tree, or stage
    /// the span for a skip step. `None` = the session was rate-limited (incomplete): NOT judged, NOT
    /// merged; the caller loops. Ceilings ARE checked on the rate-limit path (§5.5 item 6 fix).
    fn verify(&mut self, outcome: &SessionOutcome) -> Option<Verified> {
        let mem_folded = self.fold_memory_floor(outcome);

        if outcome.rate_limited {
            let secs = self.cfg.ratelimit_backoff_secs;
            eprintln!("  rate limit detected — backing off {secs}s");
            if self.cfg.memory.enabled {
                crate::core::memory::clear_scratch(self.dir, self.session);
            }
            // §5.5 item 6: check the ceilings even here — an all-night rate-limit spin must still
            // trip `wall_hours`/`over_budget`, which the old `return` before `evaluate` never did.
            let rs = self.run_state();
            let ceil = self.eng.conditions_only(&rs);
            if ceil.halt {
                eprintln!("  ⚠ ceiling tripped during backoff — aborting");
                let _ = self.abort_now(&format!("abort_if: {}", ceil.halt_reason.unwrap_or_default()));
                // signal the caller to stop by returning a Verified marked halt.
                return Some(Verified {
                    res: CycleResult { halt: true, ..Default::default() },
                    staged: None,
                    pre_cycle_goals: self.eng.snapshot_goal_state(),
                    mem_folded,
                    skip: true,
                });
            }
            self.emit(LifecycleEvent::Backoff);
            std::thread::sleep(Duration::from_secs(secs));
            return None;
        }

        let step = self.cur_step.clone().expect("cur_step set");
        let pre_cycle_goals = self.eng.snapshot_goal_state();

        if step.skip_judges {
            // ── STAGE the span (§5.7): no judges, keep the branch, extend the span tip. ──
            self.emit(LifecycleEvent::Staging);
            let iso = &self.cfg.session_isolation;
            let vetoed = self.dir.join(&iso.red_file).exists();
            if vetoed {
                eprintln!("  [span] session #{} VETOED (red_file) → work discarded, not staged", self.session);
                crate::git::remove_file(self.dir, &iso.red_file);
                // leave the branch orphaned; the span tip is unchanged.
            } else if let Some(br) = &self.session_branch {
                eprintln!("  [span] session #{} staged on {br} (skip_judges) — nothing merged yet", self.session);
                self.span_tip = Some(br.clone());
                self.span_branches.push(br.clone());
            }
            // ceilings only (no judges ran) — done_if reads stale state and cannot fire, ceilings can.
            let rs = self.run_state();
            let res = self.eng.run_step(self.dir, &rs, self.ruler, &self.judge_model, self.judge_timeout, &step.name, Some(self.session), true);
            self.publish();
            return Some(Verified { res, staged: None, pre_cycle_goals, mem_folded, skip: true });
        }

        // ── JUDGED step: stage the merge so the judges test the MERGED tree, then judge. ──
        let iso = &self.cfg.session_isolation;
        let staged = match &self.session_branch {
            Some(br) => Some((br.clone(), crate::git::stage_session(self.dir, &self.iso_base, br, &iso.red_file))),
            None => None,
        };

        eprintln!("  running judges…");
        self.emit(LifecycleEvent::Verify);
        let rs = self.run_state();
        let res = self.eng.run_step(self.dir, &rs, self.ruler, &self.judge_model, self.judge_timeout, &step.name, Some(self.session), false);
        // §5.6: judge spend counts against the ceilings — and against the RULER's per-agent tally.
        self.tokens_spent += res.judge_tokens;
        if let Some(c) = res.judge_cost {
            self.cost_spent += c;
        }
        let ruler_agent = self.cfg.judge.agent.clone();
        self.charge(&ruler_agent, res.judge_tokens, res.judge_cost);
        eprint!("{}", indent(&self.eng.scoreboard()));
        self.publish();

        Some(Verified { res, staged, pre_cycle_goals, mem_folded, skip: false })
    }

    /// **GATE** — keep / roll back the judged merge, or record the staged span · check done/abort.
    fn gate(&mut self, v: Verified, outcome: &SessionOutcome) -> Result<GateDecision> {
        let Verified { mut res, staged, pre_cycle_goals, mem_folded, skip } = v;

        // a ceiling tripped during rate-limit backoff already emitted Finished.
        if skip && res.halt && staged.is_none() && res.fresh_verdicts.is_empty() && res.deltas.is_empty() {
            return Ok(GateDecision::Stop(RunOutcome::Halt));
        }

        self.emit(LifecycleEvent::Gate);

        let step_name = self.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let mut rolled_back = false;

        if !skip {
            match &staged {
                Some((br, crate::git::StagedSession::Staged)) => {
                    // the regression gate: a DoD-set judge MET before (durable, §5.7) that now fails.
                    // Scope to the DoD-set exactly as `any_regressed`/`count_regressed` do (stop.rs
                    // `in_scope` → `g.in_dod`). A run-set-only control judge like `stalled` is DESIGNED
                    // to flip met→unmet — that flip is the very signal that fired `reconsider` — so
                    // counting its flip as a regression would roll back the work that escaped the stall
                    // (and, because rolled-back rows never land, livelock the loop). §5.7 protects the
                    // DoD-set; a judge named only in an `if` condition is not in it.
                    let landed = crate::core::verdicts::landed_met(self.dir);
                    let regressed = res.fresh_verdicts.iter().any(|(id, v)| {
                        self.eng.judges.iter().any(|g| g.in_dod && &g.name == id)
                            && v.error.is_none()
                            && !v.met
                            && landed.get(id).copied().unwrap_or(false)
                    });
                    let keep = if self.gate_regressions { !regressed } else { true };
                    crate::git::finalize_session(self.dir, br, self.session, keep);
                    let tag = if keep {
                        crate::core::verdicts::Outcome::Merged
                    } else {
                        crate::core::verdicts::Outcome::RolledBack
                    };
                    crate::core::verdicts::append(self.dir, Some(self.session), &step_name, &res.fresh_verdicts, tag)?;
                    if keep {
                        // the whole span merged with this branch (it descends from the span). Clear it.
                        // ponytail: intermediate span branches are left as refs (no public delete);
                        // harmless, and cleanup is a later polish. REPORTED.
                        self.span_tip = None;
                        self.span_branches.clear();
                    } else {
                        rolled_back = true;
                        self.eng.restore_goal_state(&pre_cycle_goals);
                        self.span_tip = None; // span discarded; next cuts off base
                        self.span_branches.clear();
                        eprint!("{}", indent(&self.eng.scoreboard()));
                        let rs = self.run_state();
                        let recomputed = self.eng.conditions_only(&rs);
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
                    self.publish();
                }
                _ => {
                    // Vetoed / NoChanges / Conflict / CheckoutFailed / no branch: nothing merged. The
                    // judged verdicts describe base, not a landed merge — record them rolled_back and
                    // restore base truth so the next step isn't gated against a phantom.
                    self.eng.restore_goal_state(&pre_cycle_goals);
                    self.span_tip = None;
                    self.span_branches.clear();
                    crate::core::verdicts::append(
                        self.dir,
                        Some(self.session),
                        &step_name,
                        &res.fresh_verdicts,
                        crate::core::verdicts::Outcome::RolledBack,
                    )?;
                    let rs = self.run_state();
                    let recomputed = self.eng.conditions_only(&rs);
                    res = CycleResult {
                        stop: recomputed.stop,
                        halt: recomputed.halt,
                        halt_reason: recomputed.halt_reason,
                        deltas: Vec::new(),
                        fresh_verdicts: Vec::new(),
                        judge_tokens: 0,
                        judge_cost: None,
                    };
                    self.publish();
                }
            }
        }

        crate::hooks::run("on_session_end", &self.cfg.hooks.on_session_end, self.dir);

        // ── summary (best-effort) ──
        let mut summarized_this_cycle = false;
        if self.cfg.summary.enabled
            && self.last_summary.elapsed().as_secs() >= self.cfg.summary.min_interval_secs
        {
            let model = self.ruler.default_summary_model().to_string();
            if let Some((s, spend)) = summary::summarize(
                self.ruler,
                &model,
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
                // §5.6: summarizer spend counts too — the summarizer runs on the ruler.
                self.tokens_spent += spend.tokens;
                if let Some(c) = spend.cost_usd {
                    self.cost_spent += c;
                }
                let ruler_agent = self.cfg.judge.agent.clone();
                self.charge(&ruler_agent, spend.tokens, spend.cost_usd);
                self.publish();
            }
        }

        // ── institutional memory: post-judge refinement ──
        if self.cfg.memory.enabled && mem_folded {
            let scoreboard = self.eng.scoreboard();
            let mut mech = crate::core::memory::mechanical_note(
                outcome.exit_code, outcome.killed_by_watchdog, outcome.rate_limited,
                outcome.duration_secs, &scoreboard, &res.deltas,
            );
            if rolled_back {
                mech = format!(
                    "session ROLLED BACK — a goal regressed on the staged merge; the work below is \
                     NOT on the base branch (kept on the session branch for inspection).\n{mech}"
                );
            }
            let worker_note = crate::core::memory::read_worker_note(self.dir, self.session);
            let (source, body) = match worker_note {
                Some(note) => (
                    "mechanical+worker",
                    format!("{mech}\n\n[worker note — UNTRUSTED hint, not authoritative]\n```text\n{note}\n```"),
                ),
                None if summarized_this_cycle && !self.dash.summary_windowed.trim().is_empty() => (
                    "mechanical+summary",
                    format!("{mech}\n\nsummary: {}", self.dash.summary_windowed.trim()),
                ),
                None => ("mechanical", mech),
            };
            self.dash.memory_bytes = crate::core::memory::fold_entry(
                self.dir, self.session, source, &body, self.cfg.memory.max_kb, true,
            );
            crate::core::memory::clear_scratch(self.dir, self.session);
            self.last_session = crate::core::memory::last_session_block(&res.deltas, &scoreboard);
            eprintln!("  [memory] session #{} folded ({source}); AGG_MEMORY.md {} B", self.session, self.dash.memory_bytes);
            self.publish();
        }

        if res.halt {
            let reason = res.halt_reason.unwrap_or_default();
            eprintln!("\n⚠ ABORT — abort_if true: {reason}\n  stopping the loop (a ceiling / guard, not success).");
            self.report_stranded_span();
            self.emit(LifecycleEvent::Finished {
                reason: format!("ABORT: {reason}"),
                ledger_tag: format!("abort:{reason}"),
            });
            return Ok(GateDecision::Stop(RunOutcome::Halt));
        }
        if res.stop {
            let (mt, tt) = self.eng.tally();
            eprintln!("\n✔ done_if satisfied — {mt}/{tt} goals met. Done after {} session(s).", self.session);
            self.emit(LifecycleEvent::Finished {
                reason: format!("{mt}/{tt} goals met after {} session(s)", self.session),
                ledger_tag: "goals-met".into(),
            });
            return Ok(GateDecision::Stop(RunOutcome::GoalsMet));
        }
        Ok(GateDecision::Loop)
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
    crate::hooks::spawn_background(&cfg.hooks.background, dir);
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

    // max_sessions: the CLI flag WINS when passed (§4.1), else the config key.
    let max_sessions = if max_sessions_flag > 0 { max_sessions_flag } else { cfg.sequence.max_sessions };

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
        budget_total: cfg.sequence.budget.total,
        cost_limit: cfg.sequence.cost.total,
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
        budget_total: cfg.sequence.budget.total,
        cost_limit: cfg.sequence.cost.total,
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
    };
    st.publish();
    st.dash.lifetime_session = lifetime_base;

    // ── baseline pass (§5.5.1): judge the untouched repo once; write `baseline` verdicts. ──
    eprintln!("  baseline: running judges once before the first session…");
    st.dash.phase = Phase::Verify;
    st.publish();
    let rs = st.run_state();
    let pre = st.eng.run_step(dir, &rs, ruler, &st.judge_model, st.judge_timeout, "baseline", None, false);
    st.tokens_spent += pre.judge_tokens;
    if let Some(c) = pre.judge_cost {
        st.cost_spent += c;
    }
    let ruler_agent = st.cfg.judge.agent.clone();
    st.charge(&ruler_agent, pre.judge_tokens, pre.judge_cost);
    eprint!("{}", indent(&st.eng.scoreboard()));
    st.publish();
    crate::core::verdicts::append(dir, None, "baseline", &pre.fresh_verdicts, crate::core::verdicts::Outcome::Baseline)?;
    if pre.halt {
        eprintln!("⚠ ABORT at baseline — abort_if already true: {}", pre.halt_reason.clone().unwrap_or_default());
        st.dash.phase = Phase::Done;
        st.dash.finished = true;
        st.dash.finish_reason = format!("ABORT at baseline: {}", pre.halt_reason.clone().unwrap_or_default());
        let (gm, gt) = st.eng.tally();
        st.ledger.update(0, 0, gm, gt);
        st.ledger.finish(now_epoch(), &format!("abort-at-baseline:{}", pre.halt_reason.unwrap_or_default()));
        st.publish();
        return Ok(RunOutcome::Halt);
    }
    if pre.stop {
        eprintln!("✔ done_if already satisfied at launch — nothing to do.");
        st.dash.phase = Phase::Done;
        st.dash.finished = true;
        st.dash.finish_reason = "already satisfied at launch".into();
        let (gm, gt) = st.eng.tally();
        st.ledger.update(0, 0, gm, gt);
        st.ledger.finish(now_epoch(), "already-satisfied");
        st.publish();
        return Ok(RunOutcome::GoalsMet);
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
        let prompt = match st.inject() {
            Injected::Prompt(p) => p,
            Injected::Stop(outcome) => return Ok(outcome),
        };
        let Some(outcome) = st.run(&prompt) else {
            return Ok(st.finish_interrupted());
        };
        if let Some(e) = st.worker_is_broken() {
            return Err(e);
        }
        let Some(verified) = st.verify(&outcome) else {
            continue; // rate-limited: incomplete session, not judged — go round again
        };
        match st.gate(verified, &outcome)? {
            GateDecision::Loop => continue,
            GateDecision::Stop(outcome) => return Ok(outcome),
        }
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}

use crate::util::now_epoch;
