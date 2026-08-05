//! [`Agg`] — the facade a Rust driver holds, and the eleven calls it makes on it.
//!
//! # The load-bearing premise
//!
//! `agg.step(&s)?` **IS** one iteration of `agg run`'s loop body — the same hook dispatch, in the
//! same order, hooks and all. It calls [`crate::loop_::step_once`]; it does not re-implement a loop.
//! Any other answer would mean re-plumbing git session isolation, keep/rollback, memory, the
//! summariser, notify, rate-limit backoff, per-agent accounting, `verdicts.jsonl`, `state.json`, the
//! watchdog and the isolation tiers a second time, correctly, forever.
//!
//! # Why every call takes `&self`
//!
//! A driver holds ONE binding and calls it from inside its own `for`/`if`. `&mut self` would force
//! the borrow checker into the driver's control flow for no gain: the loop is single-threaded, so
//! the interior mutability below (`Cell`/`RefCell`/`OnceCell`) is free and invisible.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::core::config::{AggConfig, CustomBackend, Limits, ResolvedStep};
use crate::core::engine::GoalRuntime;
use crate::core::model::{Judge, JudgeKind, JudgeSource, Verdict};
use crate::driver::{Agent, Fatal, GateFailure, GateOutcome, OnRegression, Opts, Step, StepOutcome};
use crate::git::StagedSession;
use crate::loop_::{
    run_hook, AGGState, End, Flow, Handler, Lifecycle, LoopState, RunPidGuard, RunOutcome, StopHooks,
};

/// How long `block()` naps between re-evaluating the ceilings. The same 5s chunk
/// `RateLimitBackoff` uses, for the same reason: a long wait must stay interruptible and must not
/// outlive the run's own ceilings.
const BLOCK_CHUNK_SECS: u64 = 5;

/// The Rust driver's handle on a run.
///
/// Built by [`Agg::open`], configured by the self-consuming builder chain
/// ([`Agg::limits`] / [`Agg::on_regression`] / [`Agg::instructions`]), then used through `&self`.
///
/// ⛔ **A stray `agg.yaml` in the project is IGNORED** — not merged, not a fallback.
/// [`AggConfig::load`] is never called on this path. The two entry points share FILES
/// (`agg/judges/`, the instructions file, `agg/state/`), never configuration.
///
/// Fields are private, against this crate's no-facade convention, because `Agg` *is* the facade and
/// its invariants are not expressible as field types: `started` must be filled exactly once and
/// only after the chain, and `cfg` must stop being writable the moment it has been.
pub struct Agg {
    cfg: AggConfig,
    dir: PathBuf,
    /// this run CONTINUES a previous one. Today it reaches exactly one decision — `GitSetup`
    /// discards a crashed session's dirty tree instead of refusing to start (§3.9 rule 2).
    resume: bool,
    /// the deferred run-level setup. `None` until the first spending call.
    started: OnceCell<Started>,
    /// THE LATCH. Once set, every call is a no-op that spends nothing (BUILD.md §3.3).
    ended: Cell<Option<RunOutcome>>,
    /// the ledger cursor (Phase 1).
    ord: Cell<u64>,
    /// the per-STEP lazy verdict cache — what makes `agg.judge(&x)` memoized and `&&` a gate.
    verdicts: RefCell<HashMap<String, Verdict>>,
    /// every verdict consulted since the last `gate()`; `gate()` reads and clears it.
    span: RefCell<HashMap<String, Verdict>>,
    /// the engine's goal truth as of the moment the open span began — what a rollback restores.
    ///
    /// On this path it is provably EMPTY (§3.6 item 3: the run-set is empty, so the engine has no
    /// goals), and it is carried anyway so the shared keep/rollback body gets the same argument from
    /// both callers rather than a hard-coded `&[]` that would quietly rot if a driver ever grew one.
    span_goals: RefCell<Vec<GoalRuntime>>,
    /// the recorded calls a `--resume` fast-forwards through.
    ///
    /// ponytail: `core::calls` is Phase 1, so this is always empty and `Opts { resume: true }`
    /// currently only records the intent. CEILING: a resumed driver re-executes everything.
    /// UPGRADE PATH: `Vec<CallRecord>` and the fast-forward arms in `step`/`judge`/`gate`.
    #[allow(dead_code)]
    replay: Vec<()>,
    /// the label path — see [`Agg::pos`].
    pos: RefCell<Vec<PosItem>>,
}

/// Everything that only exists once the run-level setup has run.
///
/// A `OnceCell` is what makes the builder-shaped `open()` honest: before the first spending call
/// there IS no `LoopState`, no `Lifecycle` and no guards, and the struct says so instead of
/// declaring always-present fields that cannot be constructed until the chain is complete.
struct Started {
    st: RefCell<LoopState>,
    lc: Lifecycle,
    /// the Drop guards live HERE, so dropping the `Agg` fires them on return, early return AND
    /// panic-unwind.
    _stop: StopHooks,
    _pid: RunPidGuard,
}

