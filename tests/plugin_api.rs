//! THE PLUGIN PARITY PROOF (LOOPSTATE_REDESIGN §4, HOOK_REDESIGN §1).
//!
//! This is a SEPARATE CRATE from `agg`. If a plugin defined here — outside the core, with zero access
//! to private items — can implement `agg`'s `Handler`, stash its OWN typed state in the shared
//! extension store, and be dispatched by the real `run_hook`, then agg's own features have no
//! privileged path: the core is a microkernel and everything (agg included) is a plugin on the same
//! public mechanism. That is the whole architecture, tested from the outside.

use std::path::Path;
use std::time::Instant;

use agg::loop_::{Extensions, Flow, Handler, LoopState, run_hook};

// ── a THIRD-PARTY plugin: its own state type, its own handler. Touches nothing private. ──
#[derive(Default)]
struct Counter {
    ticks: u32,
}
/// observed by a LATER plugin — proves cross-hook threading of a non-agg type.
#[derive(Default)]
struct Seen {
    ticks: u32,
}

struct Tick;
impl Handler for Tick {
    fn run(&self, ctx: &mut LoopState) -> anyhow::Result<Flow> {
        ctx.ext.get::<Counter>().ticks += 1; // its OWN type, in the shared store, no core edit
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "third-party::Tick"
    }
}
struct Observe;
impl Handler for Observe {
    fn run(&self, ctx: &mut LoopState) -> anyhow::Result<Flow> {
        let ticks = ctx.ext.get::<Counter>().ticks; // sees the earlier plugin's write
        ctx.ext.get::<Seen>().ticks = ticks;
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "third-party::Observe"
    }
}

/// Build a minimal `LoopState` from OUTSIDE the crate — proof the context itself is fully public
/// (no facade), so a plugin host can stand one up. Only the pieces a plugin needs are exercised.
fn probe_state<'a>(cfg: &'a agg::core::config::AggConfig, dir: &'a Path) -> LoopState<'a> {
    let loop_start = Instant::now();
    let dash = agg::state::DashboardState::default();
    LoopState {
        cfg,
        ruler: agg::backend::for_name("claude").unwrap(),
        judge_model: "m".into(),
        judge_timeout: 1,
        dir,
        config_base: dir,
        eng: agg::core::engine::Engine::new(vec![], "iterations > 999999".into(), None, None).unwrap(),
        cursor: agg::core::sequence::Cursor::new(vec![]),
        cur_step: None,
        live: agg::state::LiveState::new(dir, loop_start, dash.clone()),
        dash,
        ledger: agg::project::RunLedger::begin(dir, "probe", 0, 0),
        bus: None,
        budget_total: None,
        cost_limit: None,
        max_iter: None,
        max_sessions: 0,
        gate_regressions: false,
        loop_start,
        lifetime_base: 0,
        session: 0,
        tokens_spent: 0,
        cost_spent: 0.0,
        per_agent: std::collections::BTreeMap::new(),
        ext: Extensions::default(),
        scratch: Extensions::default(),
    }
}

#[test]
fn a_third_party_plugin_owns_state_and_is_dispatched_by_the_real_core() {
    let tmp = tempfile::tempdir().unwrap();
    // both runtime roots: the worker-writable one and the agg-owned `private/` (where the ledger
    // `RunLedger::begin` writes lives) — a plugin host stands up the same layout agg does.
    std::fs::create_dir_all(agg::paths::agg_dir(tmp.path())).unwrap();
    std::fs::create_dir_all(agg::paths::private_dir(tmp.path())).unwrap();
    let cfg_path = tmp.path().join("agg.yaml");
    std::fs::write(&cfg_path, "project: probe\nsequence:\n  steps: []\n").unwrap();
    let cfg = agg::core::config::AggConfig::load(&cfg_path).unwrap();

    let mut st = probe_state(&cfg, tmp.path());

    // register two third-party plugins on ONE hook and let agg's real dispatcher run them.
    let hooks: Vec<Box<dyn Handler>> = vec![Box::new(Tick), Box::new(Tick), Box::new(Observe)];
    let end = run_hook(&hooks, &mut st).expect("dispatch ok");
    assert!(end.is_none(), "all plugins Continue'd → the hook drained");

    // both Ticks wrote to the same Counter in the shared store; Observe read it back.
    assert_eq!(st.ext.get::<Counter>().ticks, 2, "external plugin state accumulated across the hook");
    assert_eq!(
        st.ext.get::<Seen>().ticks,
        2,
        "a LATER external plugin saw an EARLIER one's typed state — cross-plugin threading via ext"
    );
}
