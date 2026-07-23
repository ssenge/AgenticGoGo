//! A user shell-hook list as a plugin — best-effort, non-fatal (exactly `hooks::run`). Self-contained:
//! carries its own `dir`, so it fires from any hook via `run` (per-session) or `fire` (run-level:
//! on_start/on_stop) without needing the `LoopState` context.

use anyhow::Result;

use crate::loop_::{Flow, Handler, LoopState};

pub struct ShellHook {
    pub label: &'static str,
    pub cmds: Vec<String>,
    pub dir: std::path::PathBuf,
}
impl Handler for ShellHook {
    fn run(&self, _ctx: &mut LoopState) -> Result<Flow> {
        self.fire();
        Ok(Flow::Continue)
    }
    fn fire(&self) {
        crate::hooks::run(self.label, &self.cmds, &self.dir);
    }
    fn name(&self) -> &'static str {
        self.label
    }
}
