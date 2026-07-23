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

use crate::bus::{Bus, Command};
use crate::core::config::AggConfig;
use crate::core::sequence::Cursor;
use crate::state::{DashboardState, LiveState, Phase};
use anyhow::Result;
use std::path::Path;
use std::time::{Duration, Instant};





/// The worker's ENTIRE pushed input. The full brief is composed into `agg/state/INSTRUCTIONS.md`
/// every session and the worker is pointed at it — this kills the argv size ceiling AND the
/// argv-parse fragility (a huge `-p` value that could start with `-`), and makes the exact context
/// the worker saw inspectable on disk. The path is RELATIVE so it resolves from the worker's cwd
/// (the project dir) on every backend (Claude/Copilot `-p`, Codex positional) with no per-backend
/// special-casing — it is just a short, dash-free prompt value.
pub const INSTRUCTIONS_POINTER: &str =
    "Read the file `agg/state/INSTRUCTIONS.md` in full and follow it — it is your complete brief for this session.";

/// The standing "Before you exit" footer of every session's brief (the wiki/OKF guidance lives here).
/// Kept as a real markdown file (`include_str!`'d, like the scaffolds + skills) rather than an inline
/// string; `{{STATE}}` is filled with the step's state path when composed.
pub const EXIT_FOOTER: &str = include_str!("../plugin/scaffold/exit_footer.md");

// ── the lifecycle registry (HOOK_REDESIGN §3.1/§5) ────────────────────────────────────────────
// Handlers are `.add()`ed to hook points in code and dispatched in order by the loop — the seed of
// "every task is a hook". agg's own tasks (pick/compose/judges/gate/memory) are handlers too; only
// the true scheduler control flow (over_max_sessions, worker_is_broken, the phase emits) stays core.
//
// The context a handler receives is the whole `LoopState` (§8: the context IS the run/session
// state). Handlers run STRICTLY SEQUENTIALLY, each with an exclusive `&mut LoopState`, so passing
// the whole state is legal with no borrow gymnastics. The `Lifecycle` is owned by `run()` and passed
// ALONGSIDE the state (never stored in it) — that disjointness is what keeps the borrow sound.




/// ALL of agg's own per-run feature state, organised by feature (LOOPSTATE_REDESIGN §3.1) — the
/// built-in "plugin", stored in the per-run `ext`. Persists across sessions (git span, summarizer
/// window, memory tail, worker health); NEVER cleared mid-run.
// agg's plugin state (AGGState/AGGScratch) lives in `features::state`; re-exported so the
// context + registry can name it. A third-party plugin defines its OWN state type the same way.
pub use crate::features::state::{AGGScratch, AGGState};
pub use crate::context::LoopState;
pub use crate::plugin::{Bootstrap, End, Extensions, Flow, Handler, LifecycleEvent, PreStart, RunOutcome};
pub use crate::registry::{run_hook, Lifecycle};
pub use crate::assembly::{assemble, Assembly};
use crate::registry::run_pre_start;





// ShellHook moved to `crate::features::shell::ShellHook`.

// ── on_session_start handlers = the old INJECT stage, decomposed (HOOK_REDESIGN §4) ──────────────






// LaunchWorker moved to `crate::features::run::LaunchWorker`.

// ── on_verify handlers = the old VERIFY stage, decomposed (HOOK_REDESIGN §4) ───────────────────────








// ── on_gate handlers = the old GATE keep/rollback (HOOK_REDESIGN §4) ───────────────────────────────



// ── on_session_end handlers = the old GATE tail (HOOK_REDESIGN §4) ─────────────────────────────────

// The LLM summarizer moved to `crate::features::summary::Summarize` — agg's first feature relocated
// out of the core as a plugin, reaching the core only through the public API.













