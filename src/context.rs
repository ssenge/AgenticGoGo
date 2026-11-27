//! The shared `LoopState` context a plugin operates on.

use std::path::PathBuf;
use std::time::Instant;
use crate::backend::AgentBackend;
use crate::bus::Bus;
use crate::core::config::{RelentlessConfig, ResolvedStep};
use crate::core::engine::{Engine, RunState};
use crate::core::walk::Walk;
use crate::features::state::RLTState;
use crate::plugin::{Extensions, RunOutcome, LifecycleEvent};
use crate::state::{DashboardState, LiveState};
use crate::util::now_epoch;

/// Everything one step of the loop reads and writes.
/// The context every plugin (`Handler`) receives — the whole run/session state, shared `&mut`
/// (HOOK_REDESIGN §8: the context IS the state, threaded sequentially, no facade). Fields are `pub`
/// per the crate's no-facade convention (lib.rs) so a plugin in ANY module/crate reaches what it
/// needs — most importantly its own typed state via `ext`/`scratch`. The core knows only the hook
/// registry + this shared bus; every feature (rlt's own included) is a plugin against it.
///
/// `cfg`/`dir`/`config_base` are OWNED, not borrowed (RUST_API §3.1). A driver facade holds the
/// config AND the state in one struct; borrowing a sibling field is self-referential and not
/// expressible in safe Rust, so the lifetime parameter had to go. Reads still deref through.
pub struct LoopState {
    pub cfg: RelentlessConfig,
    /// the RULER — rubric verifiers + summarizer. Immutable across the run (§4).
    pub ruler: &'static dyn AgentBackend,
    /// the ruler model (`verifier.model`, resolved).
    pub verifier_model: String,
    /// EVERY verifier's timeout (`verifier.timeout`).
    pub verifier_timeout: u64,
    pub dir: PathBuf,
    pub config_base: PathBuf,

    pub eng: Engine,
    /// the sequence walk — yields the next step name each cycle (`core::walk`).
    pub cursor: Walk,
    /// the step being run THIS cycle (set by INJECT).
    pub cur_step: Option<ResolvedStep>,
    /// the step a Rust DRIVER handed to `rlt.step(&s)`. `PickStep` `take()`s it BEFORE it consults
    /// anything else, which is how a driver's flow replaces the `sequence:` list without a second
    /// dispatch path existing anywhere. Always `None` on the YAML path.
    ///
    /// It is a [`ResolvedStep`] and not a `StepBody` because `StepBody` has no `name` (names are the
    /// `steps:` map keys) and `PickStep` needs one for the banner, the dashboard and `cur_step`.
    pub next_step: Option<ResolvedStep>,

    pub dash: DashboardState,
    pub live: LiveState,
    pub ledger: crate::project::RunLedger,
    pub bus: Option<Bus>,

    pub budget_total: Option<u64>,
    pub cost_limit: Option<f64>,
    pub max_iter: Option<u32>,
    pub max_sessions: u32,
    pub gate_regressions: bool,

    /// Process uptime, monotonic. Backs `up_secs` in the reader — an honest "this process has been
    /// alive N seconds", which is NOT the same question as `wall_time`.
    pub loop_start: Instant,
    /// The END-TO-END run clock, persisted so it survives a resume, plus the human wait banked so
    /// far. Backs the `wall_time`/`human_wait_time`/`work_time` terms. See [`crate::core::clock`].
    pub clock: crate::core::clock::Clock,
    pub lifetime_base: u32,

    pub session: u32,
    pub tokens_spent: u64,
    pub cost_spent: f64,
    /// per-agent token + cost tally (§7.4), attributed at each spend site (worker / ruler verifiers /
    /// summarizer). Sums to `tokens_spent`/`cost_spent`; makes a mixed run's totals interpretable.
    pub per_agent: std::collections::BTreeMap<String, crate::state::AgentUsage>,

    /// per-RUN generic extension store — rlt's own feature state lives here as `RLTState`; a plugin
    /// stashes its own type. Persists across sessions (never cleared mid-run). LOOPSTATE_REDESIGN §3.
    pub ext: Extensions,
    /// per-SESSION generic extension store — rlt's stage channel lives here as `RLTScratch`;
    /// `clear()`ed each session at the loop top so no field leaks across sessions (§3/§8).
    pub scratch: Extensions,
}

