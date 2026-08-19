//! The reusable plugin framework — the types a plugin author imports.

use crate::context::LoopState;
use crate::core::config::AggConfig;
use crate::state::Phase;
use anyhow::Result;
use std::path::Path;

/// How the loop ended — mapped to a process exit code in `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// `done_if` (the Definition of Done) fired, or was already satisfied at launch.
    GoalsMet,
    /// `abort_if` fired (invariant regressed, budget/cost/iteration/wall ceiling) — NOT success.
    Halt,
    /// The `--max-sessions` cap was reached with the DoD not met.
    MaxSessions,
    /// The operator stopped the run — `agg stop`, `agg send stop`, or Ctrl-C.
    ///
    /// ⚠ This used to share exit **0** with [`RunOutcome::GoalsMet`], which made an abandoned run
    /// indistinguishable from a met goal: `if agg run; then ship; fi` shipped on `agg stop`. It is
    /// its own code (5) since the HiL work, where "a human ended it" is an everyday outcome rather
    /// than a corner case. BREAKING for any wrapper that treated a stop as success — deliberately.
    Stopped,
}

impl RunOutcome {
    pub fn exit_code(self) -> u8 {
        match self {
            RunOutcome::GoalsMet => 0,
            RunOutcome::Halt => 3,
            RunOutcome::MaxSessions => 4,
            RunOutcome::Stopped => 5,
        }
    }
}

/// Something the loop DID, at the moment it did it — the single source of truth for `dash.phase`.
pub enum LifecycleEvent {
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
    pub fn phase(&self) -> Phase {
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

/// A handler's control-flow result (§3.1). MINIMAL: reason/ledger_tag are NOT here — every Stop
/// path already `emit`s `Finished{reason,ledger_tag}` itself before yielding the outcome, so a
/// handler emits then returns `Flow::Stop(outcome)` and the core never re-emits.
pub enum Flow {
    Continue,
    /// stop the rest of THIS session's hooks, loop to the next session (the rate-limit path).
    SkipSession,
    Stop(RunOutcome),
}

/// What a whole hook-point dispatch produced (`None` = drained cleanly, fall through to next hook).
pub enum End {
    NextSession,
    Stop(RunOutcome),
}

/// A type-keyed bag: one value per Rust type (anymap-style). The loop is single-threaded, so no
/// Send/Sync needed. This is the generic extension store (LOOPSTATE_REDESIGN §3): agg's own features
/// use it via `AGGState`/`AGGScratch`, and a third-party plugin stashes ITS OWN type the same way —
/// `ctx.ext.get::<FooState>()` — without ever editing the core struct.
#[derive(Default)]
pub struct Extensions {
    map: std::collections::HashMap<std::any::TypeId, Box<dyn std::any::Any>>,
}
impl Extensions {
    /// Get-or-insert-default this type's slot, typed. How a feature/plugin reads+writes its own state.
    pub fn get<T: Default + 'static>(&mut self) -> &mut T {
        self.map
            .entry(std::any::TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("TypeId keys its own type")
    }
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

pub trait Handler {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow>;
    /// Whether this handler runs on a `skip_judges` step. Default yes; the judged-merge handlers
    /// override to `false` so a skip step bypasses them (mirrors the old `if !skip`).
    fn runs_on_skip(&self) -> bool {
        true
    }
    /// Fire a CONTEXT-FREE (run-level) hook — `on_start` before the loop state exists, `on_stop` at
    /// teardown. Default no-op; `ShellHook` overrides. This is what lets `on_start`/`on_stop` live in
    /// the SAME registry as every other hook without needing `&mut LoopState`.
    fn fire(&self) {}
    /// Stable name for the registration-order characterization tests (order is outcome-invisible).
    #[cfg_attr(not(test), allow(dead_code))]
    fn name(&self) -> &'static str;
}

/// A `pre_start` handler: agg's run-start git preconditions (recover a stranded merge, require a
/// clean git repo, ensure `agg/{state,private}` gitignored, resolve the isolation base branch). Runs before the
/// loop state exists, so it takes `Bootstrap`, not `LoopState`.
pub trait PreStart {
    fn run(&self, boot: &mut Bootstrap) -> Result<()>;
}

/// Bootstrap context for the `pre_start` hook — the ONE phase that runs before `LoopState` exists.
/// Handlers read `dir`/`cfg`, may `bail!` (a hard error out of `run()`, exactly as the old inline
/// checks did), and `ResolveIsoBase` writes `iso_base` for the constructor to read. This is a second,
/// minimal handler protocol for state-BUILDING (vs `Handler`, which operates on the built state).
pub struct Bootstrap<'a> {
    pub dir: &'a Path,
    pub cfg: &'a AggConfig,
    /// this run CONTINUES a previous one (`Opts { resume: true }`). It changes exactly one
    /// precondition: a dirty tree is DISCARDED loudly instead of refusing to start (BUILD.md §3.9
    /// rule 2). A power cut mid-worker leaves uncommitted tracked changes by construction —
    /// `GitAutoCommit` only commits *after* a session — so refusing would make the flagship resume
    /// scenario the one scenario that cannot start. Always `false` on the YAML path, which has no
    /// resume.
    pub resume: bool,
    pub iso_base: Option<String>,
}
