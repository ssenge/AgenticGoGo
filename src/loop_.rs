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
use crate::state::DashboardState;
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

pub fn run(cfg: AggConfig, mut eng: Engine, dir: &Path, max_sessions: u32) -> Result<()> {
    let resume_prompt = std::fs::read_to_string(dir.join(&cfg.resume_prompt))
        .with_context(|| format!("reading resume prompt {}", cfg.resume_prompt))?;

    let loop_start = Instant::now();
    let (m, t) = eng.tally();
    eprintln!(
        "════════════════════════════════════════════════════════════\n\
         AgenticGoGo — project {}  model {}\n\
         goals {m}/{t}  stop_when: {}\n\
         ════════════════════════════════════════════════════════════",
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

    // ---- dashboard state (Phase 4): the loop publishes a compact snapshot to
    //      .agg/state.json; `agg dashboard` renders it. Two-stream discipline:
    //      the stdout log above stays the source of truth; this is just a view. ----
    let mut dash = DashboardState {
        project: cfg.project.clone(),
        stop_when: eng.stop_when.clone(),
        budget_total: cfg.budget.total,
        phase: "starting".into(),
        ..Default::default()
    };
    let mut seq = 0u64;
    macro_rules! publish {
        () => {{
            seq += 1;
            dash.seq = seq;
            dash.up_secs = loop_start.elapsed().as_secs();
            dash.tokens_spent = tokens_spent;
            let (m, t) = eng.tally();
            dash.goals_met = m;
            dash.goals_total = t;
            dash.goals = DashboardState::goals_from_engine(&eng, &dash.goals);
            let _ = dash.write(dir);
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
    loop {
        if max_sessions != 0 && session >= max_sessions {
            eprintln!("→ reached max_sessions={max_sessions}; stopping (goals not all met).");
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
                        publish!();
                        return Ok(());
                    }
                    Command::Note { text } => eprintln!("  [bus] note: {text}"),
                }
            }
        }

        session += 1;
        dash.session = session;
        let up = loop_start.elapsed().as_secs();
        eprintln!(
            "\n──── session #{session}  (up {}h{:02}m)  goals {}/{} ────",
            up / 3600,
            (up % 3600) / 60,
            eng.tally().0,
            eng.tally().1
        );

        // 1) build the effective prompt: prepend any operator-injected instruction
        //    (consumed once), then launch the worker (streams/heartbeat/watchdog).
        let effective_prompt = match pending_instruction.take() {
            Some(instr) => format!(
                "═══ HIGH-PRIORITY OPERATOR INSTRUCTION (act on this FIRST, it overrides the default plan) ═══\n\
                 {instr}\n\n{resume_prompt}"
            ),
            None => resume_prompt.clone(),
        };
        dash.phase = "running".into();
        publish!();
        // --resume continuity (opt-in): continue the prior session's context. Default
        // is fresh-context-per-session (the core no-runaway-cost discipline).
        let resume_id = if cfg.resume_sessions { last_session_id.as_deref() } else { None };
        let outcome = worker::run_session(&cfg, &effective_prompt, dir, session, resume_id);
        last_session_id = outcome.session_id.clone();
        tokens_spent += outcome.output_tokens;
        // surface the worker's last thought as the dashboard "think" line
        if let Some(last) = outcome.thoughts.last() {
            dash.think = last.clone();
        }
        eprintln!(
            "  session #{session} exited (code {:?}) after {}s{}  (+{} out-tok, {} total)",
            outcome.exit_code,
            outcome.duration_secs,
            if outcome.rate_limited { "  [RATE-LIMITED]" } else { "" },
            outcome.output_tokens,
            tokens_spent,
        );

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
            publish!();
            break;
        }
        if res.stop {
            let (mt, tt) = eng.tally();
            eprintln!("\n✔ STOP condition satisfied — {mt}/{tt} goals met. Done after {session} session(s).");
            dash.phase = "done".into();
            dash.finished = true;
            dash.finish_reason = format!("{mt}/{tt} goals met after {session} session(s)");
            publish!();
            break;
        }
    }
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}