pub fn wait_for_resume(bus: &Bus) -> Option<String> {
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

/// Owns the registered `on_stop` handlers and fires them on Drop — so the teardown hook runs on
/// EVERY exit (normal return, early return, or panic-unwind), which a loop-body dispatch can't
/// guarantee. `on_stop` is a registry hook like the rest; the Drop guard is just its dispatcher.
struct StopHooks {
    handlers: Vec<Box<dyn Handler>>,
}
impl Drop for StopHooks {
    fn drop(&mut self) {
        for h in &self.handlers {
            h.fire();
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



/// Drive the loop with agg's default plugin pipeline. The common entry point.
pub fn run(
    cfg: AggConfig,
    assembly: Assembly,
    dir: &Path,
    config_base: &Path,
    max_sessions_flag: u32,
) -> Result<RunOutcome> {
    run_with(cfg, assembly, dir, config_base, max_sessions_flag, |_| {})
}

/// Drive the loop with a chance to register EXTRA plugins on top of agg's own (HOOK_REDESIGN §5:
/// config-in-code registration, no yaml). `register` runs after `Lifecycle::default_pipeline` and
/// before the loop, so a host adds its `Handler`s to any hook exactly as agg registers its own —
/// agg's features and a third-party plugin ride the identical path.
pub fn run_with(
    cfg: AggConfig,
    assembly: Assembly,
    dir: &Path,
    config_base: &Path,
    max_sessions_flag: u32,
    register: impl FnOnce(&mut Lifecycle),
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

    // agg's built-in hook registration (HOOK_REDESIGN §5) — EVERY lifecycle point, no exceptions,
    // including the `pre_start` git preconditions below and on_start (now) / on_stop (Drop guard).
    let mut lifecycle = Lifecycle::default_pipeline(&cfg, dir);
    register(&mut lifecycle); // host/third-party plugins, added on top of agg's own (§5)

    // ── session isolation (MANDATORY): the git preconditions run as `pre_start` hooks — recover a
    //    stranded merge, require a clean git repo, ensure `agg/state` gitignored, resolve the base
    //    branch. Any bail is a hard error out of run(), exactly as the old inline block. ──
    let mut boot = Bootstrap { dir, cfg: &cfg, iso_base: None };
    run_pre_start(&lifecycle.pre_start, &mut boot)?;
    let iso_base = boot.iso_base.expect("ResolveIsoBase set iso_base");

    #[cfg(not(unix))]
    eprintln!("  ⚠ Windows: unix-first build — the CPU-flat watchdog and process-group spawn protection are NOT active here.");
    for h in &lifecycle.on_start {
        h.fire(); // fires at run-start, BEFORE the `Setup` feature gathers prompt_includes — order preserved.
    }
    // on_stop moves into the Drop guard so it fires on any exit incl. panic (its dispatcher).
    let _stop_hooks = StopHooks { handlers: std::mem::take(&mut lifecycle.on_stop) };

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
        ext: Extensions::default(),
        scratch: Extensions::default(),
    };
    st.ext.get::<AGGState>().git.iso_base = iso_base; // resolved base branch (was a named field)
    st.publish();
    st.dash.lifetime_session = lifetime_base;

    // ── run-start hooks: spawn the user's background watchers, then the baseline pass (§5.5.1) —
    //    both are registry handlers now (the `background` / `on_run_start` hooks). Baseline's two
    //    launch-time early exits come back as End::Stop and finish the run. ──
    run_hook(&lifecycle.background, &mut st)?;
    if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_run_start, &mut st)? {
        return Ok(outcome);
    }

    // (summary clock, memory scratch, and the operator bus were opened by the `Setup` feature on
    //  `on_run_start`, right after the baseline pass — the old inline block, now a registry hook.)

    // ── the deterministic outer loop, one step at a time ──
    loop {
        if let Some(outcome) = st.over_max_sessions() {
            return Ok(outcome);
        }
        // reset the per-session channel so no field (esp. `prompt`) leaks across sessions.
        st.scratch.clear();
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

pub fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
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


    /// The registry reads as HIGH-LEVEL FEATURES — one (or a couple) per lifecycle phase — so a human
    /// understands the loop's structure at a glance (Inject / Run / Verify / Gate / Finalize …). And
    /// EVERY lifecycle point is a registry hook: an empty list here means a lifecycle task escaped the
    /// registry. Each feature's internal step order is source-visible in `with_hooks` and dispatched
    /// verbatim by `run_hook` (grouping changed no behavior).
    #[test]
    fn the_registry_reads_as_high_level_features() {
        let l = Lifecycle::with_hooks(&crate::core::config::Hooks::default(), std::path::Path::new("."));
        let names = |hs: &[Box<dyn Handler>]| hs.iter().map(|h| h.name()).collect::<Vec<_>>();
        assert_eq!(names(&l.on_start), ["on_start"]);
        assert_eq!(names(&l.on_run_start), ["Baseline", "Setup"]);
        assert_eq!(names(&l.background), ["BackgroundSpawn"]);
        assert_eq!(names(&l.on_session_start), ["Inject"]);
        assert_eq!(names(&l.on_run), ["Run"]);
        assert_eq!(names(&l.on_verify), ["Verify"]);
        assert_eq!(names(&l.on_gate), ["Gate"]);
        assert_eq!(names(&l.on_session_end), ["Finalize"]);
        assert_eq!(names(&l.on_stop), ["on_stop"]);
        assert_eq!(l.pre_start.len(), 1); // GitSetup (the PreStart protocol carries no name())
    }

    /// The one non-trivial bit of the extension store is the type-keyed downcast — verify each type
    /// gets its OWN slot, values persist across `get`s, and `clear()` drops everything (per-session).
    #[test]
    fn extensions_is_one_slot_per_type() {
        #[derive(Default, PartialEq, Debug)]
        struct A(u32);
        #[derive(Default, PartialEq, Debug)]
        struct B(String);
        let mut ext = Extensions::default();
        ext.get::<A>().0 = 7;
        ext.get::<B>().0 = "x".into();
        assert_eq!(ext.get::<A>(), &A(7)); // persists + downcasts to its own type, not B's
        assert_eq!(ext.get::<B>(), &B("x".into()));
        ext.clear();
        assert_eq!(ext.get::<A>(), &A(0)); // clear() → default re-inserted (the per-session reset)
    }

}
