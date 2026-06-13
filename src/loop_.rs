//! The harness loop: `agg run`.
//!
//! Each cycle: launch a fresh `claude -p` worker → stream its events to the log
//! (readable formatter) with a heartbeat and a watchdog → on exit, run all judges
//! → fold verdicts → check stop/halt → repeat or stop.
//!
//! This is the Rust port of a proven prior bespoke harness. The principle
//! "keep the LLM out of the loop" holds: the loop is plain code, the worker is a
//! fresh-context subprocess, judges are scripts/cheap LLM calls.

use crate::bus::{Bus, Command};
use crate::config::AggConfig;
use crate::engine::{Engine, RunState};
use crate::state::{DashboardState, LiveState};
use crate::summary;
use crate::worker;
use anyhow::{Context, Result};
use std::path::Path;
use std::time::{Duration, Instant};

/// Block until a `resume` or `stop` command arrives on the bus (poll every 2s).
/// A `stop` returns so the caller's next drain sees it and exits.
fn wait_for_resume(bus: &Bus) {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        for cmd in bus.drain() {
            match cmd {
                Command::Resume => {
                    eprintln!("  [bus] resume → continuing");
                    return;
                }
                Command::Stop { reason } => {
                    eprintln!("  [bus] stop while paused → {reason}");
                    // re-queue a stop is not possible (drained); signal via a note file
                    let _ = bus.emit("stop", &reason, "paused-stop");
                    std::process::exit(0);
                }
                other => eprintln!("  [bus] (paused) ignoring {other:?} until resume"),
            }
        }
    }
}

/// Runs `on_stop` hooks exactly once, on ANY exit from the loop (success, halt, bus-stop,
/// max-sessions, or an early `?` error) — via Drop, so we don't have to thread it through
/// every return site.
struct StopHooks<'a> {
    cmds: Vec<String>,
    dir: &'a Path,
}
impl Drop for StopHooks<'_> {
    fn drop(&mut self) {
        if !self.cmds.is_empty() {
            crate::hooks::run("on_stop", &self.cmds, self.dir);
        }
    }
}

/// Clears `.agg/run.pid` on any loop exit (clean, error, or panic) so a later `agg stop`
/// never targets a dead pid and the double-run guard never falsely reports a live loop.
struct RunPidGuard<'a> {
    dir: &'a Path,
}
impl Drop for RunPidGuard<'_> {
    fn drop(&mut self) {
        crate::detach::clear_run_pid(self.dir);
    }
}