// ---------------------------------------------------------------------------------------------
// construction + the builder chain
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// Open `dir` as a driver project. Validates the directory and nothing else — the config is
    /// filled by the chain that follows, and the run-level setup is deferred to the first
    /// `step()`/`judge()`/`gate()`/`check_limits()`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Agg, Fatal> {
        Agg::open_with(dir, Opts::default())
    }

    /// [`Agg::open`] with the run-level options that must be known before any config is applied.
    pub fn open_with(dir: impl AsRef<Path>, opts: Opts) -> Result<Agg, Fatal> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Err(Fatal::Other(anyhow::anyhow!(
                "`{}` is not a directory — Agg::open takes the PROJECT ROOT",
                dir.display()
            )));
        }
        // canonicalize so the sandbox carve-out, the git commands and the label paths all agree on
        // one spelling of the project root.
        let dir = std::fs::canonicalize(dir).map_err(Fatal::Io)?;
        // The project NAME is what branch names, the run ledger and the dashboard are keyed on.
        // There is no `.project()` builder call: the directory already names the project, and a
        // second spelling is a second thing to get wrong.
        let cfg = AggConfig {
            project: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "driver".into()),
            ..AggConfig::default()
        };
        if opts.resume {
            eprintln!(
                "  ⚠ `Opts.resume`: the CALL LEDGER is Phase 1, so this run still starts from the top.\n\
                 \x20   What it does today is tolerate the state a crash leaves — a dirty work tree is\n\
                 \x20   discarded rather than refused (§3.9 rule 2)."
            );
        }
        Ok(Agg {
            cfg,
            dir,
            resume: opts.resume,
            started: OnceCell::new(),
            ended: Cell::new(None),
            ord: Cell::new(0),
            verdicts: RefCell::new(HashMap::new()),
            span: RefCell::new(HashMap::new()),
            span_goals: RefCell::new(Vec::new()),
            replay: Vec::new(),
            pos: RefCell::new(Vec::new()),
        })
    }

    /// The run-level ceilings. **Opt-in**: they are enforced where the driver calls
    /// [`Agg::check_limits`], and nowhere else. A driver that never calls it has no ceilings.
    pub fn limits(mut self, limits: Limits) -> Agg {
        self.cfg.sequence.limits = limits;
        self
    }

    /// What a regression across a span MEANS to this project — the policy `gate()` applies.
    pub fn on_regression(mut self, policy: OnRegression) -> Agg {
        self.cfg.sequence.gate_regressions = policy == OnRegression::Rollback;
        self
    }

    /// The standing project-instructions file the worker's brief POINTS AT, relative to the project
    /// dir. Default `agg/AGG.md`.
    ///
    /// agg never reads its bytes — it checks that the file exists and emits "Read `<path>`" into the
    /// brief. A missing file produces no pointer at all.
    pub fn instructions(mut self, rel: impl Into<String>) -> Agg {
        self.cfg.instructions = rel.into();
        self
    }
}

