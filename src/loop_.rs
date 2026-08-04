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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};





/// The worker's ENTIRE pushed input. The full brief is composed into `agg/private/INSTRUCTIONS.md`
/// every session and the worker is pointed at it — this kills the argv size ceiling AND the
/// argv-parse fragility (a huge `-p` value that could start with `-`), and makes the exact context
/// the worker saw inspectable on disk. The path is RELATIVE so it resolves from the worker's cwd
/// (the project dir) on every backend (Claude/Copilot `-p`, Codex positional) with no per-backend
/// special-casing — it is just a short, dash-free prompt value.
pub const INSTRUCTIONS_POINTER: &str =
    "Read the file `agg/private/INSTRUCTIONS.md` in full and follow it — it is your complete brief for this session.";

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
pub(crate) struct StopHooks {
    pub(crate) handlers: Vec<Box<dyn Handler>>,
}
impl Drop for StopHooks {
    fn drop(&mut self) {
        for h in &self.handlers {
            h.fire();
        }
    }
}

pub(crate) struct RunPidGuard {
    pub(crate) dir: PathBuf,
}
impl Drop for RunPidGuard {
    fn drop(&mut self) {
        crate::os::detach::clear_run_pid(&self.dir);
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
                 (if you're sure it's dead, remove agg/private/run.pid and retry.)"
            );
        }
    }
    crate::os::detach::write_run_pid(dir);
    let _run_pid_guard = RunPidGuard { dir: dir.to_path_buf() };
    crate::os::signals::install();

    let ruler = cfg.ruler_backend()?;
    let judge_model = cfg.judge_model(ruler);
    let judge_timeout = cfg.judge.timeout;

    // agg's built-in hook registration (HOOK_REDESIGN §5) — EVERY lifecycle point, no exceptions,
    // including the `pre_start` git preconditions below and on_start (now) / on_stop (Drop guard).
    let mut lifecycle = Lifecycle::default_pipeline(&cfg, dir);
    register(&mut lifecycle); // host/third-party plugins, added on top of agg's own (§5)

    // ── session isolation (MANDATORY): the git preconditions run as `pre_start` hooks — recover a
    //    stranded merge, require a clean git repo, ensure `agg/{state,private}` gitignored, resolve the base
    //    branch. Any bail is a hard error out of run(), exactly as the old inline block. ──
    // `resume: false` — the YAML path has no resume, so a dirty tree still ends the run here.
    let mut boot = Bootstrap { dir, cfg: &cfg, resume: false, iso_base: None };
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
        cfg.project,
        eng.done_if.clone().unwrap_or_default()
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
    // read off `cfg` BEFORE it moves into the `LoopState` below (which now OWNS it, §3.1).
    let budget_total = cfg.sequence.limits.tokens;
    let cost_limit = cfg.sequence.limits.cost;
    let gate_regressions = cfg.sequence.gate_regressions;

    let dash = DashboardState {
        project: cfg.project.clone(),
        model: worker_model_display,
        stop_when: eng.done_if.clone().unwrap_or_default(),
        halt_when: eng.abort_if.clone().unwrap_or_default(),
        // RECORDED for the next run, not for a reader (§3.9 rule 1): a run that starts with HEAD
        // stranded on a crashed session branch recovers its real base from here.
        iso_base: iso_base.clone(),
        budget_total,
        cost_limit,
        phase: Phase::Starting,
        ..Default::default()
    };
    let live = LiveState::new(dir, loop_start, dash.clone());

    let ledger = crate::project::RunLedger::begin(dir, &cfg.project, std::process::id(), now_epoch());
    let lifetime_base = ledger.prior_lifetime_sessions();

    let mut st = LoopState {
        cfg,
        ruler,
        judge_model,
        judge_timeout,
        dir: dir.to_path_buf(),
        config_base: config_base.to_path_buf(),
        eng,
        cursor: Cursor::new(statements),
        cur_step: None,
        next_step: None, // the YAML path drives the cursor; only a Rust driver seeds this.
        dash,
        live,
        ledger,
        bus: None,
        budget_total,
        cost_limit,
        max_iter: if max_sessions == 0 { None } else { Some(max_sessions) },
        max_sessions,
        gate_regressions,
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
        // `over_max_sessions` is the SCHEDULER's ceiling, not the step's: it belongs to whoever owns
        // the iteration, so it stays here and out of `step_once`.
        if let Some(outcome) = st.over_max_sessions() {
            return Ok(outcome);
        }
        match step_once(&mut st, &lifecycle)?.end {
            Some(End::Stop(outcome)) => return Ok(outcome),
            Some(End::NextSession) => continue,
            None => {}
        }
    }
}

