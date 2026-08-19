//! `agg dashboard` — the live TUI. A separate viewer process that polls
//! `agg/private/state.json` (written by `agg run` + its worker) and repaints in place
//! with color.
//!
//! It is a *view*: it never drives the loop, never reads the firehose. If the
//! state file is missing it shows "waiting for `agg run`…". Quit with `q`/Esc.
//!
//! Layout (top→bottom):
//!   title  : "AgenticGoGo" only
//!   Info   : project · started · up · model · stop/halt · tokens/budget · session#
//!   Progress: a segmented blocks bar (custom, not the crude ratatui Gauge)
//!   Goals  : scrollable list, one detail line + wrapped rationale per goal
//!   Activity: real-time tail of recent 🔧/💬/↳ events (auto-follows unless scrolled)
//!   Summary: one cumulative-situation block (or the FINISHED banner)
//!
//! Keys: ↑/↓ scroll focused pane · PgUp/PgDn · g/G top/bottom · Tab switch focus
//!       (Goals↔Activity) · f toggle activity auto-follow · q/Esc quit.

use crate::state::{ActivityEvent, AgentUsage, DashboardState, JudgeView, Phase};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::{execute, terminal};
use ratatui::prelude::*;
use std::io::stdout;
use std::path::Path;
use std::time::Duration;

mod draw;
mod fmt;

/// Which scrollable pane currently has the keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    Judges,
    Activity,
}

/// Persistent UI state across repaints (scroll positions, focus, follow mode).
pub(super) struct DashboardUi {
    pub(super) focus: Focus,
    pub(super) judges_scroll: u16,
    pub(super) activity_scroll: u16,
    /// when true, the Activity pane sticks to the newest event (the default);
    /// scrolling up in Activity turns it off, `f` or scrolling to the bottom restores it.
    pub(super) activity_follow: bool,
    /// `Some(buffer)` while the user is typing an inject message (`i` enters this mode); `None`
    /// otherwise. In this mode keystrokes edit the buffer instead of driving the normal controls.
    pub(super) input: Option<String>,
    /// A transient confirmation/error line shown in the Summary panel after an inject is sent
    /// (`✓ …` / `✗ …`); cleared on the next keypress.
    pub(super) flash: Option<String>,
}

impl Default for DashboardUi {
    fn default() -> Self {
        DashboardUi {
            focus: Focus::Activity,
            judges_scroll: 0,
            activity_scroll: 0,
            activity_follow: true,
            input: None,
            flash: None,
        }
    }
}

pub fn run(dir: &Path) -> Result<()> {
    // The dashboard needs a real interactive terminal (raw mode). If stdin/stdout
    // isn't a TTY (piped, cron, CI), fail with a clear message instead of a cryptic
    // OS error — and point the user at the plain log, which always works.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        anyhow::bail!(
            "agg dashboard needs an interactive terminal.\n\
             Run it directly in a terminal (not piped/backgrounded).\n\
             The plain loop log on `agg run`'s stdout always works as a fallback."
        );
    }

    // ---- terminal setup ----
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut term = Terminal::new(backend)?;

    let res = event_loop(&mut term, dir);

    // ---- teardown (always, even on error) ----
    terminal::disable_raw_mode()?;
    execute!(term.backend_mut(), terminal::LeaveAlternateScreen)?;
    term.show_cursor()?;
    res
}

fn event_loop<B: Backend>(term: &mut Terminal<B>, dir: &Path) -> Result<()> {
    let mut ui = DashboardUi::default();
    loop {
        let state = DashboardState::read(dir);
        term.draw(|f| draw::draw(f, dir, state.as_ref(), &mut ui))?;

        // poll input ~4x/sec; repaint regardless to pick up state changes.
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                match handle_key(&mut ui, k.code) {
                    KeyAction::Quit => break,
                    // The only key that does I/O: send the typed instruction to the loop's bus,
                    // exactly as `agg send inject` does — the loop drains it at the next session.
                    KeyAction::Inject(text) => {
                        ui.flash = Some(
                            match crate::bus::queue_command(dir, &crate::bus::Command::InjectInstruction { text }) {
                                Ok(_) => "✓ injected — queued for the next session".to_string(),
                                Err(e) => format!("✗ inject failed: {e}"),
                            },
                        );
                    }
                    KeyAction::Continue => {}
                }
            }
        }
        // if the run finished, keep showing the final frame but allow q to exit.
    }
    Ok(())
}