// ---------------------------------------------------------------------------------------------
// the deferred run-level setup
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// The started run, performing the run-level setup on first use.
    ///
    /// Both latch checks are needed: one for a run already ended, one for a setup that ended it —
    /// the `Baseline` pass can stop the run at launch, exactly as it does on the YAML path.
    fn ready(&self) -> Result<&Started, Fatal> {
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        if self.started.get().is_none() {
            let started = self.boot()?;
            let _ = self.started.set(started);
        }
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        Ok(self.started.get().expect("just set"))
    }

    /// `loop_::run_with`'s whole pre-loop, in the same order (BUILD.md §3.2).
    ///
    /// It is NOT in `open()` because at `open()` time the config is empty: the builder chain has not
    /// run yet, so `assemble`, `capability::check` and the banner would all read placeholder values.
    ///
    /// `run_with`'s `register` host-plugin seam is deliberately not exposed — a driver IS the host
    /// program. Re-add it the first time one wants a third-party handler.
    fn boot(&self) -> Result<Started, Fatal> {
        let cfg = self.cfg.clone();
        let dir = self.dir.clone();
        let config_base = crate::paths::config_base(&dir);

        // the driver variant: an empty `sequence.steps` means no entry list to validate, no
        // `done_if`, and an EMPTY run-set — judges are lazy and the driver asks.
        let asm = crate::assembly::assemble(&cfg, &config_base)?;
        crate::capability::check(&cfg, &asm.engine.judges)?;
        for name in cfg.agent_names() {
            crate::backend::for_name(&name)?.preflight()?;
        }

        if let Some(pid) = crate::os::detach::live_pid(&dir) {
            if pid != std::process::id() {
                return Err(Fatal::Other(anyhow::anyhow!(
                    "a loop is already running in this project (pid {pid}).\n  \
                     watch it:   agg dashboard\n  stop it:    agg stop\n  \
                     (if you're sure it's dead, remove agg/private/run.pid and retry.)"
                )));
            }
        }
        crate::os::detach::write_run_pid(&dir);
        let pid_guard = RunPidGuard { dir: dir.clone() };
        crate::os::signals::install();

        let ruler = cfg.ruler_backend()?;
        let judge_model = cfg.judge_model(ruler);
        let judge_timeout = cfg.judge.timeout;

        let mut lifecycle = Lifecycle::default_pipeline(&cfg, &dir);
        let mut boot =
            crate::plugin::Bootstrap { dir: &dir, cfg: &cfg, resume: self.resume, iso_base: None };
        crate::registry::run_pre_start(&lifecycle.pre_start, &mut boot)?;
        let iso_base = boot.iso_base.expect("ResolveIsoBase set iso_base");

        #[cfg(not(unix))]
        eprintln!("  ⚠ Windows: unix-first build — the CPU-flat watchdog and process-group spawn protection are NOT active here.");
        for h in &lifecycle.on_start {
            h.fire();
        }
        let stop_hooks = StopHooks { handlers: std::mem::take(&mut lifecycle.on_stop) };

        let loop_start = std::time::Instant::now();
        eprintln!(
            "════════════════════════════════════════════════════════════\n\
             AgenticGoGo — project {} (rust driver)\n\
             ════════════════════════════════════════════════════════════\n\
             ▶ watch live:  run `agg dashboard` in another terminal\n\
             ⏹ stop anytime: `agg stop`",
            cfg.project
        );

        let limits = cfg.sequence.limits.clone();
        let gate_regressions = cfg.sequence.gate_regressions;
        let worker_model_display = cfg
            .defaults
            .model
            .clone()
            .unwrap_or_else(|| cfg.worker_backend().map(|b| b.default_model().to_string()).unwrap_or_default());
        let dash = crate::state::DashboardState {
            project: cfg.project.clone(),
            model: worker_model_display,
            budget_total: limits.tokens,
            cost_limit: limits.cost,
            phase: crate::state::Phase::Starting,
            // RECORDED for the next run, not for a reader (§3.9 rule 1): a run that starts with HEAD
            // stranded on a crashed session branch recovers its real base from here.
            iso_base: iso_base.clone(),
            ..Default::default()
        };
        let live = crate::state::LiveState::new(&dir, loop_start, dash.clone());
        let ledger =
            crate::project::RunLedger::begin(&dir, &cfg.project, std::process::id(), crate::util::now_epoch());
        let lifetime_base = ledger.prior_lifetime_sessions();

        let mut st = LoopState {
            cfg,
            ruler,
            judge_model,
            judge_timeout,
            dir: dir.clone(),
            config_base,
            eng: asm.engine,
            cursor: crate::core::walk::Walk::new(asm.steps),
            cur_step: None,
            next_step: None,
            dash,
            live,
            ledger,
            bus: None,
            budget_total: limits.tokens,
            cost_limit: limits.cost,
            // ⛔ `max_sessions` stays 0 (= the loop's "unlimited" sentinel) even when
            // `limits.sessions` is set: `over_max_sessions` is the SCHEDULER's ceiling and the
            // facade has no scheduler. All four limits are opt-in TOGETHER, through
            // `check_limits()` — enforcing this one anyway would make the surface incoherent.
            max_iter: limits.sessions,
            max_sessions: 0,
            gate_regressions,
            loop_start,
            lifetime_base,
            session: 0,
            tokens_spent: 0,
            cost_spent: 0.0,
            per_agent: std::collections::BTreeMap::new(),
            ext: crate::plugin::Extensions::default(),
            scratch: crate::plugin::Extensions::default(),
        };
        st.ext.get::<AGGState>().git.iso_base = iso_base;
        st.publish();
        st.dash.lifetime_session = lifetime_base;
        // the first span opens here, against the engine's launch truth.
        *self.span_goals.borrow_mut() = st.eng.snapshot_goal_state();

        run_hook(&lifecycle.background, &mut st)?;
        // the `Baseline` pass can stop the run at launch (abort_if already true / done_if already
        // satisfied). It finalizes dash + ledger itself, so the facade only latches the outcome.
        if let Some(End::Stop(outcome)) = run_hook(&lifecycle.on_run_start, &mut st)? {
            self.ended.set(Some(outcome));
        }

        Ok(Started { st: RefCell::new(st), lc: lifecycle, _stop: stop_hooks, _pid: pid_guard })
    }
}

// ---------------------------------------------------------------------------------------------
// step
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// Run ONE worker session for `step` — the same hook dispatch a YAML lap runs.
    ///
    /// The work is COMMITTED on the session branch and STAGED on the open span; nothing merges
    /// until `gate()` says so.
    pub fn step(&self, step: &Step) -> Result<StepOutcome, Fatal> {
        // 1. the latch, before anything else.
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        // 2. a fresh step is a fresh world: this is what makes memoization PER-STEP.
        self.verdicts.borrow_mut().clear();
        // Phase 1: a recorded step fast-forwards here, assigning session/tokens/cost from the
        // record and consuming one ordinal, with no work done.
        let body = self.resolve(step)?;
        let started = self.ready()?;

        let mut st = started.st.borrow_mut();
        loop {
            // ⚠ RE-SEEDED INSIDE THE LOOP. `PickStep` `take()`s `next_step`, so a `NextSession`
            // retry (the rate-limit path) would otherwise find `None` and fall through to the
            // sequence cursor — which on a driver project is empty.
            st.next_step = Some(body.clone());
            let end = crate::loop_::step_once(&mut st, &started.lc)?;
            match end.end {
                Some(End::NextSession) => continue,
                Some(End::Stop(outcome)) => {
                    drop(st);
                    self.ended.set(Some(outcome));
                    return Err(Fatal::Ended(outcome));
                }
                None => {
                    self.ord.set(self.ord.get() + 1); // Phase 1: …and append the record.
                    return Ok(end.outcome.expect("a step that ran to the end reports its outcome"));
                }
            }
        }
    }

    /// A driver [`Step`] as the pipeline's [`ResolvedStep`], with the run's defaults filled in.
    fn resolve(&self, step: &Step) -> Result<ResolvedStep, Fatal> {
        let Some(name) = step.name.clone() else {
            return Err(Fatal::Other(anyhow::anyhow!(
                "agg.step() was handed a TEMPLATE (an unnamed Step). Name it first: \
                 `template.create(\"implement\")`"
            )));
        };
        Ok(ResolvedStep {
            name,
            agent: step.agent.name().to_string(),
            model: step.model.clone(),
            effort: step.effort.as_str().map(String::from),
            worker_args: self.cfg.defaults.worker_args.clone(),
            state: step.state.clone().unwrap_or_else(|| self.cfg.defaults.state.clone()),
            role_prompt: self.cfg.defaults.role_prompt.clone(),
            prompt: step.prompt.clone(),
            // ⛔ ALWAYS. `step()` STAGES and `gate()` decides, so the driver path takes today's
            // `skip_judges` route unconditionally: `StageSpan` keeps the branch and extends the
            // span, while `StageMerge`/`RunJudges`/`GateKeepRollback` become no-ops. There is no
            // run-set to run here — judges are lazy and the driver asks.
            skip_judges: true,
            isolation: step.isolation,
            image: self
                .cfg
                .defaults
                .image
                .clone()
                .unwrap_or_else(|| crate::isolation::DEFAULT_IMAGE.to_string()),
            readonly: step.readonly.clone(),
            writable: step.writable.clone(),
            custom: match &step.agent {
                Agent::Custom(b) => Some(CustomBackend(b.clone())),
                _ => None,
            },
        })
    }
}