/// ONE iteration of the loop body — the whole hook dispatch for a single step, in order.
///
/// It exists as a named fn because the Rust driver API replaces exactly this and nothing else: a
/// driver's `step()` must be the SAME dispatch the YAML path runs, or every feature that hangs off
/// these hooks (git isolation, keep/rollback, memory, notify, accounting, the watchdog) would have to
/// be re-plumbed correctly in a second place and would rot out of sync.
///
/// The decision of what an `End` MEANS belongs to the caller, so both are returned rather than acted
/// on. ⚠ The per-hook asymmetry is deliberate and load-bearing: `on_gate` and `on_session_end` honour
/// only `End::Stop` and DROP a `NextSession`, because at those points the session's remaining work
/// (Finalize) must still run — a uniform "NextSession ⇒ next lap" would skip it. That asymmetry is
/// resolved HERE, so no caller can get it wrong.
pub(crate) fn step_once(st: &mut LoopState, lc: &Lifecycle) -> Result<StepEnd> {
    // reset the per-session channel so no field (esp. `prompt`) leaks across sessions.
    st.scratch.clear();
    st.emit(LifecycleEvent::Inject);
    if let Some(end) = run_hook(&lc.on_session_start, st)? {
        return Ok(StepEnd::ended(end));
    }
    st.emit(LifecycleEvent::Run);
    // SIGINT → finish_interrupted → Stopped
    if let Some(end) = run_hook(&lc.on_run, st)? {
        return Ok(StepEnd::ended(end));
    }
    if let Some(e) = st.worker_is_broken() {
        return Err(e);
    }
    // rate-limited: incomplete session — go round again. Ceiling tripped during backoff → Halt.
    if let Some(end) = run_hook(&lc.on_verify, st)? {
        return Ok(StepEnd::ended(end));
    }
    // ⚠ The outcome is snapshotted HERE, while the values still exist: `GateKeepRollback`
    // `take()`s `scratch.staged` and `CheckRunStop` `take()`s `scratch.res`, and `landed` cannot be
    // reconstructed from what is left (BUILD.md §3.4 item 6).
    let staged = StepStaging::capture(st);
    // GATE keep/rollback → poison-pill Halt short-circuits here (CeilingPoisonGuard).
    if let Some(End::Stop(outcome)) = run_hook(&lc.on_gate, st)? {
        return Ok(StepEnd::ended(End::Stop(outcome)));
    }
    // session-end work (shell hook, summary, memory fold) then the run-stop check (CheckRunStop).
    if let Some(End::Stop(outcome)) = run_hook(&lc.on_session_end, st)? {
        return Ok(StepEnd { end: Some(End::Stop(outcome)), outcome: Some(staged.finish(st)) });
    }
    Ok(StepEnd { end: None, outcome: Some(staged.finish(st)) })
}

/// What one [`step_once`] produced: the loop-control answer, and — when the step got far enough to
/// have one — what it DID.
///
/// The `outcome` exists for the Rust driver (`agg.step(&s)?` returns it). The YAML path ignores it;
/// it is built regardless because building it costs a handful of field copies and making it
/// conditional would mean a second code path through the one function this design says there is
/// only one of.
pub(crate) struct StepEnd {
    pub end: Option<End>,
    /// `None` when the step never reached VERIFY — an early `Stop`/`NextSession` means nothing
    /// landed anywhere and there is no honest `Landing` to report.
    pub outcome: Option<crate::driver::StepOutcome>,
}

