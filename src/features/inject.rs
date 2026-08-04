//! The `inject` feature group: the on_session_start handlers (the old INJECT stage).

use std::path::Path;

use anyhow::Result;
use crate::bus::Command;
use crate::core::config::ResolvedStep;
use crate::core::stop::{self, StopContext};
use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LoopState, EXIT_FOOTER, INSTRUCTIONS_POINTER, wait_for_resume};

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
                Command::Resume => ctx.ext.get::<AGGState>().operator.resumed = true,
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
///
/// A Rust driver hands its step in through `ctx.next_step`, which is consulted FIRST — the cursor,
/// the `if`-conditions and `resolve_step` are then never reached on that path (BUILD.md §3.8).
pub struct PickStep;
impl Handler for PickStep {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        if let Some(step) = ctx.next_step.take() {
            announce(ctx, step);
            return Ok(Flow::Continue);
        }
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
        announce(ctx, step);
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "PickStep"
    }
}

/// The half of `PickStep` that runs once a step is RESOLVED, whichever path resolved it: bump the
/// session counter, update the ledger, print the banner, and publish the step into the state.
fn announce(ctx: &mut LoopState, step: ResolvedStep) {
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
}

/// Cut this session's git branch off the span tip (or base).
pub struct SessionBranchCut;
impl Handler for SessionBranchCut {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let base_ref = base_ref(ctx); // owned; ends the &mut-self borrow before we touch cfg below
        let iso = &ctx.cfg.session_isolation;
        let br = crate::git::session_branch(&iso.branch_prefix, &ctx.cfg.project, ctx.session);
        crate::git::remove_file(&ctx.dir, &iso.red_file); // clear a stale veto
        ctx.ext.get::<AGGState>().git.session_branch = if crate::git::create_branch(&ctx.dir, &br, &base_ref) {
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
        if let Some(change) = crate::os::spawns::scan(&ctx.dir) {
            eprintln!("  [spawn] {change}");
        }
        let step = ctx.cur_step.clone().expect("PickStep set cur_step");
        let state_path = ctx.config_base.join(&step.state);
        ctx.ext.get::<AGGState>().inject.state_before = std::fs::read_to_string(&state_path).ok();
        let prompt = compose_prompt(ctx, &step);
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
            crate::core::memory::clear_scratch(&ctx.dir, ctx.session);
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "ClearMemScratch"
    }
}