// ---------------------------------------------------------------------------------------------
// gate — close the span, apply the policy
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// CLOSE the open span: merge everything `step()` staged since the last gate into the base
    /// branch, or discard it, according to [`OnRegression`].
    ///
    /// This is where the design's split lands. `step()` says WHEN work is produced; `gate()` says
    /// when it may LAND; and the project's `on_regression` policy says what a regression means. A
    /// driver that never gates loses nothing — every session is committed on its own branch and the
    /// span tip holds them all — but base is never touched either.
    ///
    /// # What a regression is
    ///
    /// Every judge asked since the last gate that is now `!met`, did not error, and whose last
    /// LANDED verdict said met. **That is the whole rule** — no exclusion list, no opt-in marker. It
    /// is sound only because of the one convention this path depends on: **a judge's `met` means
    /// GOOD** (BUILD.md §0.2 rule 3), so met→unmet always means worse. An inverted detector
    /// (met-when-bad) must be inverted before it is used as a driver judge.
    ///
    /// Because judges are LAZY, the verdicts that count are the ones the driver actually asked for —
    /// including one asked for the first time long after the steps it judges. A gate inside `step()`
    /// could not see those, which is the reason `gate()` is a separate call at all.
    pub fn gate(&self) -> Result<GateOutcome, Fatal> {
        // 1. the latch, before anything else.
        if let Some(o) = self.ended.get() {
            return Err(Fatal::Ended(o));
        }
        // 2. Phase 1: a recorded gate fast-forwards to its recorded GateOutcome here.
        let started = self.ready()?;
        let mut st = started.st.borrow_mut();

        // 3. nothing staged ⇒ nothing to decide. No ref is touched, no record is appended and no
        //    ordinal is consumed — a gate on an empty span must be free to call unconditionally at
        //    the bottom of a driver's `for`.
        let Some(tip) = st.ext.get::<AGGState>().git.span_tip.clone() else {
            return Ok(GateOutcome::Nothing);
        };

        // 4. FIVE results, and four of them are not `Staged`.
        let iso_base = st.ext.get::<AGGState>().git.iso_base.clone();
        let red_file = st.cfg.session_isolation.red_file.clone();
        let staged = crate::git::stage_session(&st.dir, &iso_base, &tip, &red_file);

        // ⚠ `Conflict` and `CheckoutFailed` KEEP the span, so they return HERE, above the shared
        // body (which closes the span unconditionally). git aborted and left base untouched with
        // the tip in place: the work is still there and still gateable once the operator resolves
        // it. Reporting it as `RolledBack` would be a lie twice over — nothing was discarded, and
        // no policy decided anything.
        let failure = match staged {
            StagedSession::Conflict => Some(GateFailure::Conflict),
            StagedSession::CheckoutFailed => Some(GateFailure::CheckoutFailed),
            _ => None,
        };
        if let Some(f) = failure {
            self.ord.set(self.ord.get() + 1); // Phase 1: …and append the CallRecord::Gate.
            return Ok(GateOutcome::Failed(f));
        }

        // 5. the regression set: every judge asked since the last gate (§0.2 rule 3 is what makes
        //    an unscoped rule safe here). Sorted so `verdicts.jsonl` does not inherit a HashMap's
        //    iteration order.
        let mut fresh: Vec<(String, Verdict)> =
            self.span.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        fresh.sort_by(|a, b| a.0.cmp(&b.0));
        let landed = crate::core::verdicts::landed_met(&st.dir);
        let regressed = fresh
            .iter()
            .any(|(id, v)| v.error.is_none() && !v.met && landed.get(id).copied().unwrap_or(false));

        // 6/7/8 — the policy, the merge, the `verdicts.jsonl` row and the span teardown, all in the
        // body the YAML path's `GateKeepRollback` runs. One decision, two call sites.
        let pre_goals = std::mem::take(&mut *self.span_goals.borrow_mut());
        let gated = crate::features::gate::keep_or_rollback(
            &mut st,
            Some(&(tip, staged.clone())),
            &fresh,
            regressed,
            &pre_goals,
        )?;

        let outcome = match (&staged, gated) {
            (StagedSession::Staged, crate::features::gate::Gated::Merged) => GateOutcome::Kept,
            (StagedSession::Staged, _) => GateOutcome::RolledBack,
            // the worker vetoed: git already deleted the tip and the span is closed.
            (StagedSession::Vetoed, _) => GateOutcome::Failed(GateFailure::Vetoed),
            // no commits in the whole span — the tip is deleted and base is exactly as it was.
            _ => GateOutcome::Nothing,
        };

        // 9. the next span opens now, and it opens against the tree this gate just decided on.
        *self.span_goals.borrow_mut() = st.eng.snapshot_goal_state();
        drop(st);
        self.span.borrow_mut().clear();
        // ⚠ and the PER-STEP cache too. A gate is a state discontinuity inside a step window: after
        // a rollback the tree is base again while a memoized verdict still describes the span that
        // was just discarded, and the driver's next `if` would read it as current.
        self.verdicts.borrow_mut().clear();
        self.ord.set(self.ord.get() + 1); // Phase 1: …and append the CallRecord::Gate.
        Ok(outcome)
    }
}