/// What a handled key asks the event loop to do. `Inject` carries the typed text out of the pure
/// key handler so the loop (not the handler) does the bus write — keeping `handle_key` I/O-free and
/// unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyAction {
    Continue,
    Quit,
    Inject(String),
}

/// Apply one keypress to the UI state. Pure over `(ui, code)` — no terminal I/O — so the whole
/// interaction model (focus switching, scrolling, follow-mode, quit) is unit-testable without a
/// real TTY. The event loop just reads keys and calls this.
fn handle_key(ui: &mut DashboardUi, code: KeyCode) -> KeyAction {
    // ---- inject INPUT MODE: keystrokes edit the buffer, not the normal controls (so typing "q"
    //      does not quit). `i` in normal mode enters this; Enter sends, Esc cancels. ----
    if ui.input.is_some() {
        match code {
            KeyCode::Esc => ui.input = None, // cancel
            KeyCode::Enter => {
                let text = ui.input.take().unwrap_or_default().trim().to_string();
                if !text.is_empty() {
                    return KeyAction::Inject(text);
                } // empty ⇒ just close, no-op
            }
            KeyCode::Backspace => {
                if let Some(b) = ui.input.as_mut() {
                    b.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(b) = ui.input.as_mut() {
                    b.push(c);
                }
            }
            _ => {}
        }
        return KeyAction::Continue;
    }
    // any key in normal mode dismisses a lingering inject confirmation.
    ui.flash = None;
    match code {
        KeyCode::Char('i') => ui.input = Some(String::new()), // enter inject mode
        KeyCode::Char('q') | KeyCode::Esc => return KeyAction::Quit,
        KeyCode::Tab => {
            ui.focus = match ui.focus {
                Focus::Judges => Focus::Activity,
                Focus::Activity => Focus::Judges,
            };
        }
        KeyCode::Char('f') => {
            ui.activity_follow = !ui.activity_follow;
            // "pinned to the bottom" and "paused" are contradictory: `draw_activity` re-enables
            // follow for anything sitting at max_scroll (that's how scrolling back down resumes
            // it). Without stepping one line back, an explicit `f` pause was silently undone by
            // the very next repaint — `f` did nothing at all in the default, pinned state.
            if !ui.activity_follow {
                ui.activity_scroll = ui.activity_scroll.saturating_sub(1);
            }
        }
        KeyCode::Up => scroll(ui, -1),
        KeyCode::Down => scroll(ui, 1),
        KeyCode::PageUp => scroll(ui, -10),
        KeyCode::PageDown => scroll(ui, 10),
        KeyCode::Char('g') | KeyCode::Home => match ui.focus {
            Focus::Judges => ui.judges_scroll = 0,
            Focus::Activity => {
                ui.activity_scroll = 0;
                ui.activity_follow = false;
            }
        },
        KeyCode::Char('G') | KeyCode::End => match ui.focus {
            Focus::Judges => ui.judges_scroll = u16::MAX, // clamped at draw time
            Focus::Activity => ui.activity_follow = true,
        },
        _ => {}
    }
    KeyAction::Continue
}

/// Apply a relative scroll to the focused pane. Scrolling the Activity pane up
/// drops out of follow-mode; scrolling back to the top of follow re-enables it.
fn scroll(ui: &mut DashboardUi, delta: i32) {
    match ui.focus {
        Focus::Judges => {
            ui.judges_scroll = (ui.judges_scroll as i32 + delta).max(0) as u16;
        }
        Focus::Activity => {
            // in follow mode "scroll up" means "look back" → leave follow.
            if delta < 0 {
                ui.activity_follow = false;
            }
            ui.activity_scroll = (ui.activity_scroll as i32 + delta).max(0) as u16;
        }
    }
}

/// Render the dashboard headlessly to a ratatui [`Buffer`](ratatui::buffer::Buffer) at a fixed
/// size, using the REAL `draw()` path — so a captured image can never drift from what the live
/// TUI shows. Used by `examples/dashboard_svg.rs` to generate the README screenshot.
#[doc(hidden)]
pub fn render_buffer(state: &DashboardState, w: u16, h: u16) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    let mut ui = DashboardUi::default();
    term.draw(|f| draw::draw(f, Path::new("."), Some(state), &mut ui)).unwrap();
    term.backend().buffer().clone()
}

