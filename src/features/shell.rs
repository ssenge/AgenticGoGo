//! A user shell-hook list as a plugin — best-effort, non-fatal (exactly `hooks::run`). Self-contained:
//! carries its own `dir`, so it fires from any hook via `run` (per-session) or `fire` (run-level:
//! on_start/on_stop) without needing the `LoopState` context.

use anyhow::Result;

use crate::isolation::Isolation;
use crate::loop_::{Flow, Handler, LoopState};

pub struct ShellHook {
    pub label: &'static str,
    pub cmds: Vec<String>,
    pub dir: std::path::PathBuf,
    /// Blast-radius tier for the RUN-LEVEL dispatch path (`fire()`, no step context): `None` for
    /// `on_start` (pre-worker, clean tree), the run's tier for `on_stop` (post-worker teardown).
    /// The per-session path (`run(ctx)`) ignores this and uses the CURRENT step's tier instead.
    pub isolation: Isolation,
}
impl Handler for ShellHook {
    /// Per-session dispatch (on_session_start / on_session_end, inside a Feature): confine with the
    /// CURRENT step's tier — the worker that just ran (or is about to) under that tier could have
    /// rewritten a file this hook execs, so the hook must run in the same jail (ISOLATION.md §13).
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let tier = ctx.cur_step.as_ref().map(|s| s.isolation).unwrap_or(self.isolation);
        crate::hooks::run(self.label, &self.cmds, &self.dir, tier);
        Ok(Flow::Continue)
    }
    /// Run-level dispatch (on_start / on_stop, fired outside any step): use the baked tier.
    fn fire(&self) {
        crate::hooks::run(self.label, &self.cmds, &self.dir, self.isolation);
    }
    fn name(&self) -> &'static str {
        self.label
    }
}