// ---------------------------------------------------------------------------------------------
// judge — lazy, memoized per step
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// This step's verdict for `j`, running it if this step has not asked for it yet.
    ///
    /// Memoized per step, which is what makes `&&` a cost gate:
    /// `agg.judge(&build).met() && agg.judge(&load_test).met()` never reaches the 40-minute load
    /// test on a cycle where the build is red.
    ///
    /// ⚠ Returns a [`Verdict`], not a `Result` — deliberately. `Result` is `#[must_use]`, so
    /// `if agg.judge(&x).met()` would become `if agg.judge(&x)?.met()` at every site in every
    /// driver. After the latch this returns a NOT-MET verdict naming the outcome; never a
    /// fabricated `met: true`.
    pub fn judge(&self, j: &Judge) -> Verdict {
        if let Some(o) = self.ended.get() {
            return Verdict::binary(false)
                .with_rationale(format!("run ended ({o:?}) — judge `{}` was not run", j.name));
        }
        let lazy = Lazy { agg: self, asking: RefCell::new(Vec::new()) };
        lazy.verdict_for(j)
    }

    /// Run one judge for real: resolve its paths against the project, dispatch it, and charge its
    /// ruler spend to the run exactly as `RunJudges` does on the YAML path.
    ///
    /// `src` is threaded through so a NATIVE judge's `JudgeCtx` consults the same lazy cache — the
    /// `LoopState` borrow is deliberately DROPPED before the judge runs, so a nested ask cannot
    /// panic on a double borrow.
    fn run_judge(&self, j: &Judge, src: &dyn JudgeSource) -> Verdict {
        let started = match self.ready() {
            Ok(s) => s,
            Err(e) => return Verdict::failed(format!("judge `{}`: {e}", j.name)),
        };
        let (ruler, model, timeout, session, step, isolation, iso_base) = {
            let mut st = started.st.borrow_mut();
            let step = st.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| "driver".into());
            let isolation = st.cur_step.as_ref().map(|s| s.isolation).unwrap_or_default();
            let base = st.ext.get::<AGGState>().git.iso_base.clone();
            (
                st.ruler,
                st.judge_model.clone(),
                j.timeout.unwrap_or(st.judge_timeout),
                st.session,
                step,
                isolation,
                base,
            )
        };
        let kind = self.resolve_kind(&j.kind);
        let (verdict, spend) = crate::core::judge::run(
            &kind,
            &j.name,
            &self.dir,
            ruler,
            &model,
            timeout,
            Some(session),
            &step,
            isolation,
            src,
            Some(&iso_base),
        );
        // §5.6: judge spend counts against the ceilings — and against the RULER's per-agent tally.
        {
            let mut st = started.st.borrow_mut();
            st.tokens_spent += spend.tokens;
            if let Some(c) = spend.cost_usd {
                st.cost_spent += c;
            }
            let ruler_agent = st.cfg.judge.agent.clone();
            st.charge(&ruler_agent, spend.tokens, spend.cost_usd);
            st.publish();
        }
        verdict
    }

    /// A driver-constructed [`JudgeKind`] with its paths resolved against the project dir.
    ///
    /// The constructors (`Judge::rubric`/`script`) take the path VERBATIM, because a driver builds
    /// its judges above `Agg::open` (the chain is self-consuming, so it must) and cannot yet know
    /// the project root. A rubric's `inputs:` frontmatter is read here, at the same point.
    fn resolve_kind(&self, kind: &JudgeKind) -> JudgeKind {
        match kind {
            JudgeKind::Script { path } => JudgeKind::Script { path: self.dir.join(path) },
            JudgeKind::Llm { path, .. } => crate::core::judges::kind_for(&self.dir.join(path)),
            JudgeKind::Native { f } => JudgeKind::Native { f: f.clone() },
        }
    }
}

/// The lazy per-step verdict store, shared by `Agg::judge()` and by every `JudgeCtx` a native judge
/// receives — one memoization, one answer.
///
/// It owns the recursion stack rather than `Agg` because a nested ask goes through the SAME
/// instance: the ctx holds `&dyn JudgeSource`, so `ctx.met(&other)` re-enters here with the stack
/// intact.
struct Lazy<'a> {
    agg: &'a Agg,
    asking: RefCell<Vec<String>>,
}