impl LoopState {
    pub fn emit(&mut self, event: LifecycleEvent) {
        self.dash.phase = event.phase();
        if let LifecycleEvent::Finished { reason, ledger_tag } = &event {
            self.dash.finished = true;
            self.dash.finish_reason = reason.clone();
            let (gm, gt) = self.tally();
            let usage = self.ext.get::<crate::usage::Ledger>().summary();
            self.ledger.set_usage(usage);
            self.ledger.update(self.session, self.tokens_spent, gm, gt);
            self.ledger.finish(now_epoch(), ledger_tag);
        }
        self.publish();
    }

    /// met / total for every scoreboard — the snapshot AND the run ledger, which must never disagree
    /// about the same run (they did: `state.json` said `2/2` while `rlt history` said `0/0`).
    ///
    /// [`Engine::tally`] counts the DoD-set, which only the YAML path has; a driver decides it is done
    /// by RETURNING and leaves `eng.verifiers` empty by construction. Falling back to the verifiers it
    /// actually ASKED keeps the number honest — a vacuous `0/0` reads as "achieved nothing", not as
    /// "declared no DoD".
    fn tally(&self) -> (usize, usize) {
        match self.eng.tally() {
            (_, 0) if !self.dash.verifiers.is_empty() => {
                (self.dash.verifiers.iter().filter(|j| j.met).count(), self.dash.verifiers.len())
            }
            counted => counted,
        }
    }

    /// Attribute one spend to an agent's running tally (§7.4). A `None` cost is an agent that cannot
    /// report a price — it never fabricates a `0`, so that agent's cost stays `None` (rendered "—")
    /// until a real price arrives, then it accumulates only the reported part.
    pub fn charge(&mut self, agent: &str, tokens: u64, cost: Option<f64>) {
        let e = self.per_agent.entry(agent.to_string()).or_default();
        e.tokens += tokens;
        if let Some(c) = cost {
            e.cost = Some(e.cost.unwrap_or(0.0) + c);
        }
    }

    /// Account one agent call everywhere it counts: the run's spend counters (what `limits.tokens`
    /// and `limits.cost` read — output tokens and reported dollars), the per-agent tally, and the
    /// per-session usage record behind the report's `session_log` (A-19).
    pub fn charge_usage(&mut self, role: crate::usage::Role, agent: &str, u: &crate::usage::Usage) {
        let output = u.tokens.output_or_zero();
        self.tokens_spent += output;
        if let Some(c) = u.cost_usd {
            self.cost_spent += c;
        }
        self.charge(agent, output, u.cost_usd);
        let step = self.cur_step.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let session = self.session;
        self.ext.get::<crate::usage::Ledger>().record(session, &step, role, agent, u);
    }

    /// Account every call the RULER made (rubric verifiers, the summarizer).
    pub fn charge_ruler(&mut self, calls: &[crate::usage::Usage]) {
        let ruler = self.cfg.verifier.agent.clone();
        self.charge_ruler_as(&ruler, calls);
    }

    /// [`Self::charge_ruler`] for a verifier that asked an agent of its own (`Verifier::agent`).
    pub fn charge_ruler_as(&mut self, agent: &str, calls: &[crate::usage::Usage]) {
        for u in calls {
            self.charge_usage(crate::usage::Role::Ruler, agent, u);
        }
    }