impl StepEnd {
    fn ended(end: End) -> StepEnd {
        StepEnd { end: Some(end), outcome: None }
    }
}

/// The per-step values that do not survive the GATE, captured between VERIFY and GATE.
struct StepStaging {
    step: String,
    session: u32,
    verdicts: Vec<(String, crate::core::model::Verdict)>,
    tokens: u64,
    cost: f64,
    secs: u64,
    exit: i32,
    staged: Option<(String, crate::git::StagedSession)>,
    on_span: bool,
}

impl StepStaging {
    fn capture(st: &mut LoopState) -> StepStaging {
        let (verdicts, staged, secs, exit) = {
            let sc = st.scratch.get::<AGGScratch>();
            let (secs, exit) = sc
                .outcome
                .as_ref()
                .map(|o| (o.duration_secs, o.exit_code.unwrap_or(-1)))
                .unwrap_or((0, -1));
            (sc.res.as_ref().map(|r| r.fresh_verdicts.clone()).unwrap_or_default(), sc.staged.clone(), secs, exit)
        };
        StepStaging {
            step: st.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
            session: st.session,
            verdicts,
            tokens: st.tokens_spent,
            cost: st.cost_spent,
            secs,
            exit,
            staged,
            on_span: st.ext.get::<AGGState>().git.span_tip.is_some(),
        }
    }

    fn finish(self, st: &mut LoopState) -> crate::driver::StepOutcome {
        use crate::driver::Landing;
        let rolled_back = st.scratch.get::<AGGScratch>().rolled_back;
        let landed = match &self.staged {
            // a judged step whose merge was staged: the gate either kept it (it is on base now) or
            // discarded it.
            Some((_, crate::git::StagedSession::Staged)) if !rolled_back => Landing::Base,
            Some((_, crate::git::StagedSession::Staged)) => Landing::RolledBack,
            // Vetoed / NoChanges / Conflict / CheckoutFailed — nothing merged, and the `_` arm of
            // `GateKeepRollback` restored base truth. No work landed anywhere.
            Some(_) => Landing::Nothing,
            // no merge was staged: the driver/`skip_judges` path. The span tip says whether this
            // session's branch joined the open span or was discarded (a red_file veto).
            None if self.on_span => Landing::Span,
            None => Landing::Nothing,
        };
        crate::driver::StepOutcome {
            step: self.step,
            session: self.session,
            landed,
            verdicts: self.verdicts,
            tokens: self.tokens,
            cost: self.cost,
            secs: self.secs,
            exit: self.exit,
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
        // derived from `paths`, not restated: the pointer is a LITERAL (a const can't call a fn), so
        // this is the only thing standing between a layout move and a worker sent to a missing brief.
        let rel = crate::paths::instructions_md(std::path::Path::new(""));
        assert!(
            INSTRUCTIONS_POINTER.contains(&*rel.to_string_lossy()),
            "pointer must name the file agg actually writes ({})",
            rel.display()
        );
    }


    /// The registry reads as HIGH-LEVEL FEATURES — one (or a couple) per lifecycle phase — so a human
    /// understands the loop's structure at a glance (Inject / Run / Verify / Gate / Finalize …). And
    /// EVERY lifecycle point is a registry hook: an empty list here means a lifecycle task escaped the
    /// registry. Each feature's internal step order is source-visible in `with_hooks` and dispatched
    /// verbatim by `run_hook` (grouping changed no behavior).
    #[test]
    fn the_registry_reads_as_high_level_features() {
        let l = Lifecycle::with_hooks(&crate::core::config::Hooks::default(), std::path::Path::new("."), crate::isolation::Isolation::None);
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
