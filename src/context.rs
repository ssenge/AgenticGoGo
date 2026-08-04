//! The shared `LoopState` context a plugin operates on.

use std::path::PathBuf;
use std::time::Instant;
use crate::backend::AgentBackend;
use crate::bus::Bus;
use crate::core::config::{AggConfig, ResolvedStep};
use crate::core::engine::{Engine, RunState};
use crate::core::sequence::Cursor;
use crate::features::state::AGGState;
use crate::plugin::{Extensions, RunOutcome, LifecycleEvent};
use crate::state::{DashboardState, LiveState};
use crate::util::now_epoch;

/// Everything one step of the loop reads and writes.
/// The context every plugin (`Handler`) receives — the whole run/session state, shared `&mut`
/// (HOOK_REDESIGN §8: the context IS the state, threaded sequentially, no facade). Fields are `pub`
/// per the crate's no-facade convention (lib.rs) so a plugin in ANY module/crate reaches what it
/// needs — most importantly its own typed state via `ext`/`scratch`. The core knows only the hook
/// registry + this shared bus; every feature (agg's own included) is a plugin against it.
///
/// `cfg`/`dir`/`config_base` are OWNED, not borrowed (RUST_API §3.1). A driver facade holds the
/// config AND the state in one struct; borrowing a sibling field is self-referential and not
/// expressible in safe Rust, so the lifetime parameter had to go. Reads still deref through.
pub struct LoopState {
    pub cfg: AggConfig,
    /// the RULER — LLM judges + summarizer. Immutable across the run (§4).
    pub ruler: &'static dyn AgentBackend,
    /// the ruler model (`judge.model`, resolved).
    pub judge_model: String,
    /// EVERY judge's timeout (`judge.timeout`).
    pub judge_timeout: u64,
    pub dir: PathBuf,
    pub config_base: PathBuf,

    pub eng: Engine,
    /// the sequence cursor — yields the next step name each cycle.
    pub cursor: Cursor,
    /// the step being run THIS cycle (set by INJECT).
    pub cur_step: Option<ResolvedStep>,

    pub dash: DashboardState,
    pub live: LiveState,
    pub ledger: crate::project::RunLedger,
    pub bus: Option<Bus>,

    pub budget_total: Option<u64>,
    pub cost_limit: Option<f64>,
    pub max_iter: Option<u32>,
    pub max_sessions: u32,
    pub gate_regressions: bool,

    pub loop_start: Instant,
    pub lifetime_base: u32,

    pub session: u32,
    pub tokens_spent: u64,
    pub cost_spent: f64,
    /// per-agent token + cost tally (§7.4), attributed at each spend site (worker / ruler judges /
    /// summarizer). Sums to `tokens_spent`/`cost_spent`; makes a mixed run's totals interpretable.
    pub per_agent: std::collections::BTreeMap<String, crate::state::AgentUsage>,

    /// per-RUN generic extension store — agg's own feature state lives here as `AGGState`; a plugin
    /// stashes its own type. Persists across sessions (never cleared mid-run). LOOPSTATE_REDESIGN §3.
    pub ext: Extensions,
    /// per-SESSION generic extension store — agg's stage channel lives here as `AGGScratch`;
    /// `clear()`ed each session at the loop top so no field leaks across sessions (§3/§8).
    pub scratch: Extensions,
}

impl LoopState {
    pub fn emit(&mut self, event: LifecycleEvent) {
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
    pub fn charge(&mut self, agent: &str, tokens: u64, cost: Option<f64>) {
        let e = self.per_agent.entry(agent.to_string()).or_default();
        e.tokens += tokens;
        if let Some(c) = cost {
            e.cost = Some(e.cost.unwrap_or(0.0) + c);
        }
    }

    pub fn publish(&mut self) {
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

    pub fn run_state(&self) -> RunState {
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
        (self.ext.get::<AGGState>().worker.dud_streak >= LIMIT).then(|| {
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

    pub fn finish_interrupted(&mut self) -> RunOutcome {
        eprintln!("\n⚠ interrupted (SIGINT/SIGTERM) — stopping after the current session; worker killed, base untouched.");
        self.emit(LifecycleEvent::Finished {
            reason: "interrupted (SIGINT/SIGTERM)".into(),
            ledger_tag: "interrupted".into(),
        });
        RunOutcome::Stopped
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