/// A representative snapshot for documentation/screenshots — a mixed claude+codex run mid-sequence,
/// in a `reconsider` (staging) step: a binary invariant holding, a numeric judge climbing, a broken
/// judge surfaced, and the run-set `stalled` control judge that triggered the reconsider.
#[doc(hidden)]
pub fn sample_state() -> DashboardState {
    let mut per_agent = std::collections::BTreeMap::new();
    per_agent.insert("claude".to_string(), AgentUsage { tokens: 903_411, cost: Some(7.12) });
    per_agent.insert("codex".to_string(), AgentUsage { tokens: 381_491, cost: Some(1.05) });
    DashboardState {
        project: "telos".into(),
        model: "claude-opus-4-8".into(),
        step: "reconsider".into(),
        step_agent: "claude".into(),
        step_model: "claude-opus-4-8".into(),
        pos: "cycle 3/20 › attempt 2/3".into(),
        stop_when: "all_goals".into(),
        halt_when: "session > 40 OR over_cost".into(),
        iso_base: "main".into(),
        started_at_epoch: 1_751_000_000,
        up_secs: 5_231, // 1h27m
        session: 3,
        lifetime_session: 17,
        phase: Phase::Staging,
        idle_secs: 0,
        tokens_spent: 1_284_902,
        budget_total: Some(4_000_000),
        cost_spent: 8.17,
        cost_limit: Some(25.0),
        goals_met: 1,
        goals_total: 3,
        goals: Vec::new(), // the successor `judges` below drives every reader now.
        per_agent,
        judges: vec![
            JudgeView {
                name: "build_passes".into(),
                kind: "script".into(),
                in_dod: true,
                invariant: true,
                state: "met".into(),
                met: true,
                value: None,
                max: None,
                target: 1.0,
                delta: 0.0,
                rationale: "cargo build: 0 warnings, 0 errors".into(),
                error: None,
            },
            JudgeView {
                name: "coverage".into(),
                kind: "llm".into(),
                in_dod: true,
                invariant: false,
                state: "in_progress".into(),
                met: false,
                value: Some(64.0),
                max: Some(100.0),
                target: 80.0,
                delta: 12.0,
                rationale: "64% of modules covered (was 52% last step)".into(),
                error: None,
            },
            JudgeView {
                name: "no_todos".into(),
                kind: "script".into(),
                in_dod: true,
                invariant: false,
                state: "pending".into(),
                met: false,
                value: None,
                max: None,
                target: 1.0,
                delta: 0.0,
                rationale: "judge failed: judges/no_todos.sh: exit 127 (rg: command not found)".into(),
                error: Some("judges/no_todos.sh: exit 127 (rg: command not found)".into()),
            },
            JudgeView {
                name: "stalled".into(),
                kind: "script".into(),
                in_dod: false,
                invariant: false,
                state: "met".into(),
                met: true,
                value: None,
                max: None,
                target: 1.0,
                delta: 0.0,
                rationale: "no state-file change across 2 sessions — reconsider triggered".into(),
                error: None,
            },
        ],
        now: "🔧 reading judges/no_todos.sh to fix the broken judge".into(),
        think: "build is green but no_todos has errored for two sessions and coverage stalled at 64% — fix the judge before pushing more code".into(),
        recent: vec![
            ActivityEvent { ts: "14:31:02".into(), kind: "init".into(), text: "session #3 · step `reconsider` [claude] · Reconsider role".into() },
            ActivityEvent { ts: "14:31:04".into(), kind: "think".into(), text: "coverage stalled at 64%, no_todos judge erroring — diagnose before continuing".into() },
            ActivityEvent { ts: "14:31:09".into(), kind: "tool".into(), text: "$ cat judges/no_todos.sh".into() },
            ActivityEvent { ts: "14:31:16".into(), kind: "tool_result".into(), text: "rg not found — judge depends on a tool the runner lacks".into() },
            ActivityEvent { ts: "14:31:22".into(), kind: "think".into(), text: "rewrite the judge to use grep -rn instead of rg".into() },
        ],
        summary_cumulative: "17 lifetime sessions. Endpoints wired, build green since session 2. Coverage climbing (52%→64%). no_todos judge broken since session 2. Codex handled the implement steps; Claude handles reconsider + rules the judges.".into(),
        summary_windowed: "This run: brought build to 0 warnings, raised coverage to 64%, hit a stall — now in a reconsider step to repair the judge.".into(),
        memory_bytes: 14_208,
        // the sample drives `agg dashboard --demo` and the SVG doc shots — flagged, because the
        // whole point of the flag is that it must be impossible to miss.
        notify_session: Some(17),
        notify_reason: "STALLED — no judge moved across the last 3 merged steps".into(),
        seq: 342,
        finished: false,
        finish_reason: String::new(),
        asks: Vec::new(),
    }
}















// ---------------- styling + formatting helpers ----------------













#[cfg(test)]
mod tests;