pub fn run(cfg: AggConfig, mut eng: Engine, dir: &Path, max_sessions: u32) -> Result<()> {
    // Record THIS process as the live loop so `agg stop` / the double-run guard read a
    // current pid. Covers BOTH foreground `agg run` and the detached child (which re-runs
    // `agg run` for real) — the child overwrites the launcher's pid with its own.
    crate::detach::write_run_pid(dir);
    let _run_pid_guard = RunPidGuard { dir };

    let resume_prompt = std::fs::read_to_string(dir.join(&cfg.resume_prompt))
        .with_context(|| format!("reading resume prompt {}", cfg.resume_prompt))?;

    // ---- lifecycle hooks (tool-agnostic): on_start once, background watchers spawned now,
    //      on_stop guaranteed on any exit via the Drop guard. ----
    crate::hooks::run("on_start", &cfg.hooks.on_start, dir);
    crate::hooks::spawn_background(&cfg.hooks.background, dir);
    let _stop_hooks = StopHooks { cmds: cfg.hooks.on_stop.clone(), dir };
    // prompt-include fragments, composed once (the resume prompt is read once at launch too).
    let prompt_prefix = crate::hooks::gather_prompt_includes(&cfg.prompt_includes, dir);

    let loop_start = Instant::now();
    let (m, t) = eng.tally();
    eprintln!(
        "════════════════════════════════════════════════════════════\n\
         AgenticGoGo — project {}  model {}\n\
         goals {m}/{t}  stop_when: {}\n\
         ════════════════════════════════════════════════════════════\n\
         ▶ watch live:  run `agg dashboard` in another terminal\n\
         ⏱ first session may take a minute — the worker is warming up, not hung\n\
         ⏹ stop anytime: `agg stop`",
        cfg.project, cfg.model, eng.stop_when
    );

    // run-level accounting for budget/wall-time guards. `budget_total` is mutable:
    // the bus `set-budget` command can change it mid-run.
    let mut tokens_spent: u64 = 0;
    let mut budget_total = cfg.budget.total;
    let run_state = |toks: u64, budget: Option<u64>| RunState {
        tokens_spent: toks,
        budget_total: budget,
        wall_hours: loop_start.elapsed().as_secs_f64() / 3600.0,
    };

    // ---- dashboard state (Phase 4): the loop + worker publish a compact snapshot to
    //      .agg/state.json; `agg dashboard` renders it. Two-stream discipline:
    //      the stdout log above stays the source of truth; this is just a view.
    //
    //      Single-writer-under-lock: ONE shared `LiveState` is mutated by both the loop
    //      (boundary updates: phase/session/goals/summaries) and the worker's reader
    //      thread (live updates: now/think/recent/idle, mid-session). `dash` below is
    //      the loop's working copy of the fields IT owns; `publish!` folds those into
    //      the shared state without clobbering the worker-owned live fields. ----
    let mut dash = DashboardState {
        project: cfg.project.clone(),
        model: cfg.model.clone(),
        stop_when: eng.stop_when.clone(),
        halt_when: eng.halt_when.clone().unwrap_or_default(),
        budget_total: cfg.budget.total,
        phase: "starting".into(),
        ..Default::default()
    };
    let live = LiveState::new(dir, loop_start, dash.clone());
    macro_rules! publish {
        () => {{
            dash.up_secs = loop_start.elapsed().as_secs();
            dash.tokens_spent = tokens_spent;
            let (m, t) = eng.tally();
            dash.goals_met = m;
            dash.goals_total = t;
            dash.goals = DashboardState::goals_from_engine(&eng, &dash.goals);
            // fold the loop-owned fields into the shared snapshot; leave the
            // worker-owned live fields (now/think/recent/idle_secs) untouched.
            live.update(|s| {
                s.project = dash.project.clone();
                s.model = dash.model.clone();
                s.stop_when = dash.stop_when.clone();
                s.halt_when = dash.halt_when.clone();
                s.tokens_spent = dash.tokens_spent;
                s.budget_total = dash.budget_total;
                s.session = dash.session;
                s.phase = dash.phase.clone();
                s.goals_met = dash.goals_met;
                s.goals_total = dash.goals_total;
                s.goals = dash.goals.clone();
                s.summary_cumulative = dash.summary_cumulative.clone();
                s.summary_windowed = dash.summary_windowed.clone();
                s.finished = dash.finished;
                s.finish_reason = dash.finish_reason.clone();
            });
        }};
    }
    publish!();

    // Evaluate the goals ONCE up front (run the judges) — maybe we're already done,
    // or an invariant is already broken, before burning a single session.
    eprintln!("  baseline: running judges once before the first session…");
    dash.phase = "judging".into();
    publish!();
    let pre = eng.evaluate_cycle(dir, &run_state(tokens_spent, budget_total));
    eprint!("{}", indent(&eng.scoreboard()));
    publish!();
    if pre.halt {
        eprintln!("⚠ HALT at baseline — guard already true: {}", pre.halt_reason.clone().unwrap_or_default());
        dash.phase = "done".into();
        dash.finished = true;
        dash.finish_reason = format!("HALT at baseline: {}", pre.halt_reason.unwrap_or_default());
        publish!();
        return Ok(());
    }
    if pre.stop {
        eprintln!("✔ stop condition already satisfied at launch — nothing to do.");
        dash.phase = "done".into();
        dash.finished = true;
        dash.finish_reason = "already satisfied at launch".into();
        publish!();
        return Ok(());
    }

    // summarizer state: the rolling cumulative summary + last-summary timestamp.
    let mut cumulative = String::new();
    let mut last_summary = Instant::now() - std::time::Duration::from_secs(cfg.summary.min_interval_secs);

    // ---- bus (Phase 6): operator/outer-Claude steering, drained at each session
    //      boundary (the only safe injection point for headless workers). ----
    let bus = Bus::open(dir).ok();
    let mut pending_instruction: Option<String> = None; // prepended to next prompt
    let mut last_session_id: Option<String> = None;      // for optional --resume continuity

    let mut session = 0u32;
    // Persistent project run-history ledger (.agg/project.json): append an
    // in-flight record for THIS run, finalized on any exit via the Drop guard
    // below. The lifetime session total (shown on the dashboard so a restart
    // doesn't look like the work started over) is derived from prior runs in the
    // ledger; `session` (per-run) still drives --resume/labels.
    let mut ledger = crate::project::RunLedger::begin(
        dir,
        &cfg.project,
        std::process::id(),
        now_epoch(),
    );
    let lifetime_base = ledger.prior_lifetime_sessions();
    dash.lifetime_session = lifetime_base;

    // ── per-session git isolation (opt-in) ────────────────────────────────────────────────
    // Capture the base branch ONCE at startup. Each session runs on its own branch off this
    // base and is merged back UNLESS the worker vetoed it (red file). Disabled cleanly if the
    // repo isn't in a usable state (not a repo / detached HEAD / dirty tree) — isolation is an
    // enhancement, never a correctness requirement.
    let iso = &cfg.session_isolation;
    let iso_base: Option<String> = if iso.enabled {
        if !crate::git::is_repo(dir) {
            eprintln!("  [iso] session_isolation enabled but not a git repo — running on current branch");
            None
        } else if !crate::git::is_clean(dir) {
            eprintln!("  [iso] session_isolation enabled but work tree has tracked changes — commit/stash first; running on current branch");
            None
        } else {
            // keep agg's runtime state out of git so it never lands on session branches / base.
            crate::git::ensure_agg_gitignored(dir);
            let base = if iso.base_branch.is_empty() {
                crate::git::current_branch(dir)
            } else {
                Some(iso.base_branch.clone())
            };
            match &base {
                Some(b) => eprintln!("  [iso] per-session branch isolation ON — base branch '{b}', merge unless '{}' present", iso.red_file),
                None => eprintln!("  [iso] session_isolation enabled but HEAD is detached — running on current branch"),
            }
            base
        }
    } else {
        None
    };
    loop {
        if max_sessions != 0 && session >= max_sessions {
            eprintln!("→ reached max_sessions={max_sessions}; stopping (goals not all met).");
            let (gm, gt) = eng.tally();
            ledger.update(session, tokens_spent, gm, gt);
            ledger.finish(now_epoch(), "max-sessions");
            break;
        }

        // ── drain the bus at the session boundary; apply steering commands ──
        if let Some(bus) = &bus {
            for cmd in bus.drain() {
                match cmd {
                    Command::InjectInstruction { text } => {
                        eprintln!("  [bus] inject-instruction → prepended to next session");
                        pending_instruction = Some(match pending_instruction.take() {
                            Some(prev) => format!("{prev}\n\n{text}"),
                            None => text,
                        });
                    }
                    Command::SetBudget { total } => {
                        eprintln!("  [bus] set-budget → {:?}", total);
                        budget_total = total;
                    }
                    Command::Pause => {
                        eprintln!("  [bus] pause → waiting for resume/stop…");
                        wait_for_resume(bus);
                    }
                    Command::Resume => { /* a stray resume with no pause: ignore */ }
                    Command::Stop { reason } => {
                        eprintln!("  [bus] stop → {reason}");
                        dash.phase = "done".into();
                        dash.finished = true;
                        dash.finish_reason = format!("stopped via bus: {reason}");
                        let (gm, gt) = eng.tally();
                        ledger.update(session, tokens_spent, gm, gt);
                        ledger.finish(now_epoch(), "stopped");
                        publish!();
                        return Ok(());
                    }
                    Command::Note { text } => eprintln!("  [bus] note: {text}"),
                }
            }
        }

        session += 1;
        dash.session = session;
        // bump + persist this run's record so a later `agg run` continues the count
        // and the dashboard's lifetime total stays live across restarts.
        dash.lifetime_session = lifetime_base + session;
        let (gm, gt) = eng.tally();
        ledger.update(session, tokens_spent, gm, gt);
        let up = loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{session} (#{} lifetime)  (up {}h{:02}m)  goals {}/{} ────",
            dash.lifetime_session,
            up / 3600,
            (up % 3600) / 60,
            eng.tally().0,
            eng.tally().1
        );

        // ── isolation: cut this session's branch off the base + clear any stale red veto ──
        // `session_branch` is Some(name) only when isolation is active AND the branch was
        // created cleanly; otherwise the session runs on the current branch as before.
        let session_branch: Option<String> = match &iso_base {
            Some(base) => {
                let br = crate::git::session_branch(&iso.branch_prefix, &cfg.project, session);
                crate::git::remove_file(dir, &iso.red_file); // clear stale veto before the run
                if crate::git::create_branch(dir, &br, base) {
                    eprintln!("  [iso] session #{session} on branch {br} (off {base})");
                    Some(br)
                } else {
                    eprintln!("  [iso] could not create session branch — running on {base}");
                    None
                }
            }
            None => None,
        };

        // on_session_start hooks (e.g. incremental refresh of a code graph / cache).
        crate::hooks::run("on_session_start", &cfg.hooks.on_session_start, dir);

        // Layer-3 spawn scanner: flip finished long-tasks to "done", prune stale entries.
        // Runs every boundary (the harness's natural tick) — autonomous-safe, only updates
        // liveness of tasks WE registered; never kills a process it can't prove is ours.
        if let Some(change) = crate::spawns::scan(dir) {
            eprintln!("  [spawn] {change}");
        }

        // 1) build the effective prompt: [operator instruction] + [spawn status] +
        //    [prompt_includes] + resume. The operator instruction (if any) is consumed once;
        //    the spawn status tells this session about background tasks left running by a
        //    prior session (so it polls instead of relaunching); the prompt_includes prefix
        //    is the user's reusable tooling/guidance fragments.
        let base = if prompt_prefix.is_empty() {
            resume_prompt.clone()
        } else {
            format!("{prompt_prefix}\n\n{resume_prompt}")
        };
        // prepend any tracked background-task status so the worker sees what is pending + why.
        let base = match crate::spawns::summary_for_prompt(dir) {
            Some(status) => format!("{status}\n{base}"),
            None => base,
        };
        let effective_prompt = match pending_instruction.take() {
            Some(instr) => format!(
                "═══ HIGH-PRIORITY OPERATOR INSTRUCTION (act on this FIRST, it overrides the default plan) ═══\n\
                 {instr}\n\n{base}"
            ),
            None => base,
        };
        // NOTE: an `ultracode` prompt prefix was tried (to let the headless worker
        // spawn subagent Workflows) and REMOVED 2026-06-10. In `claude -p` headless
        // mode the worker fired an async Workflow then PARKED itself waiting for a
        // re-invoke that never comes (Workflow returns a task-id immediately), going
        // idle ~0% CPU until the watchdog killed it — a pure delegate-and-wait stall
        // for zero output. The work here is single-instance + sequential and does
        // not need fan-out, so the worker does it DIRECTLY (inline) instead.
        dash.phase = "running".into();
        publish!();
        // --resume continuity (opt-in): continue the prior session's context. Default
        // is fresh-context-per-session (the core no-runaway-cost discipline).
        let resume_id = if cfg.resume_sessions { last_session_id.as_deref() } else { None };
        let outcome = worker::run_session(&cfg, &effective_prompt, dir, session, resume_id, &live);
        last_session_id = outcome.session_id.clone();
        tokens_spent += outcome.output_tokens;
        // (run_session now reaps any straggler in the worker's process group on exit, and the
        // worker's reader thread already streamed `now`/`think`/`recent` live — nothing to do here.)
        eprintln!(
            "  session #{session} exited (code {:?}) after {}s{}{}  (+{} out-tok, {} total)",
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            if outcome.killed_by_watchdog { "  [WATCHDOG-KILLED: hung worker]" } else { "" },
            outcome.output_tokens,
            tokens_spent,
        );

        // ── isolation: resolve the session branch (DEFAULT MERGE, unless the worker vetoed) ──
        // The worker committed to `session_branch`. Default is to merge it back into the base.
        // If the worker wrote the red file, it vetoed this session ⇒ discard the branch, base
        // untouched. A crashed/killed worker that never wrote the red file still merges its
        // partial commits (default-merge). Judges below then run on the resolved base state.
        if let (Some(base), Some(br)) = (&iso_base, &session_branch) {
            let vetoed = crate::git::file_exists(dir, &iso.red_file);
            // back to base before merge/discard (git ops require not being on the branch we delete).
            let on_base = crate::git::checkout(dir, base);
            if !on_base {
                eprintln!("  [iso] WARNING could not checkout base '{base}'; leaving session branch {br} in place");
            } else if vetoed {
                eprintln!("  [iso] session #{session} VETOED (worker wrote {}) → discarding branch {br}", iso.red_file);
                crate::git::remove_file(dir, &iso.red_file); // don't let the veto persist on base
                crate::git::delete_branch(dir, br);
            } else {
                let msg = format!("agg: merge session #{session} ({br})");
                if crate::git::merge_no_ff(dir, br, &msg) {
                    eprintln!("  [iso] session #{session} merged → {base}");
                    crate::git::delete_branch(dir, br);
                } else {
                    eprintln!("  [iso] session #{session} merge FAILED (conflict) — branch {br} kept for inspection, base unchanged");
                }
            }
        }

        // 2) rate-limit backoff (exit-code + terminal-event gated).
        if outcome.rate_limited {
            let secs = cfg.ratelimit_backoff_secs;
            eprintln!("  rate limit detected — backing off {secs}s");
            dash.phase = "backoff".into();
            publish!();
            std::thread::sleep(std::time::Duration::from_secs(secs));
            continue; // don't judge on a rate-limited (incomplete) session
        }

        // 3) run judges, fold verdicts, evaluate conditions (incl. budget/wall guards).
        eprintln!("  running judges…");
        dash.phase = "judging".into();
        publish!();
        let res = eng.evaluate_cycle(dir, &run_state(tokens_spent, budget_total));
        eprint!("{}", indent(&eng.scoreboard()));
        publish!();

        // on_session_end hooks run AFTER judging, so they see the post-cycle state (e.g.
        // persist a memory note, update an index, refresh a graph for the next session).
        crate::hooks::run("on_session_end", &cfg.hooks.on_session_end, dir);

        // 4) LLM summary (cumulative + windowed), rate-limited by min_interval_secs.
        //    Best-effort: a summarizer failure NEVER breaks the loop.
        if cfg.summary.enabled
            && last_summary.elapsed().as_secs() >= cfg.summary.min_interval_secs
        {
            if let Some(s) = summary::summarize(
                &cfg.summary.model,
                &cumulative,
                &outcome.thoughts,
                &res.deltas,
                120,
            ) {
                eprintln!("  [SUMMARY cumulative] {}", s.cumulative);
                eprintln!("  [SUMMARY windowed]   {}", s.windowed);
                cumulative = s.cumulative.clone();
                dash.summary_cumulative = s.cumulative;
                dash.summary_windowed = s.windowed;
                last_summary = Instant::now();
                publish!();
            }
        }

        if res.halt {
            let reason = res.halt_reason.unwrap_or_default();
            eprintln!(
                "\n⚠ HALT — guard condition true: {reason}\n  stopping the loop (this is a guard, not success)."
            );
            dash.phase = "done".into();
            dash.finished = true;
            dash.finish_reason = format!("HALT: {reason}");
            let (gm, gt) = eng.tally();
            ledger.update(session, tokens_spent, gm, gt);
            ledger.finish(now_epoch(), &format!("halt:{reason}"));
            publish!();
            break;
        }
        if res.stop {
            let (mt, tt) = eng.tally();
            eprintln!("\n✔ STOP condition satisfied — {mt}/{tt} goals met. Done after {session} session(s).");
            dash.phase = "done".into();
            dash.finished = true;
            dash.finish_reason = format!("{mt}/{tt} goals met after {session} session(s)");
            ledger.update(session, tokens_spent, mt, tt);
            ledger.finish(now_epoch(), "goals-met");
            publish!();
            break;
        }
    }
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
