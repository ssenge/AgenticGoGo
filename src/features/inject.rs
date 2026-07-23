//! The `inject` feature group: the on_session_start handlers (the old INJECT stage).

use anyhow::Result;
use crate::loop_::{AGGState, AGGScratch, Flow, Handler, LoopState, wait_for_resume};
use crate::bus::Command;
use crate::core::stop::{self, StopContext};

/// Drain the operator bus at the session boundary (inject / pause / set-budget / stop / note).
pub struct BusDrain;
impl Handler for BusDrain {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let cmds = match &ctx.bus {
            Some(bus) => bus.drain(),
            None => Vec::new(),
        };
        for cmd in cmds {
            match cmd {
                Command::InjectInstruction { text } => {
                    eprintln!("  [bus] inject-instruction → prepended to next session");
                    let op = &mut ctx.ext.get::<AGGState>().operator;
                    op.pending_instruction = Some(match op.pending_instruction.take() {
                        Some(prev) => format!("{prev}\n\n{text}"),
                        None => text,
                    });
                }
                Command::SetBudget { total } => {
                    eprintln!("  [bus] set-budget → {:?}", total);
                    ctx.budget_total = total;
                }
                Command::Pause => {
                    eprintln!("  [bus] pause → waiting for resume/stop…");
                    let stopped = match &ctx.bus {
                        Some(bus) => wait_for_resume(bus),
                        None => None,
                    };
                    if let Some(reason) = stopped {
                        return Ok(Flow::Stop(ctx.stopped_via_bus(reason)));
                    }
                }
                Command::Resume => {}
                Command::Stop { reason } => return Ok(Flow::Stop(ctx.stopped_via_bus(reason))),
                Command::Note { text } => eprintln!("  [bus] note: {text}"),
            }
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "BusDrain"
    }
}

/// Advance the sequence cursor → resolve the next step; then (ONLY on a resolved step) bump the
/// session counter, update the ledger, print the banner, and set `cur_step` + `scratch.skip_judges`.
pub struct PickStep;
impl Handler for PickStep {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let step_name = {
            let rs = ctx.run_state();
            let eng = &ctx.eng;
            let picked = ctx.cursor.next_step(&mut |cond| {
                let sc = StopContext {
                    judges: &eng.judges,
                    judge_errors: &[],
                    tokens_spent: rs.tokens_spent,
                    budget_total: rs.budget_total,
                    cost_spent: rs.cost_spent,
                    cost_limit: rs.cost_limit,
                    sessions_done: rs.sessions_done,
                    max_sessions: rs.max_sessions,
                    wall_hours: rs.wall_hours,
                };
                stop::evaluate(cond, &sc)
            });
            match picked {
                Ok(n) => n,
                Err(e) => return Ok(Flow::Stop(ctx.abort_now(&format!("sequence error: {e}")))),
            }
        };
        let step = match ctx.cfg.resolve_step(&step_name) {
            Ok(s) => s,
            Err(e) => return Ok(Flow::Stop(ctx.abort_now(&format!("{e}")))),
        };

        ctx.session += 1;
        ctx.dash.session = ctx.session;
        ctx.dash.lifetime_session = ctx.lifetime_base + ctx.session;
        let (gm, gt) = ctx.eng.tally();
        ctx.ledger.update(ctx.session, ctx.tokens_spent, gm, gt);
        let up = ctx.loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{} (#{} lifetime)  step `{}` [{}]  (up {}h{:02}m)  goals {gm}/{gt} ────",
            ctx.session,
            ctx.dash.lifetime_session,
            step.name,
            step.agent,
            up / 3600,
            (up % 3600) / 60,
        );
        // skip_judges into the channel BEFORE cur_step is moved (later hooks read the channel).
        ctx.scratch.get::<AGGScratch>().skip_judges = step.skip_judges;
        ctx.cur_step = Some(step);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "PickStep"
    }
}

/// Cut this session's git branch off the span tip (or base).
pub struct SessionBranchCut;
impl Handler for SessionBranchCut {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let base_ref = ctx.base_ref(); // owned; ends the &mut-self borrow before we touch cfg below
        let iso = &ctx.cfg.session_isolation;
        let br = crate::git::session_branch(&iso.branch_prefix, &ctx.cfg.project, ctx.session);
        crate::git::remove_file(ctx.dir, &iso.red_file); // clear a stale veto
        ctx.ext.get::<AGGState>().git.session_branch = if crate::git::create_branch(ctx.dir, &br, &base_ref) {
            eprintln!("  [iso] session #{} on branch {br} (off {base_ref})", ctx.session);
            Some(br)
        } else {
            eprintln!("  [iso] could not create session branch — running on {base_ref}");
            None
        };
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "SessionBranchCut"
    }
}

/// Capture the state file (for the staleness warning) + compose the brief into `INSTRUCTIONS.md`;
/// the tiny pointer (or the inline brief on a write failure) goes to `scratch.prompt`.
pub struct WriteInstructions;
impl Handler for WriteInstructions {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(change) = crate::os::spawns::scan(ctx.dir) {
            eprintln!("  [spawn] {change}");
        }
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let state_path = ctx.config_base.join(&step.state);
        ctx.ext.get::<AGGState>().inject.state_before = std::fs::read_to_string(&state_path).ok();
        let prompt = ctx.compose_prompt(&step);
        ctx.scratch.get::<AGGScratch>().prompt = Some(prompt);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "WriteInstructions"
    }
}

/// Reset the on-disk per-session memory scratch for the fresh session.
pub struct ClearMemScratch;
impl Handler for ClearMemScratch {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if ctx.cfg.memory.enabled {
            crate::core::memory::clear_scratch(ctx.dir, ctx.session);
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "ClearMemScratch"
    }
}
