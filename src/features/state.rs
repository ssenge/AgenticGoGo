//! agg's OWN plugin state — the built-in plugin's data, stored in the run's generic `Extensions`
//! store (LOOPSTATE_REDESIGN §3.1). A third-party plugin defines its own state type the same way;
//! this is just agg's. `AGGState` is per-run (in `ext`); `AGGScratch` is per-session (in `scratch`,
//! `clear()`ed each iteration).

use std::time::Instant;

use crate::backend::worker::SessionOutcome;
use crate::core::engine::{CycleResult, GoalRuntime};

/// ALL of agg's own per-run feature state, organised by feature. Persists across sessions (git span,
/// summarizer window, memory tail, worker health); NEVER cleared mid-run.
#[derive(Default)]
pub struct AGGState {
    pub git: GitIso,
    pub summary: Summary,
    pub memory: Memory,
    pub worker: WorkerHealth,
    pub operator: Operator,
    pub inject: Inject,
}
/// per-session git isolation + spans (was `iso_base`/`session_branch`/`span_tip`/`span_branches`).
#[derive(Default)]
pub struct GitIso {
    /// the resolved isolation base branch (set once at run start).
    pub iso_base: String,
    /// this session's branch (cut by INJECT, resolved by GATE).
    pub session_branch: Option<String>,
    /// the branch the NEXT session cuts off — the TIP of the staged span, or `None` = base (§5.7).
    pub span_tip: Option<String>,
    /// staged span branches accumulated by `skip_judges` steps (for reporting on abort).
    pub span_branches: Vec<String>,
}
/// the LLM summarizer's rolling window (was `cumulative`/`last_summary`).
#[derive(Default)]
pub struct Summary {
    pub cumulative: String,
    /// `None` until `Setup` primes it (on_run_start, before the loop) — reads treat `None` as due.
    pub last_summary: Option<Instant>,
}
/// institutional-memory tail (was `last_session`).
#[derive(Default)]
pub struct Memory {
    pub last_session: String,
}
/// worker-broken detection (was `dud_streak`).
#[derive(Default)]
pub struct WorkerHealth {
    pub dud_streak: u32,
}
/// operator steering carried to the next session (was `pending_instruction`).
#[derive(Default)]
pub struct Operator {
    pub pending_instruction: Option<String>,
}
/// compose-time inputs (was `prompt_prefix`/`state_before`).
#[derive(Default)]
pub struct Inject {
    /// `prompt_includes` fragments, composed once at launch.
    pub prompt_prefix: String,
    /// content of the step's state file at session start, to warn if the agent never touched it.
    pub state_before: Option<String>,
}

/// The per-session channel between stage-handlers, stored in the per-session `scratch` store and
/// `clear()`ed each session at the loop top so no field (esp. `prompt`) leaks across sessions. NOT
/// the on-disk memory scratch (`memory::clear_scratch`) — a different thing.
#[derive(Default)]
pub struct AGGScratch {
    /// `WriteInstructions` (on_session_start) → the RUN launch.
    pub prompt: Option<String>,
    /// `PickStep` sets it from `cur_step.skip_judges`; `run_hook`'s predicate bypasses a handler that
    /// opts out of skip steps (`runs_on_skip()==false`).
    pub skip_judges: bool,
    /// `LaunchWorker` (on_run) → VERIFY/GATE.
    pub outcome: Option<SessionOutcome>,
    /// `FloorFold` (on_verify) → the post-judge refine fold in GATE.
    pub mem_folded: bool,
    /// `SnapshotGoals` (on_verify) → a rollback in GATE restores it.
    pub pre_cycle_goals: Vec<GoalRuntime>,
    /// `StageSpan` (skip) XOR `RunJudges` (judged) → GATE; REWRITTEN by GATE on a rollback.
    pub res: Option<CycleResult>,
    /// `StageMerge` (judged) → GATE's keep/rollback. `None` on a skip step.
    pub staged: Option<(String, crate::git::StagedSession)>,
    /// `GateKeepRollback` (on_gate) → `RefineFold`'s "session ROLLED BACK" prefix. Staged-!keep only.
    pub rolled_back: bool,
    /// `Summarize` (on_session_end) → `RefineFold`'s mechanical+summary source choice.
    pub summarized_this_cycle: bool,
}