    pub fn publish(&mut self) {
        self.dash.up_secs = self.loop_start.elapsed().as_secs();
        // Republished on every publish rather than cached: an ask's AGE changes with the clock, and a
        // reader showing "waiting 0s" for the last twenty minutes is worse than showing nothing.
        let now = crate::util::now_epoch();
        self.dash.asks = crate::core::asks::open(&self.dir)
            .iter()
            .map(|a| crate::state::AskView::of(a, now))
            .collect();
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
        self.dash.goals = DashboardState::goals_from_engine(&self.eng, &self.dash.goals);
        // ⚠ ONLY when the engine HAS verifiers. On the DRIVER path `eng.verifiers` is empty by construction
        // — the run-set is built solely from YAML expressions (`assembly.rs`), and a driver's verifiers
        // are `Verifier` VALUES in its own binary that it hands to `rlt.verifier(&j)` one at a time.
        // Rebuilding unconditionally here overwrote what the facade had just written with an empty
        // vec, so the TUI rendered `(no verifiers in snapshot)`, the web BFF served `verifiers: []` and
        // Progress read `0/0 · 0%` — all while the verifiers were demonstrably running and their
        // verdicts were reaching `verdicts.jsonl`. The publisher, not the ledger, was the broken half.
        if !self.eng.verifiers.is_empty() {
            self.dash.verifiers = DashboardState::verifiers_from_engine(&self.eng, &self.dash.verifiers);
        }
        let (m, t) = self.tally();
        self.dash.goals_met = m;
        self.dash.goals_total = t;
        self.publish_report_parts(now);
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

    /// Copy into the snapshot what the run report states and a live reader needs mid-run (A-17):
    /// the clock, session fates, detectors, ratchet references and usage totals. All from the loop's
    /// own memory — the same sources the report is built from, so the two never disagree.
    fn publish_report_parts(&mut self, now: u64) {
        self.dash.clock_started_at = self.clock.started_at_epoch;
        self.dash.time = crate::report::Time {
            wall_time_s: self.clock.wall_secs(now),
            work_time_s: self.clock.work_secs(now),
            human_wait_time_s: self.clock.human_wait_secs,
        };
        let staged = self.ext.get::<RLTState>().git.span_branches.len() as u32;
        let tally = self.ext.get::<crate::report::Tally>();
        self.dash.sessions = crate::report::Sessions {
            total: self.session,
            merged: tally.merged,
            rolled_back: tally.rolled_back,
            no_improvement: tally.no_improvement,
            no_changes: tally.no_changes,
            staged_unmerged: staged,
        };
        self.dash.detectors = crate::report::detectors(&self.eng.gate, &self.dash.verifiers);
        self.dash.ratchet = crate::report::ratchets(&self.eng.gate);
        self.dash.usage = self.ext.get::<crate::usage::Ledger>().summary();
    }

    pub fn run_state(&self) -> RunState {
        RunState {
            tokens_spent: self.tokens_spent,
            budget_total: self.budget_total,
            cost_spent: self.cost_spent,
            cost_limit: self.cost_limit,
            sessions_done: self.session,
            max_sessions: self.max_iter,
            // NOT `loop_start.elapsed()`: that is process uptime, and `wall_time` is defined as
            // end-to-end across resumes (`internal/HUMAN_LOOP.md` §7.4).
            wall_secs: self.clock.wall_secs(crate::util::now_epoch()),
            human_wait_secs: self.clock.human_wait_secs,
            rate_limit_wait_secs: self.clock.rate_limit_wait_secs,
        }
    }

    pub fn over_max_sessions(&mut self) -> Option<RunOutcome> {
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

    pub fn stopped_via_bus(&mut self, reason: String) -> RunOutcome {
        eprintln!("  [bus] stop → {reason}");
        self.emit(LifecycleEvent::Finished {
            reason: format!("stopped via bus: {reason}"),
            ledger_tag: "stopped".into(),
        });
        RunOutcome::Stopped
    }



    pub fn worker_is_broken(&mut self) -> Option<anyhow::Error> {
        const LIMIT: u32 = 3;
        (self.ext.get::<RLTState>().worker.dud_streak >= LIMIT).then(|| {
            let agent = self.cur_step.as_ref().map(|s| s.agent.as_str()).unwrap_or("worker");
            anyhow::anyhow!(
                "the `{agent}` worker failed to start {LIMIT} times in a row — non-zero exit, ZERO \
                 tokens, every time.\n\
                 That means the agent CLI rejected the invocation itself; it never reached the model. \
                 Retrying cannot help: each session builds the same command.\n\
                 Run `rlt doctor`, and try the worker by hand to see the CLI's own error."
            )
        })
    }

    pub fn finish_interrupted(&mut self) -> RunOutcome {
        eprintln!("\n⚠ interrupted (SIGINT/SIGTERM) — stopping after the current session; worker killed, base untouched.");
        self.emit(LifecycleEvent::Finished {
            reason: "interrupted (SIGINT/SIGTERM)".into(),
            ledger_tag: "interrupted".into(),
        });
        RunOutcome::Stopped
    }

    /// The run ends because free disk space fell below the floor: exit 7, resumable.
    pub fn disk_halt(&mut self, reason: &str) -> RunOutcome {
        eprintln!("\n⚠ {reason}");
        self.emit(LifecycleEvent::Finished { reason: reason.to_string(), ledger_tag: "disk".into() });
        RunOutcome::Disk
    }

    pub fn abort_now(&mut self, reason: &str) -> RunOutcome {
        eprintln!("\n⚠ {reason}");
        self.emit(LifecycleEvent::Finished {
            reason: reason.to_string(),
            ledger_tag: format!("abort:{reason}"),
        });
        RunOutcome::Halt
    }


}
