//! The hook registry + dispatch: `run_hook`, the `Feature` handler, `run_pre_start`, and `Lifecycle`.

use std::path::Path;
use anyhow::Result;
use crate::core::config::AggConfig;
use crate::plugin::{Bootstrap, End, Flow, Handler, PreStart};
use crate::context::LoopState;
use crate::features::state::AGGScratch;

/// Dispatch one hook point's handlers in order, honoring `Flow`. A handler's hard `Err` bubbles out
/// (it is NOT a `RunOutcome` — e.g. `verdicts::append`, or the worker-broken guard). §3.1.
pub fn run_hook(hooks: &[Box<dyn Handler>], st: &mut LoopState) -> Result<Option<End>> {
    for h in hooks {
        if st.scratch.get::<AGGScratch>().skip_judges && !h.runs_on_skip() {
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

/// A HIGH-LEVEL feature hook: one named registry entry composing several ordered sub-steps (each its
/// own small `Handler`). The registry reads as the loop's lifecycle (Inject / Run / Verify / Gate /
/// Finalize …) while each step stays a focused unit. It dispatches its steps with the SAME `run_hook`
/// semantics (Flow, `runs_on_skip`), so grouping changes NOTHING about behavior — it is the flat
/// handler list, nested one level. This is what keeps the registry readable without micro-task soup.
struct Feature {
    name: &'static str,
    steps: Vec<Box<dyn Handler>>,
}
impl Handler for Feature {
    fn run(&self, st: &mut LoopState) -> Result<Flow> {
        Ok(match run_hook(&self.steps, st)? {
            Some(End::NextSession) => Flow::SkipSession,
            Some(End::Stop(o)) => Flow::Stop(o),
            None => Flow::Continue,
        })
    }
    fn name(&self) -> &'static str {
        self.name
    }
}

pub(crate) fn run_pre_start(hs: &[Box<dyn PreStart>], boot: &mut Bootstrap) -> Result<()> {
    for h in hs {
        h.run(boot)?; // a `bail!` propagates out of `run()`, exactly like the old inline check
    }
    Ok(())
}

/// The hook registry: one ordered plugin list per lifecycle point. `default_pipeline` is agg's OWN
/// registration (its features are plugins, no different from a third party's). Fields are `pub` so a
/// host can `lifecycle.on_verify.push(Box::new(MyPlugin))` before the loop — config-in-code, no yaml
/// (HOOK_REDESIGN §5). This is the OQ5 registration seam.
#[derive(Default)]
pub struct Lifecycle {
    pub pre_start: Vec<Box<dyn PreStart>>,
    pub on_start: Vec<Box<dyn Handler>>,
    pub on_run_start: Vec<Box<dyn Handler>>,
    pub background: Vec<Box<dyn Handler>>,
    pub on_session_start: Vec<Box<dyn Handler>>,
    pub on_run: Vec<Box<dyn Handler>>,
    pub on_verify: Vec<Box<dyn Handler>>,
    pub on_gate: Vec<Box<dyn Handler>>,
    pub on_session_end: Vec<Box<dyn Handler>>,
    pub on_stop: Vec<Box<dyn Handler>>,
}
impl Lifecycle {
    pub fn default_pipeline(cfg: &AggConfig, dir: &Path) -> Self {
        Self::with_hooks(&cfg.hooks, dir)
    }
    /// Split out from `default_pipeline` so the registration ORDER is testable without a full
    /// `AggConfig` (`Hooks: Default`).
    pub(crate) fn with_hooks(hooks: &crate::core::config::Hooks, dir: &Path) -> Self {
        let mut l = Lifecycle::default();
        let shell = |label: &'static str, cmds: &[String]| -> Box<dyn Handler> {
            Box::new(crate::features::shell::ShellHook { label, cmds: cmds.to_vec(), dir: dir.to_path_buf() })
        };
        let feature = |name: &'static str, steps: Vec<Box<dyn Handler>>| -> Box<dyn Handler> {
            Box::new(Feature { name, steps })
        };
        // ── THE REGISTRY, read top-to-bottom = the loop's lifecycle. Each hook point holds a
        //    HIGH-LEVEL FEATURE; a feature's `vec![…]` is its internal structure (small, focused
        //    steps), dispatched with the same Flow/skip semantics — grouping changes no behavior. ──
        l.pre_start.push(Box::new(crate::features::gitsetup::GitSetup)); // git preconditions (before the loop state exists)
        l.on_start.push(shell("on_start", &hooks.on_start));
        l.background.push(Box::new(crate::features::setup::BackgroundSpawn { cmds: hooks.background.clone() }));
        l.on_run_start.push(Box::new(crate::features::setup::Baseline)); // baseline judge pass, then bootstrap finalize:
        l.on_run_start.push(Box::new(crate::features::setup::Setup));
        l.on_session_start.push(feature(
            "Inject",
            vec![
                Box::new(crate::features::inject::BusDrain),
                Box::new(crate::features::inject::PickStep),
                Box::new(crate::features::inject::SessionBranchCut),
                shell("on_session_start", &hooks.on_session_start),
                Box::new(crate::features::inject::WriteInstructions),
                Box::new(crate::features::inject::ClearMemScratch),
            ],
        ));
        l.on_run.push(feature("Run", vec![Box::new(crate::features::run::LaunchWorker)]));
        l.on_verify.push(feature(
            "Verify",
            vec![
                Box::new(crate::features::verify::FloorFold),
                Box::new(crate::features::verify::RateLimitBackoff),
                Box::new(crate::features::verify::GitAutoCommit),
                Box::new(crate::features::verify::SnapshotGoals),
                Box::new(crate::features::verify::StageSpan),
                Box::new(crate::features::verify::StageMerge),
                Box::new(crate::features::verify::RunJudges),
            ],
        ));
        l.on_gate.push(feature("Gate", vec![Box::new(crate::features::gate::CeilingPoisonGuard), Box::new(crate::features::gate::GateKeepRollback)]));
        l.on_session_end.push(feature(
            "Finalize",
            vec![
                shell("on_session_end", &hooks.on_session_end),
                Box::new(crate::features::summary::Summarize),
                Box::new(crate::features::finalize::RefineFold),
                Box::new(crate::features::finalize::CheckRunStop),
            ],
        ));
        l.on_stop.push(shell("on_stop", &hooks.on_stop));
        l
    }
}