impl JudgeSource for Lazy<'_> {
    fn verdict_for(&self, j: &Judge) -> Verdict {
        if let Some(v) = self.agg.verdicts.borrow().get(&j.name) {
            return v.clone();
        }
        // ⛔ Recursion is REFUSED, not overflowed: a judge that asks for itself (directly or round
        // a cycle) names the cycle in its own verdict.
        if self.asking.borrow().iter().any(|n| n == &j.name) {
            let mut path = self.asking.borrow().clone();
            path.push(j.name.clone());
            return Verdict::failed(format!("judge recursion: {}", path.join(" → ")));
        }
        self.asking.borrow_mut().push(j.name.clone());
        let verdict = self.agg.run_judge(j, self);
        self.asking.borrow_mut().pop();
        self.agg.verdicts.borrow_mut().insert(j.name.clone(), verdict.clone());
        // …and into the span, so `gate()` sees every verdict asked for since the last gate — which
        // is the whole reason `gate()` exists (a gate inside `step()` could not see a lazy ask).
        self.agg.span.borrow_mut().insert(j.name.clone(), verdict.clone());
        verdict
    }
}

// ---------------------------------------------------------------------------------------------
// ceilings, the operator bus, and the three notification levels
// ---------------------------------------------------------------------------------------------

impl Agg {
    /// Enforce the run's ceilings HERE. **Opt-in**: a driver that never calls this has none.
    ///
    /// # Why opt-in does not weaken the moat
    ///
    /// The moat is about the **worker**, which cannot reach any of this — `agg/private/` is carved
    /// out of its writable set, so it can neither forge a verdict nor raise its own budget. The
    /// driver is a binary the operator compiled; it could always have written different `.limits()`
    /// or not run agg at all.
    ///
    /// Cheap by construction (it reads counters already in memory), so calling it every cycle costs
    /// nothing. Idempotent: after the latch is set every `agg.*` call is already a no-op.
    ///
    /// On EVERY call, breach or not, it also drains the operator bus (so `agg stop` is seen) and
    /// checks for Ctrl-C — both end the run exactly as the shipped loop does.
    pub fn check_limits(&self) -> Result<(), Fatal> {
        let started = self.ready()?;
        self.operator_check(started)?;

        let limits = self.cfg.sequence.limits.clone();
        let (session, tokens, cost, elapsed) = {
            let st = started.st.borrow();
            (st.session, st.tokens_spent, st.cost_spent, st.loop_start.elapsed())
        };
        // The order is the table's. A `None` limit is not checked.
        if let Some(max) = limits.sessions {
            if session >= max {
                return Err(self.end_now(
                    started,
                    RunOutcome::MaxSessions,
                    format!("reached limits.sessions={max}"),
                    "max-sessions",
                ));
            }
        }
        if let Some(max) = limits.tokens {
            if tokens >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("over_budget: {tokens}/{max} tokens"), "abort:over_budget"));
            }
        }
        if let Some(max) = limits.cost {
            if cost >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("over_cost: ${cost:.4}/${max:.4}"), "abort:over_cost"));
            }
        }
        if let Some(max) = limits.wall_hours {
            let hours = elapsed.as_secs_f64() / 3600.0;
            if hours >= max {
                return Err(self.end_now(started, RunOutcome::Halt, format!("wall_hours: {hours:.2}/{max}"), "abort:wall_hours"));
            }
        }
        Ok(())
    }

    /// LATCH FIRST, then `Err` — so a driver that swallows the error still spends nothing after it.
    fn end_now(&self, started: &Started, outcome: RunOutcome, reason: String, tag: &str) -> Fatal {
        eprintln!("\n⚠ {reason} — stopping the run.");
        {
            let mut st = started.st.borrow_mut();
            st.emit(crate::plugin::LifecycleEvent::Finished { reason, ledger_tag: tag.to_string() });
        }
        self.ended.set(Some(outcome));
        Fatal::Ended(outcome)
    }

    /// Ctrl-C and the operator bus, on every ceiling check and every notification call.
    ///
    /// The bus is drained through the SHIPPED `BusDrain` handler, so `agg send inject|budget|pause|
    /// stop|resume` cannot come to mean one thing on the YAML path and another here.
    fn operator_check(&self, started: &Started) -> Result<(), Fatal> {
        if crate::os::signals::interrupted() {
            let outcome = started.st.borrow_mut().finish_interrupted();
            self.ended.set(Some(outcome));
            return Err(Fatal::Ended(outcome));
        }
        let flow = {
            let mut st = started.st.borrow_mut();
            crate::features::inject::BusDrain.run(&mut st)?
        };
        if let Flow::Stop(outcome) = flow {
            self.ended.set(Some(outcome));
            return Err(Fatal::Ended(outcome));
        }
        Ok(())
    }

    /// FYI. Lands in the log and the reader; nothing is expected back.
    pub fn info(&self, msg: &str) {
        self.note("info", msg);
    }

    /// A response would help; **the loop CONTINUES regardless**. This is `notify_if`'s non-terminal
    /// contract, made explicit and driver-invocable.
    pub fn ask(&self, msg: &str) {
        self.note("ask", msg);
    }

    /// A line for the log and the reader. Delivers no notification.
    pub fn log(&self, msg: &str) {
        self.note("log", msg);
    }

    /// The shared body of `info`/`ask`/`log`: the latch test (latch + Ctrl-C + a bus drain), then
    /// delivery.
    ///
    /// ⚠ These return `()`, so they can SET the latch but cannot RETURN the ceiling: the stop lands
    /// at the next `step()`/`gate()`/`check_limits()`/`block()`. That is still strictly better than
    /// nothing — the bus is drained and `agg stop` is *seen* — but it does not interrupt a driver
    /// that logs and then sleeps for an hour. Only a returning call could.
    fn note(&self, level: &str, msg: &str) {
        if self.ended.get().is_some() {
            return;
        }
        let Ok(started) = self.ready() else { return };
        let _ = self.operator_check(started);
        if self.ended.get().is_some() {
            return;
        }
        eprintln!("  [{level}] {msg}");
        if level != "log" {
            let mut st = started.st.borrow_mut();
            crate::features::notify::driver_ping(&mut st, level, msg);
        }
    }

    /// Cannot proceed without a human: deliver `msg` and WAIT on the operator bus until
    /// `agg send resume` (or `agg stop`) arrives.
    ///
    /// ⛔ **The worker cannot reach this.** The opt-in is the driver author's, in source, at one
    /// call site — which is what makes stop-and-wait legitimate here and never a mechanism.
    ///
    /// Ceilings keep firing while it waits: the loop below re-evaluates them every 5 seconds, the
    /// same chunking `RateLimitBackoff` uses, so `wall_hours`/`over_budget` END the run rather than
    /// hanging until morning.
    pub fn block(&self, msg: &str) -> Result<(), Fatal> {
        let started = self.ready()?;
        self.operator_check(started)?;
        {
            let mut st = started.st.borrow_mut();
            crate::features::notify::driver_ping(&mut st, "block", msg);
            // a stale Resume from before the block is not this block's answer.
            st.ext.get::<AGGState>().operator.resumed = false;
        }
        eprintln!("  [block] {msg}\n    answer with `agg send resume`, or end the run with `agg stop`.");
        loop {
            // the ceilings + Ctrl-C + the bus drain (which records the Resume), then the nap.
            self.check_limits()?;
            if std::mem::take(&mut started.st.borrow_mut().ext.get::<AGGState>().operator.resumed) {
                eprintln!("  [block] resumed by the operator.");
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(BLOCK_CHUNK_SECS));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// pos + summary — the two calls that never spend
// ---------------------------------------------------------------------------------------------

/// One declared position in the label path.
struct PosItem {
    id: u64,
    label: String,
    max: u64,
    value: u64,
}

/// An RAII breadcrumb: "cycle 7/20". Declared once, dropped when its scope ends.
///
/// agg cannot see a hand-written `for`, so this is how a bound reaches the reader at all. A loop
/// with no `pos` shows NO counter — never a wrong one.
pub struct PosFrame<'a> {
    id: u64,
    frames: &'a RefCell<Vec<PosItem>>,
}

impl PosFrame<'_> {
    /// Where the loop is now. The VALUE only — the bound was declared once, at `pos()`.
    pub fn update(&self, i: u64) {
        if let Some(f) = self.frames.borrow_mut().iter_mut().find(|f| f.id == self.id) {
            f.value = i;
        }
    }
}