/// Compose the worker's whole brief into `agg/private/INSTRUCTIONS.md`, then return the tiny fixed
/// pointer that becomes the actual `-p` value (§2/§3). Highest-priority first — operator steering,
/// then the task (role + `prompt:`), then context pointers/excerpts (memory tail → STATE → AGG.md →
/// wiki), then the standing footer. Long files are POINTED at or excerpted so agg keeps the context
/// budget bounded. On a write failure it falls back to returning the brief inline, arg-safe.
pub fn compose_prompt(ctx: &mut LoopState, step: &ResolvedStep) -> String {
    let mut s = String::new();
    s.push_str(
        "<!-- agg/private/INSTRUCTIONS.md — WRITTEN BY agg, REGENERATED every session. Do not edit; it is overwritten. -->\n\n",
    );
    let agent = &step.agent;
    s.push_str(&format!("# Session {} · step `{}` · agent `{agent}`\n", ctx.session, step.name));

    // ── operator steering — highest priority, act on it FIRST. The banner keeps the phrase
    //    "HIGH-PRIORITY OPERATOR INSTRUCTION" so the memory sanitizer (`looks_like_marker`) still
    //    de-fangs a worker note that tries to forge it. ──
    if let Some(instr) = ctx.ext.get::<AGGState>().operator.pending_instruction.take() {
        s.push_str(&format!(
            "\n## ⚠ HIGH-PRIORITY OPERATOR INSTRUCTION — do this FIRST (it overrides the default plan)\n{instr}\n"
        ));
    }
    if let Some(status) = crate::os::spawns::summary_for_prompt(&ctx.dir) {
        s.push_str(&format!("\n{status}\n"));
    }

    // ── the task: the step's ROLE framing (config-driven, §4) + its specific `prompt:` ──
    if let Some(rp) = &step.role_prompt {
        if !rp.trim().is_empty() {
            s.push_str(&format!("\n## Your role this session\n{}\n", rp.trim()));
        }
    }
    if let Some(p) = &step.prompt {
        if !p.trim().is_empty() {
            s.push_str(&format!("\n## This session — do ONE focused chunk\n{}\n", p.trim()));
        }
    }
    let prompt_prefix = ctx.ext.get::<AGGState>().inject.prompt_prefix.clone();
    if !prompt_prefix.is_empty() {
        s.push_str(&format!("\n{}\n", prompt_prefix.trim()));
    }

    // ── context: memory recent-tail excerpt + a conditional pointer to the full LOG ──
    if ctx.cfg.memory.enabled {
        let last_session = ctx.ext.get::<AGGState>().memory.last_session.clone();
        let mem = crate::core::memory::read_block(&ctx.dir, &last_session, ctx.cfg.memory.inject_kb);
        if !mem.trim().is_empty() {
            s.push_str(&format!("\n## What's been tried\n{}\n", mem.trim()));
            s.push_str(
                "Full history in `agg/private/LOG.md` — read it ONLY if you need older detail; it is long, don't load it all.\n",
            );
        }
    }

    // ── STATE → a POINTER, not an excerpt (it is crisp by design; read the whole small file) ──
    if let Ok(st) = std::fs::read_to_string(ctx.config_base.join(&step.state)) {
        if !st.trim().is_empty() {
            s.push_str(&format!(
                "\n## Where things stand\nRead `agg/{}` — your predecessor's forward advice (kept short; read it in full).\n",
                step.state
            ));
        }
    }

    // ── the instructions file → a POINTER (the standing project instructions), never its bytes ──
    if ctx.dir.join(&ctx.cfg.instructions).exists() {
        s.push_str(&format!(
            "\n## Project instructions\nRead `{}` — the standing scope, architecture, and rules for this project.\n",
            ctx.cfg.instructions
        ));
    }

    // ── the LLM wiki — list its pages if any exist ──
    let wiki = crate::paths::wiki_dir(&ctx.dir);
    if wiki.exists() {
        let pages = wiki_pages(&wiki);
        if !pages.is_empty() {
            s.push_str(&format!(
                "\n## Knowledge base\nConsult and maintain the durable wiki at `agg/state/wiki/` (start with {}).\n",
                pages.join(", ")
            ));
        }
    }

    // ── standing footer (from plugin/scaffold/exit_footer.md). {{STATE}} is filled from step.state. ──
    s.push('\n');
    s.push_str(&EXIT_FOOTER.replace("{{STATE}}", &step.state));

    // write the composed brief to disk; the worker's actual `-p` is the tiny pointer.
    let path = crate::paths::instructions_md(&ctx.dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &s) {
        Ok(()) => INSTRUCTIONS_POINTER.to_string(),
        Err(e) => {
            eprintln!("  ⚠ could not write {} ({e}); passing the brief inline this session", path.display());
            if s.starts_with('-') { format!("\n{s}") } else { s }
        }
    }
}

/// The wiki's page names (up to a handful), sorted, for the INSTRUCTIONS "start with …" hint. A pure
/// listing — an empty/absent dir yields no names and the hint is dropped. Caps at 5 so a large wiki
/// can't bloat the pointer; the worker sees the rest by opening the dir.
fn wiki_pages(wiki: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(wiki) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".md").then(|| format!("`wiki/{n}`"))
        })
        .collect();
    names.sort();
    names.truncate(5);
    names
}

/// The branch a JUDGED step's regression check reads / the branch a session is cut off. Base is the
/// resolved isolation base branch, unless a `skip_judges` span is in progress (then its tip).
pub fn base_ref(ctx: &mut LoopState) -> String {
    let git = &ctx.ext.get::<AGGState>().git;
    git.span_tip.clone().unwrap_or_else(|| git.iso_base.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_pages_lists_markdown_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("plan.md"), "x").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "x").unwrap();
        let pages = wiki_pages(tmp.path());
        assert_eq!(pages, ["`wiki/plan.md`"]);
    }
}