impl Drop for PosFrame<'_> {
    /// ⚠ Removes THIS frame BY ID, never by popping the top. Nothing forces guards to drop LIFO —
    /// an explicit `drop(outer)` before an inner one, or a guard stored in a struct, would pop the
    /// WRONG frame and silently corrupt the label path.
    fn drop(&mut self) {
        self.frames.borrow_mut().retain(|f| f.id != self.id);
    }
}

/// Frame ids. A plain counter: ids only need to be distinct within one process.
static NEXT_POS_ID: AtomicU64 = AtomicU64::new(1);

impl Agg {
    /// Declare where the driver is: a label and its DECLARED bound. Spends nothing, and is
    /// unaffected by the latch.
    pub fn pos(&self, label: impl Into<String>, max: u64) -> PosFrame<'_> {
        let id = NEXT_POS_ID.fetch_add(1, Ordering::Relaxed);
        self.pos.borrow_mut().push(PosItem { id, label: label.into(), max, value: 0 });
        PosFrame { id, frames: &self.pos }
    }

    /// The label path as a reader sees it — `cycle 3/20 › attempt 2/3`.
    pub fn label_path(&self) -> String {
        self.pos
            .borrow()
            .iter()
            .map(|f| format!("{} {}/{}", f.label, f.value, f.max))
            .collect::<Vec<_>>()
            .join(" › ")
    }

    /// The run's human-readable status — the same view `agg status` renders, from the same
    /// published `state.json`. Spends nothing and works before the run has started.
    pub fn summary(&self) -> String {
        crate::ui::status::render(&self.dir)
    }

    /// The ledger cursor. Phase 1 reads it; today it only counts completed steps.
    pub fn ord(&self) -> u64 {
        self.ord.get()
    }

    /// The outcome the run is LATCHED to, if it has ended.
    pub fn ended(&self) -> Option<RunOutcome> {
        self.ended.get()
    }

    /// The project root, canonicalized.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// THE UNGATED-SPAN WARNING this run would print if it ended now, or `None` if everything the
    /// driver staged has been gated.
    ///
    /// Deliberately NOT one of the eleven calls, and it exists for one reason: "is the run about to
    /// tell the operator their work is stranded, and does it name the right branch" is a question
    /// only the run's own git state can answer, and a test that rebuilt the sentence from its own
    /// idea of the branch names would assert nothing.
    pub fn ungated_span(&self) -> Option<String> {
        let started = self.started.get()?;
        let mut st = started.st.borrow_mut();
        let git = &mut st.ext.get::<AGGState>().git;
        crate::features::finalize::stranded_span_message(
            &git.span_branches,
            git.span_tip.as_deref(),
            &git.iso_base,
        )
    }

    /// End the run EXPLICITLY, with the outcome the driver means.
    ///
    /// The optional twelfth call. A clean drop records [`RunOutcome::GoalsMet`], because `Drop` cannot
    /// see whether `main` returned `Ok` or `Err` — this is how a driver that cares says which it was.
    /// Idempotent, and a no-op once a ceiling has already latched an outcome.
    pub fn finish(&self, outcome: RunOutcome) {
        if self.ended.get().is_some() {
            return;
        }
        // NOT `ready()`: booting a whole run — preflight, guards, the banner — just to record that it
        // is over would be absurd. A driver that never spent anything simply latches.
        if let Some(started) = self.started.get() {
            let mut st = started.st.borrow_mut();
            record_end(&mut st, outcome);
        }
        self.ended.set(Some(outcome));
    }

    /// How many worker sessions this run has launched. `0` before the first `step()`.
    ///
    /// Deliberately NOT one of the eleven calls: a driver reads per-step numbers off
    /// [`StepOutcome`] and the run's own view off [`Agg::summary`]. It exists because "did the latch
    /// really stop the pipeline" is a question only the counter can answer.
    pub fn sessions(&self) -> u32 {
        self.started.get().map(|s| s.st.borrow().session).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------
// run end — BUILD.md §3.10
// ---------------------------------------------------------------------------------------------

/// Stamp the run's end into `state.json` (`dash.finished` + the reason) and the run ledger
/// (`ledger.finish`), through the SAME `emit(Finished)` every `Flow::Stop` on the YAML path uses —
/// so `agg status`, the dashboard and `agg history` cannot come to mean one thing per entry point.
fn record_end(st: &mut LoopState, outcome: RunOutcome) {
    let n = st.session;
    let (reason, tag) = match outcome {
        RunOutcome::GoalsMet => (format!("driver returned after {n} session(s)"), "goals-met"),
        RunOutcome::Halt => (format!("driver ended the run after {n} session(s)"), "abort:driver"),
        RunOutcome::MaxSessions => (format!("driver stopped at its session ceiling ({n})"), "max-sessions"),
        RunOutcome::Stopped => (format!("driver stopped the run after {n} session(s)"), "stopped"),
    };
    st.emit(crate::plugin::LifecycleEvent::Finished { reason, ledger_tag: tag.to_string() });
}

/// RUN END. For a driver, "run end" is `main` returning or panicking — there is no post-loop
/// finalize to inherit, because on the YAML path all of it happens inside `CheckRunStop`.
///
/// Order is load-bearing: this body runs FIRST, then `Agg`'s fields drop, which fires `StopHooks`
/// (the `on_stop` shell hook) and `RunPidGuard` (releases the double-run guard). So the run is
/// recorded as ended before anything the operator wired to `on_stop` observes it.
///
/// | how the driver ends | how agg learns | recorded |
/// |---|---|---|
/// | a ceiling fired | the latch | the latched [`RunOutcome`] |
/// | panic | `std::thread::panicking()` | the ledger's own pessimistic `crashed` |
/// | clean drop, no latch | `Drop` | [`RunOutcome::GoalsMet`] |
///
/// The last row's `Err`-return ambiguity is ACCEPTED: `Drop` cannot see whether `main` returned
/// `Ok` or `Err`. [`Agg::finish`] is the explicit form for a driver that wants the distinction.
impl Drop for Agg {
    fn drop(&mut self) {
        // ⚠ ONE `eprintln!` AND NOTHING ELSE while unwinding. A panic inside `Drop` aborts the
        // process, and the frame that panicked may well have been holding the `RefCell` — so a
        // `borrow_mut()` here would BE that second panic. The ledger already carries a pessimistic
        // `crashed` end_reason from `RunLedger::begin` and stamps the end time in its own `Drop`, so
        // the run is still recorded as failed without agg touching anything.
        if std::thread::panicking() {
            eprintln!("  ⚠ agg: the driver PANICKED — any staged span is left on its branch, un-merged.");
            return;
        }
        let Some(started) = self.started.get() else {
            return; // nothing ever spent: there is no run to finalize.
        };
        // `try_borrow_mut` and not `borrow_mut`: a Drop that panics is strictly worse than a Drop
        // that silently skips a report, and nothing else can be holding this borrow here.
        let Ok(mut st) = started.st.try_borrow_mut() else { return };

        // the ungated span, named, with the command that lands it. ⛔ Never auto-merged (agg must not
        // make a call it was not given) and never auto-rolled-back (discarding an overnight run over a
        // late regression is far worse than keeping it).
        crate::features::finalize::report_stranded_span(&mut st);

        // `dash.finished`, not the latch: a `Flow::Stop` that already emitted `Finished` must not be
        // re-recorded, while a latched outcome that never got emitted still must be.
        if !st.dash.finished {
            record_end(&mut st, self.ended.get().unwrap_or(RunOutcome::GoalsMet));
        }
    }
}
