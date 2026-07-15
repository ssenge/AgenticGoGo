//! `agg dashboard` — the live TUI. A separate viewer process that polls
//! `agg/state/state.json` (written by `agg run` + its worker) and repaints in place
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
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::stdout;
use std::path::Path;
use std::time::Duration;

/// Which scrollable pane currently has the keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Judges,
    Activity,
}

/// Persistent UI state across repaints (scroll positions, focus, follow mode).
struct DashboardUi {
    focus: Focus,
    judges_scroll: u16,
    activity_scroll: u16,
    /// when true, the Activity pane sticks to the newest event (the default);
    /// scrolling up in Activity turns it off, `f` or scrolling to the bottom restores it.
    activity_follow: bool,
}

impl Default for DashboardUi {
    fn default() -> Self {
        DashboardUi {
            focus: Focus::Activity,
            judges_scroll: 0,
            activity_scroll: 0,
            activity_follow: true,
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
        term.draw(|f| draw(f, dir, state.as_ref(), &mut ui))?;

        // poll input ~4x/sec; repaint regardless to pick up state changes.
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if handle_key(&mut ui, k.code) == KeyAction::Quit {
                    break;
                }
            }
        }
        // if the run finished, keep showing the final frame but allow q to exit.
    }
    Ok(())
}

/// Whether a handled key asked the dashboard to quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    Continue,
    Quit,
}

/// Apply one keypress to the UI state. Pure over `(ui, code)` — no terminal I/O — so the whole
/// interaction model (focus switching, scrolling, follow-mode, quit) is unit-testable without a
/// real TTY. The event loop just reads keys and calls this.
fn handle_key(ui: &mut DashboardUi, code: KeyCode) -> KeyAction {
    match code {
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
    term.draw(|f| draw(f, Path::new("."), Some(state), &mut ui)).unwrap();
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
        stop_when: "all_goals".into(),
        halt_when: "session > 40 OR over_cost".into(),
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
        seq: 342,
        finished: false,
        finish_reason: String::new(),
    }
}

fn draw(f: &mut Frame, dir: &Path, state: Option<&DashboardState>, ui: &mut DashboardUi) {
    let area = f.area();
    let Some(s) = state else {
        let msg = Paragraph::new(format!(
            "waiting for `agg run`…\n\n(no {} yet — start the loop in another terminal)",
            DashboardState::path(dir).display()
        ))
        .block(title_block(" AgenticGoGo "))
        .wrap(Wrap { trim: true });
        f.render_widget(msg, area);
        return;
    };

    // Vertical split. A thin title row (name only) tops the screen; Info + the Progress/Per-agent
    // band are fixed-height; Judges and Activity flex; Summary is compact.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title row: "AgenticGoGo" only
            Constraint::Length(4), // Info  (border + 2 lines: run/step + spend/conditions)
            Constraint::Length(6), // band: Progress | Per-agent (border + 4 lines)
            Constraint::Min(8),    // Judges (scrollable scoreboard)
            Constraint::Min(6),    // Activity (real-time tail)
            Constraint::Length(5), // Summary (border + ~3 lines)
        ])
        .split(area);

    // the band splits horizontally: judge progress on the left, per-agent spend on the right (§7.4).
    let band = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(chunks[2]);

    draw_title(f, chunks[0]);
    draw_info(f, chunks[1], s);
    draw_progress(f, band[0], s);
    draw_per_agent(f, band[1], s);
    draw_judges(f, chunks[3], s, ui);
    draw_activity(f, chunks[4], s, ui);
    draw_summary(f, chunks[5], s);
}

fn title_block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title.bold())
}

/// The top title row — the application name ONLY (per the redesign: project/uptime/
/// stop-condition moved out of the title into the Info block). Rendered as a bold,
/// centered banner with no border to keep it light.
fn draw_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(Span::styled(
        "AgenticGoGo",
        Style::default().fg(Color::Cyan).bold(),
    )))
    .alignment(Alignment::Center);
    f.render_widget(title, area);
}

/// A bordered block whose title gains a cyan ▸ marker + reverse style when focused,
/// so the user can see which pane the arrow keys drive.
fn focusable_block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default().bold()
    };
    let marker = if focused { "▸" } else { " " };
    Block::default()
        .borders(Borders::ALL)
        .border_style(if focused { Style::default().fg(Color::Cyan) } else { Style::default() })
        .title(Span::styled(format!("{marker}{title}"), style))
}

/// Info block: the run's identity + spend. Line 1 is WHO/WHERE — project, session, phase, and the
/// current step with its agent + resolved model (§7.4: a mixed run is uninterpretable without it).
/// Line 2 is the ledger — tokens, usage, memory, idle, and the stop/abort conditions.
fn draw_info(f: &mut Frame, area: Rect, s: &DashboardState) {
    let project = if s.project.is_empty() { "—" } else { &s.project };
    let tokens = match s.budget_total {
        Some(t) => format!("{} / {}", human(s.tokens_spent), human(t)),
        None => human(s.tokens_spent),
    };
    // usage string ($ = the API-equivalent price Claude reports as `total_cost_usd`, NOT a
    // subscription charge — on a Max/Pro plan this is a usage proxy, not money billed). Shown on
    // the info line only when a cap is set or any spend exists (so a token-only run stays
    // uncluttered). Mirrors the `agg status` rule.
    let cost = match s.cost_limit {
        Some(t) => Some(format!("${:.2} / ${:.2} (API-eq)", s.cost_spent, t)),
        None if s.cost_spent > 0.0 => Some(format!("${:.2} (API-eq)", s.cost_spent)),
        None => None,
    };
    let halt = if s.halt_when.is_empty() { "—".to_string() } else { s.halt_when.clone() };
    let phase = Span::styled(s.phase.to_string(), Style::default().fg(phase_color(&s.phase)).bold());
    // the current step + its agent/model (§7.4). `step_model` falls back to the worker-default
    // `model` so an older state.json (no per-step model) still shows something real.
    let step = if s.step.is_empty() { "—" } else { s.step.as_str() };
    let step_agent = if s.step_agent.is_empty() { "—" } else { s.step_agent.as_str() };
    let step_model = if !s.step_model.is_empty() {
        s.step_model.as_str()
    } else if !s.model.is_empty() {
        s.model.as_str()
    } else {
        "agent default"
    };

    let line1 = Line::from(vec![
        label("project "),
        Span::styled(project.to_string(), Style::default().bold()),
        sep(),
        label("session "),
        // "#<this-run>" plus the cumulative lifetime total when it differs (i.e. the
        // project has run across more than one `agg run`), so a restart no longer
        // looks like the work started over: e.g. "#4 (of 23)".
        Span::styled(
            if s.lifetime_session > s.session {
                format!("#{} (of {})", s.session, s.lifetime_session)
            } else {
                format!("#{}", s.session)
            },
            Style::default().bold(),
        ),
        sep(),
        label("phase "),
        phase,
        sep(),
        label("step "),
        Span::styled(step.to_string(), Style::default().fg(Color::Cyan).bold()),
        Span::styled(format!(" [{step_agent} · {step_model}]"), Style::default().fg(Color::Blue)),
    ]);
    let idle_color = if s.idle_secs >= 240 { Color::Red } else { Color::DarkGray };
    // Ordered by importance so the LEAST critical items (memory/idle/started) are the ones a narrow
    // terminal truncates first — the spend guards and the run's done/abort conditions survive.
    // The mandatory head (up + tokens) always renders; the rest are droppable SEGMENTS, each led by
    // its own separator, so a segment that won't fit is omitted WHOLE rather than clipped mid-token
    // (ratatui would otherwise leave "memory 13" — a KB value cut to a bogus standalone number).
    let mut line2_spans = vec![
        label("up "),
        Span::raw(crate::util::fmt_dur(s.up_secs)),
        sep(),
        label("tokens "),
        Span::raw(tokens),
    ];
    let mut segments: Vec<Vec<Span>> = Vec::new();
    if let Some(cost) = cost {
        segments.push(vec![sep(), label("usage "), Span::styled(cost, Style::default().fg(Color::Magenta))]);
    }
    segments.push(vec![
        sep(),
        label("done_if "),
        Span::styled(s.stop_when.clone(), Style::default().fg(Color::Green)),
        sep(),
        label("abort_if "),
        Span::styled(halt, Style::default().fg(Color::Yellow)),
    ]);
    if s.memory_bytes > 0 {
        segments.push(vec![sep(), label("memory "), Span::styled(crate::util::human_bytes(s.memory_bytes), Style::default().fg(Color::Cyan))]);
    }
    segments.push(vec![sep(), label("idle "), Span::styled(format!("{}s", s.idle_secs), Style::default().fg(idle_color))]);
    segments.push(vec![sep(), label("started "), Span::raw(fmt_started(s.started_at_epoch))]);

    // Greedily append segments while they fit the inner width; stop at the first that doesn't (the
    // ordering is by importance, so we never skip a spend guard to squeeze in a lesser trailing item).
    let span_w = |spans: &[Span]| spans.iter().map(|sp| sp.content.chars().count()).sum::<usize>();
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut used = span_w(&line2_spans);
    for seg in segments {
        let w = span_w(&seg);
        if used + w > inner_w {
            break;
        }
        used += w;
        line2_spans.extend(seg);
    }
    let line2 = Line::from(line2_spans);

    let p = Paragraph::new(vec![line1, line2]).block(title_block(" Info "));
    f.render_widget(p, area);
}

/// Progress: the DoD-set aggregate (§5.3 — the count ranges over the DoD-set, NOT the run-set) as a
/// segmented bar, plus a one-line tally of judge lifecycle states so "1/3" says WHICH three.
fn draw_progress(f: &mut Frame, area: Rect, s: &DashboardState) {
    let frac = if s.goals_total == 0 { 0.0 } else { s.goals_met as f64 / s.goals_total as f64 };
    let color = grade_color(frac);

    let label = format!(" {}/{} · {:.0}%", s.goals_met, s.goals_total, frac * 100.0);
    let inner_w = area.width.saturating_sub(2) as usize;
    let track_w = inner_w.saturating_sub(label.chars().count() + 1).max(1);
    let filled = ((frac * track_w as f64).round() as usize).min(track_w);
    let bar = Line::from(vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(track_w - filled), Style::default().fg(Color::DarkGray)),
        Span::styled(label, Style::default().fg(color).bold()),
    ]);

    // lifecycle tally across the DoD-set — met / in-progress / pending / regressed / errored.
    let dod: Vec<JudgeView> = s.judge_views().into_iter().filter(|j| j.in_dod).collect();
    let n = |pred: fn(&JudgeView) -> bool| dod.iter().filter(|&j| pred(j)).count();
    let errored = n(|j| j.error.is_some());
    let tally = Line::from(vec![
        Span::styled(format!("✔{} ", n(|j| j.met)), Style::default().fg(Color::Green)),
        Span::styled(format!("◑{} ", n(|j| j.state == "in_progress")), Style::default().fg(Color::Yellow)),
        Span::styled(format!("·{} ", n(|j| j.state == "pending" && j.error.is_none())), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("⚠{} ", n(|j| j.state == "regressed")), Style::default().fg(Color::Red)),
        Span::styled(format!("⊘{errored}"), Style::default().fg(if errored > 0 { Color::Red } else { Color::DarkGray })),
    ]);
    let p = Paragraph::new(vec![bar, Line::default(), tally]).block(title_block(" Progress "));
    f.render_widget(p, area);
}

/// Per-agent (§7.4): a token + cost row per agent, then a total. A mixed claude/codex run's
/// aggregate is meaningless without this. An agent that cannot report a price shows "—", never
/// "$0.00" (that would lie). Empty ⇒ a single-agent run; fall back to a one-line aggregate.
fn draw_per_agent(f: &mut Frame, area: Rect, s: &DashboardState) {
    let mut lines: Vec<Line> = Vec::new();
    if s.per_agent.is_empty() {
        lines.push(Line::from(vec![
            label("all "),
            Span::raw(human(s.tokens_spent)),
            Span::raw(" tok  "),
            Span::styled(money(agg_cost(s)), Style::default().fg(Color::Magenta)),
        ]));
        lines.push(Line::from(Span::styled(
            "  (single agent — no per-agent split)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (agent, u) in &s.per_agent {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<9}", fit(agent, 9)), Style::default().fg(agent_color(agent)).bold()),
                Span::styled(format!("{:>8} tok  ", human(u.tokens)), Style::default().fg(Color::Cyan)),
                Span::styled(money(u.cost), Style::default().fg(Color::Magenta)),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", "total"), Style::default().bold()),
            Span::styled(format!("{:>8} tok  ", human(s.tokens_spent)), Style::default().fg(Color::Cyan).bold()),
            Span::styled(money(agg_cost(s)), Style::default().fg(Color::Magenta).bold()),
        ]));
    }
    let p = Paragraph::new(lines).block(title_block(" Per-agent "));
    f.render_widget(p, area);
}

/// Judges: the §7.4 per-judge scoreboard (successor to the goal list). Each judge renders a detail
/// line — glyph, name, measure (met/unmet for binary, value/target + bar for numeric, `error` for a
/// broken judge), ▲delta, guard/run-set flags, kind — plus its wrapped rationale. DoD-set first,
/// then a divider and any run-set control judges (e.g. `stalled`), so "why we stalled" is visible.
fn draw_judges(f: &mut Frame, area: Rect, s: &DashboardState, ui: &mut DashboardUi) {
    let focused = ui.focus == Focus::Judges;
    let inner_w = area.width.saturating_sub(2) as usize;
    let judges = s.judge_views();
    let (dod, run): (Vec<JudgeView>, Vec<JudgeView>) = judges.into_iter().partition(|j| j.in_dod);

    let mut lines: Vec<Line> = Vec::new();
    let push_judge = |lines: &mut Vec<Line>, j: &JudgeView| {
        lines.push(judge_detail_line(j));
        if !j.rationale.is_empty() {
            for wrapped in wrap_indent(&j.rationale, "    ↳ ", "      ", inner_w) {
                let c = if j.error.is_some() { Color::Red } else { Color::DarkGray };
                lines.push(Line::from(Span::styled(wrapped, Style::default().fg(c))));
            }
        }
    };
    for j in &dod {
        push_judge(&mut lines, j);
    }
    if !run.is_empty() {
        lines.push(Line::from(Span::styled(
            "── run-set (not counted toward done) ──",
            Style::default().fg(Color::DarkGray),
        )));
        for j in &run {
            push_judge(&mut lines, j);
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  (no judges in snapshot)", Style::default().fg(Color::DarkGray))));
    }

    let total = lines.len() as u16;
    let view_h = area.height.saturating_sub(2); // minus borders
    let max_scroll = total.saturating_sub(view_h);
    ui.judges_scroll = ui.judges_scroll.min(max_scroll);

    let title = if max_scroll > 0 {
        format!(" Judges  [{}/{}]  ↑↓ ", (ui.judges_scroll + view_h).min(total), total)
    } else {
        " Judges ".to_string()
    };
    let p = Paragraph::new(lines)
        .block(focusable_block(&title, focused))
        .scroll((ui.judges_scroll, 0));
    f.render_widget(p, area);
}

/// One judge's detail line: glyph · name · measure · ▲delta · flags · kind.
fn judge_detail_line(j: &JudgeView) -> Line<'static> {
    let (glyph, color) = judge_glyph(j);
    let delta = if j.value.is_some() && j.delta.abs() > f64::EPSILON {
        format!("  ▲{:+.0}", j.delta)
    } else {
        String::new()
    };
    let flag = if j.invariant { "  [guard]" } else { "" };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(format!("{:<18}", fit(&j.name, 18)), Style::default().fg(color).bold()),
    ];
    spans.extend(judge_measure_spans(j, color));
    // `judge:kind` starts at a CONSTANT column for every row: measure is a fixed 22 cols (below),
    // delta a fixed 7, and the variable-width `[guard]` flag trails the kind rather than shoving it
    // right — so the kind column no longer ratchets across guarded/numeric/binary rows.
    spans.extend([
        Span::styled(format!("{:<7}", delta), Style::default().fg(Color::Green)),
        Span::styled(format!("  judge:{}", j.kind), Style::default().fg(Color::Blue)),
        Span::styled(flag.to_string(), Style::default().fg(Color::Yellow)),
    ]);
    Line::from(spans)
}

/// The measure column as colored spans: binary ⇒ met/unmet (never `0`, §7.4); numeric ⇒
/// `value/target` + a proportional bar; broken ⇒ `error`.
fn judge_measure_spans(j: &JudgeView, color: Color) -> Vec<Span<'static>> {
    if j.error.is_some() {
        return vec![Span::styled(format!("{:<22}", "error"), Style::default().fg(Color::Red).bold())];
    }
    match j.value {
        None => {
            let word = if j.met { "met" } else { "unmet" };
            vec![Span::styled(format!("{word:<22}"), Style::default().fg(color))]
        }
        Some(v) => {
            let frac = if j.target > 0.0 {
                (v / j.target).clamp(0.0, 1.0)
            } else if j.met {
                1.0
            } else {
                0.0
            };
            let bar_w = 8usize;
            let filled = ((frac * bar_w as f64).round() as usize).min(bar_w);
            let num = format!("{:.0}/{:.0} ", v, j.target);
            // 12 (num) + 8 (bar) + 2 (trailing) = 22, matching the binary/error branches' `{:<22}`
            // so the column after the measure lands identically on every judge row.
            vec![
                Span::styled(format!("{num:<12}"), Style::default().fg(color)),
                Span::styled("█".repeat(filled), Style::default().fg(color)),
                Span::styled(format!("{}  ", "░".repeat(bar_w - filled)), Style::default().fg(Color::DarkGray)),
            ]
        }
    }
}

/// State glyph + color for a judge. A broken judge (`error`) gets its own ⊘, distinct from a clean
/// "not met" — a reader must never confuse "I could not grade this" with "this is not met" (§5.2).
fn judge_glyph(j: &JudgeView) -> (&'static str, Color) {
    if j.error.is_some() {
        return ("⊘", Color::Red);
    }
    match j.state.as_str() {
        "met" => ("✔", Color::Green),
        "regressed" => ("⚠", Color::Red),
        "in_progress" => ("◑", Color::Yellow),
        _ => ("·", Color::DarkGray),
    }
}

/// Activity: the REAL-TIME event tail. Renders `recent` events newest-at-bottom,
/// auto-following the latest unless the user scrolled up. Each line is colored by
/// its event kind (tool/think/result/…).
fn draw_activity(f: &mut Frame, area: Rect, s: &DashboardState, ui: &mut DashboardUi) {
    let focused = ui.focus == Focus::Activity;
    let inner_w = area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for ev in &s.recent {
        lines.push(activity_line(ev, inner_w));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (waiting for the worker's first event…)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let total = lines.len() as u16;
    let view_h = area.height.saturating_sub(2);
    let max_scroll = total.saturating_sub(view_h);

    // auto-follow: pin to the bottom (show the newest events) unless the user scrolled.
    if ui.activity_follow {
        ui.activity_scroll = max_scroll;
    } else {
        ui.activity_scroll = ui.activity_scroll.min(max_scroll);
        // scrolled back down to the bottom → resume following.
        if ui.activity_scroll >= max_scroll {
            ui.activity_follow = true;
        }
    }

    let follow_tag = if ui.activity_follow { "⏵live" } else { "paused" };
    let title = format!(" Activity  [{follow_tag}]  {} events ", s.recent.len());
    let p = Paragraph::new(lines)
        .block(focusable_block(&title, focused))
        .scroll((ui.activity_scroll, 0));
    f.render_widget(p, area);
}

/// One activity tail line: `HH:MM:SS <glyph> <text>`, colored by kind.
fn activity_line(ev: &ActivityEvent, width: usize) -> Line<'static> {
    let (glyph, color) = activity_glyph(&ev.kind);
    let prefix_len = 9 + 2; // "HH:MM:SS " + "<glyph> "
    let text = fit(&ev.text, width.saturating_sub(prefix_len));
    Line::from(vec![
        Span::styled(format!("{} ", ev.ts), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(text, Style::default().fg(color)),
    ])
}

/// Summary: ONE block. The cumulative "story" (wrapped) + a recent line, or the
/// terminal FINISHED banner when the run is done.
fn draw_summary(f: &mut Frame, area: Rect, s: &DashboardState) {
    let body = if s.finished {
        vec![Line::from(Span::styled(
            format!("✔ FINISHED: {}", s.finish_reason),
            Style::default().fg(Color::Green).bold(),
        ))]
    } else {
        // The panel is a fixed 3 content lines. Cap the cumulative story at 2 wrapped lines so the
        // windowed `recent:` line ALWAYS survives (it used to be shoved out entirely when the story
        // was long); the story ellipsizes on clip instead of vanishing mid-sentence (§7.4 finding).
        let inner_w = area.width.saturating_sub(2) as usize;
        let story = if s.summary_cumulative.is_empty() { "(no summary yet)" } else { &s.summary_cumulative };
        let mut v = wrapped_block("story:  ", Color::Green, story, 2, inner_w);
        if !s.summary_windowed.is_empty() {
            v.extend(wrapped_block("recent: ", Color::Blue, &s.summary_windowed, 1, inner_w));
        }
        v
    };
    // No `.wrap()`: `wrapped_block` has already wrapped every line to the inner width and clamped the
    // line count, and Wrap{trim} would eat the continuation indent. Pre-wrapping is what lets us
    // reserve a line for `recent:` instead of letting the story consume the whole panel.
    let p = Paragraph::new(body).block(title_block(" Summary   (Tab=focus · ↑↓=scroll · f=follow · q=quit) "));
    f.render_widget(p, area);
}

// ---------------- styling + formatting helpers ----------------

fn label(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().fg(Color::DarkGray))
}
fn sep() -> Span<'static> {
    Span::styled("  ", Style::default())
}

/// Green at 100%, through yellow, to red near 0% — the "approaching red" spec.
fn grade_color(frac: f64) -> Color {
    if frac >= 0.8 {
        Color::Green
    } else if frac >= 0.5 {
        Color::Yellow
    } else if frac >= 0.25 {
        Color::LightRed
    } else {
        Color::Red
    }
}

/// A per-agent cost cell: a real price, or "—" for an agent that cannot report one (never "$0.00").
fn money(c: Option<f64>) -> String {
    match c {
        Some(c) => format!("${c:.2}"),
        None => "—".to_string(),
    }
}

/// The total cost to show for the per-agent panel: the aggregate spend, or `None` (→ "—") when NO
/// agent could report a price — so a fully non-reporting run never prints a lying "$0.00".
fn agg_cost(s: &DashboardState) -> Option<f64> {
    let reported = if s.per_agent.is_empty() {
        s.cost_spent > 0.0
    } else {
        s.per_agent.values().any(|u| u.cost.is_some())
    };
    reported.then_some(s.cost_spent)
}

/// A stable accent per agent so claude/codex/copilot read apart at a glance.
fn agent_color(agent: &str) -> Color {
    match agent {
        "claude" => Color::Magenta,
        "codex" => Color::Green,
        "copilot" => Color::Blue,
        _ => Color::Cyan,
    }
}

fn activity_glyph(kind: &str) -> (&'static str, Color) {
    match kind {
        "tool" => ("🔧", Color::Cyan),
        "think" => ("💬", Color::Magenta),
        "tool_result" => ("↳", Color::DarkGray),
        "result" => ("✅", Color::Green),
        "init" => ("▶", Color::Blue),
        _ => ("·", Color::Gray),
    }
}

/// The four deterministic outer-loop stages, plus the off-cycle ones. Exhaustive over [`Phase`]:
/// adding a stage is now a compile error here until it gets a color, instead of silently
/// falling into a `_ => Gray` arm.
fn phase_color(phase: &Phase) -> Color {
    match phase {
        Phase::Inject => Color::Blue,
        Phase::Run => Color::Green,
        Phase::Verify => Color::Cyan,
        Phase::Gate => Color::Magenta,
        Phase::Backoff => Color::Yellow,
        Phase::Staging => Color::Yellow,
        Phase::Done => Color::Green,
        // `Starting` is pre-loop; `Other` is a stage from a different agg build (see Phase).
        Phase::Starting | Phase::Other(_) => Color::Gray,
    }
}

/// Absolute local wall-clock for the loop's start, as `HH:MM:SS <zone>`. The local offset comes
/// from `localtime`, so a user in CEST sees their wall clock, not UTC. The zone label
/// (e.g. `UTC+2`) names what's being shown.
fn fmt_started(epoch: u64) -> String {
    if epoch == 0 {
        return "—".to_string();
    }
    let (h, m, s) = crate::ui::localtime::local_hms(epoch);
    format!("{h:02}:{m:02}:{s:02} {}", crate::ui::localtime::offset_label())
}

fn human(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// Fit `s` into a fixed column `n`, where the `…` counts toward the width (unlike
/// `util::truncate`, whose ellipsis overflows by one). The TUI needs exact-width fitting so
/// a cell never bleeds past its column; the off-by-one is deliberate, hence a separate fn.
fn fit(s: &str, n: usize) -> String {
    if n == 0 || s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Word-wrap `text` to `width`, prefixing the first line with `first` and every
/// continuation line with `cont` (so a rationale reads as an indented block).
fn wrap_indent(text: &str, first: &str, cont: &str, width: usize) -> Vec<String> {
    let avail = width.saturating_sub(first.chars().count()).max(8);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let prospective = if line.is_empty() { word.chars().count() } else { line.chars().count() + 1 + word.chars().count() };
        if prospective > avail && !line.is_empty() {
            out.push(line.clone());
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out.into_iter()
        .enumerate()
        .map(|(i, l)| if i == 0 { format!("{first}{l}") } else { format!("{cont}{l}") })
        .collect()
}

/// Render `text` under a colored `prefix`, wrapped to `inner_w` and HARD-CAPPED at `max` lines — so a
/// fixed-height panel can't let a long block crowd out whatever renders below it. The last kept line
/// gets an ellipsis when the text was clipped, instead of ending mid-sentence with no signal.
fn wrapped_block(prefix: &'static str, color: Color, text: &str, max: usize, inner_w: usize) -> Vec<Line<'static>> {
    let pw = prefix.chars().count();
    let body_w = inner_w.saturating_sub(pw).max(8);
    let indent = " ".repeat(pw);
    let mut lines = wrap_indent(text, "", "", body_w);
    let clipped = lines.len() > max;
    lines.truncate(max.max(1));
    if clipped {
        if let Some(last) = lines.last_mut() {
            let t: String = last.chars().take(body_w.saturating_sub(1)).collect();
            *last = format!("{t}…");
        }
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                Line::from(vec![Span::styled(prefix, Style::default().fg(color).bold()), Span::raw(l)])
            } else {
                Line::from(Span::raw(format!("{indent}{l}")))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ActivityEvent, AgentUsage, DashboardState, JudgeView};
    use ratatui::backend::TestBackend;

    fn demo_state() -> DashboardState {
        let mut per_agent = std::collections::BTreeMap::new();
        per_agent.insert("claude".to_string(), AgentUsage { tokens: 1_500_000, cost: Some(1.10) });
        per_agent.insert("codex".to_string(), AgentUsage { tokens: 600_000, cost: Some(0.15) });
        DashboardState {
            project: "myproject".into(),
            model: "claude-opus-4-8".into(),
            step: "worker".into(),
            step_agent: "codex".into(),
            step_model: "gpt-5-codex".into(),
            stop_when: "all_goals".into(),
            halt_when: "regressed".into(),
            started_at_epoch: 1_700_000_000,
            up_secs: 11520,
            session: 7,
            lifetime_session: 7,
            phase: Phase::Run,
            idle_secs: 12,
            tokens_spent: 2_100_000,
            budget_total: Some(5_000_000),
            cost_spent: 1.25,
            cost_limit: Some(5.0),
            memory_bytes: 2048,
            goals_met: 1,
            goals_total: 2,
            goals: Vec::new(), // the successor `judges` below drives every reader now.
            per_agent,
            judges: vec![
                JudgeView {
                    name: "tests_pass".into(),
                    kind: "script".into(),
                    in_dod: true,
                    invariant: false,
                    state: "in_progress".into(),
                    met: false,
                    value: Some(18.0),
                    max: Some(28.0),
                    target: 28.0,
                    delta: 3.0,
                    rationale: "18/28 tests passing; the remaining ten exercise the BOUND-limited \
                                instances that need the conflict-analysis bound-climb lever to land."
                        .into(),
                    error: None,
                },
                JudgeView {
                    name: "no_regressions".into(),
                    kind: "script".into(),
                    in_dod: true,
                    invariant: true,
                    state: "met".into(),
                    met: true,
                    value: None,
                    max: None,
                    target: 1.0,
                    delta: 0.0,
                    rationale: "build green".into(),
                    error: None,
                },
                JudgeView {
                    name: "stalled".into(),
                    kind: "script".into(),
                    in_dod: false,
                    invariant: false,
                    state: "pending".into(),
                    met: false,
                    value: None,
                    max: None,
                    target: 1.0,
                    delta: 0.0,
                    rationale: "state file changed this session — not stalled".into(),
                    error: None,
                },
            ],
            now: "🔧 $ Run the test suite".into(),
            think: "implementing the remaining parser cases".into(),
            recent: vec![
                ActivityEvent { ts: "14:07:01".into(), kind: "tool".into(), text: "$ Run the test suite".into() },
                ActivityEvent { ts: "14:07:03".into(), kind: "tool_result".into(), text: "18 passed, 10 failed".into() },
                ActivityEvent { ts: "14:07:05".into(), kind: "think".into(), text: "the bound improved on aflow30a".into() },
            ],
            summary_cumulative: "Building the parser; 18/28 tests pass, no regressions.".into(),
            summary_windowed: "Fixed an edge case; the suite is greener.".into(),
            seq: 5,
            finished: false,
            finish_reason: String::new(),
        }
    }

    fn render(s: &DashboardState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        let mut ui = DashboardUi::default();
        term.draw(|f| draw(f, Path::new("."), Some(s), &mut ui)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    /// Render the full dashboard headless and assert the key content appears across
    /// every panel — proves draw() doesn't panic and lays out the right text.
    #[test]
    fn renders_all_panels() {
        let s = demo_state();
        let text = render(&s, 100, 40);
        assert!(text.contains("AgenticGoGo")); // title (waiting screen reuses it too)
        assert!(text.contains("Info"));
        assert!(text.contains("myproject"));
        assert!(text.contains("#7"));            // session number
        // the current step + its agent/model live in Info (§7.4).
        assert!(text.contains("worker"));        // step name
        assert!(text.contains("gpt-5-codex"));   // step model (a mixed-run codex step)
        assert!(text.contains("Progress"));
        assert!(text.contains("1/2"));           // DoD aggregate label
        assert!(text.contains("Per-agent"));     // §7.4 per-agent panel
        assert!(text.contains("claude"));        // a per-agent row
        assert!(text.contains("codex"));         // the other agent / the step agent
        assert!(text.contains("Judges"));        // scoreboard title
        assert!(text.contains("tests_pass"));
        assert!(text.contains("18/28"));         // numeric judge shows value/target
        assert!(text.contains("Activity"));
        assert!(text.contains("14:07:05"));      // activity tail timestamp
        assert!(text.contains("bound improved")); // activity tail text
        assert!(text.contains("Summary"));
        assert!(text.contains("Building the parser")); // cumulative summary
    }

    /// The §7.4 defect this migration fixes, at the TUI: a binary judge renders met/unmet, NOT a
    /// fabricated `0`; a run-set judge is shown under its own divider.
    #[test]
    fn binary_judge_reads_met_and_run_set_is_divided() {
        let s = demo_state();
        let text = render(&s, 100, 44);
        assert!(text.contains("no_regressions"));
        assert!(text.contains("met")); // the binary invariant reads "met", never "0/0"
        assert!(!text.contains("0/0"), "a binary judge must never render a fabricated 0/0");
        assert!(text.contains("run-set")); // the divider before `stalled`
        assert!(text.contains("stalled"));
    }

    /// The title row must carry the name ONLY — project/model/stop must NOT bleed into
    /// it. Row 0 is the centered "AgenticGoGo" banner; row 1 is the Info block border.
    #[test]
    fn title_is_name_only() {
        let backend = TestBackend::new(100, 40);
        let mut term = Terminal::new(backend).unwrap();
        let s = demo_state();
        let mut ui = DashboardUi::default();
        term.draw(|f| draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
        let buf = term.backend().buffer().clone();
        let row0: String = (0..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.contains("AgenticGoGo"));
        assert!(!row0.contains("myproject")); // details live in Info, not the title
        assert!(!row0.contains("all_goals"));
        // row 1 is the Info block's top border.
        let row1: String = (0..100).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row1.contains("Info"));
    }

    #[test]
    fn missing_state_shows_waiting() {
        let backend = TestBackend::new(80, 20);
        let mut term = Terminal::new(backend).unwrap();
        let mut ui = DashboardUi::default();
        term.draw(|f| draw(f, Path::new("/nonexistent"), None, &mut ui)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("waiting for"));
    }

    #[test]
    fn finished_banner_replaces_summary() {
        let mut s = demo_state();
        s.finished = true;
        s.finish_reason = "28/28 goals met after 42 session(s)".into();
        let text = render(&s, 100, 40);
        assert!(text.contains("FINISHED"));
        assert!(text.contains("28/28 goals met"));
    }

    #[test]
    fn color_grading() {
        assert_eq!(grade_color(1.0), Color::Green);
        assert_eq!(grade_color(0.6), Color::Yellow);
        assert_eq!(grade_color(0.3), Color::LightRed);
        assert_eq!(grade_color(0.1), Color::Red);
    }

    #[test]
    fn human_compacts_large_counts() {
        assert_eq!(human(2_100_000), "2.1M");
        assert_eq!(human(5_000), "5.0k");
        assert_eq!(human(42), "42");
        // fmt_dur/human_bytes now live in util.rs and are tested there.
    }

    #[test]
    fn wrap_indent_wraps_and_prefixes() {
        let out = wrap_indent("one two three four five", "↳ ", "  ", 12);
        assert!(out.len() >= 2); // wrapped onto multiple lines
        assert!(out[0].starts_with("↳ "));
        assert!(out[1].starts_with("  "));
    }

    #[test]
    fn activity_follow_pins_to_bottom() {
        // with many events, follow-mode should scroll to show the newest (max_scroll).
        let mut s = demo_state();
        s.recent = (0..40)
            .map(|i| ActivityEvent { ts: "00:00:00".into(), kind: "tool".into(), text: format!("event {i}") })
            .collect();
        let backend = TestBackend::new(100, 40);
        let mut term = Terminal::new(backend).unwrap();
        let mut ui = DashboardUi::default(); // follow = true
        term.draw(|f| draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        // the newest event must be visible; an early one scrolled off.
        assert!(text.contains("event 39"));
    }

    // ── interaction model: handle_key is pure, so every keypress is unit-testable ──────────────

    #[test]
    fn q_and_esc_quit() {
        let mut ui = DashboardUi::default();
        assert_eq!(handle_key(&mut ui, KeyCode::Char('q')), KeyAction::Quit);
        assert_eq!(handle_key(&mut ui, KeyCode::Esc), KeyAction::Quit);
        // a non-quit key continues.
        assert_eq!(handle_key(&mut ui, KeyCode::Char('x')), KeyAction::Continue);
    }

    #[test]
    fn tab_toggles_focus_between_judges_and_activity() {
        let mut ui = DashboardUi::default(); // starts on Activity
        assert_eq!(ui.focus, Focus::Activity);
        handle_key(&mut ui, KeyCode::Tab);
        assert_eq!(ui.focus, Focus::Judges);
        handle_key(&mut ui, KeyCode::Tab);
        assert_eq!(ui.focus, Focus::Activity);
    }

    #[test]
    fn f_toggles_activity_follow() {
        let mut ui = DashboardUi::default(); // follow = true
        handle_key(&mut ui, KeyCode::Char('f'));
        assert!(!ui.activity_follow);
        handle_key(&mut ui, KeyCode::Char('f'));
        assert!(ui.activity_follow);
    }

    /// `handle_key` alone can't catch this: the pause has to survive the NEXT repaint. It did
    /// not — `draw_activity` re-pins anything sitting at max_scroll, so pressing `f` in the
    /// default (bottom-pinned) state toggled follow off and straight back on, and the live TUI
    /// never showed `[paused]`.
    #[test]
    fn f_pause_survives_the_next_repaint() {
        let mut s = demo_state();
        s.recent = (0..50)
            .map(|i| ActivityEvent { ts: "00:00:00".into(), kind: "think".into(), text: format!("event {i}") })
            .collect();
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        let mut ui = DashboardUi::default(); // Activity focused, follow = true

        term.draw(|f| draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
        assert!(ui.activity_follow, "starts pinned to the newest event");

        handle_key(&mut ui, KeyCode::Char('f'));
        term.draw(|f| draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
        assert!(!ui.activity_follow, "an explicit `f` pause must survive the repaint");

        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("paused"), "the Activity title must read [paused]");

        // and `f` again re-pins to the newest event
        handle_key(&mut ui, KeyCode::Char('f'));
        term.draw(|f| draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
        assert!(ui.activity_follow);
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("live"), "the Activity title must read [⏵live] again");
    }

    #[test]
    fn scrolling_activity_up_drops_follow_then_end_restores_it() {
        let mut ui = DashboardUi::default(); // Activity focus, follow = true
        handle_key(&mut ui, KeyCode::Up); // look back → leave follow
        assert!(!ui.activity_follow, "scrolling up must leave follow-mode");
        assert_eq!(ui.activity_scroll, 0, "already at top, clamped at 0");
        // scroll down a few, then G/End re-pins to the newest.
        handle_key(&mut ui, KeyCode::PageDown);
        assert_eq!(ui.activity_scroll, 10);
        handle_key(&mut ui, KeyCode::Char('G'));
        assert!(ui.activity_follow, "End/G must re-enable follow on the Activity pane");
    }

    #[test]
    fn judges_pane_scrolls_and_clamps_at_top() {
        let mut ui = DashboardUi::default();
        handle_key(&mut ui, KeyCode::Tab); // focus Judges
        handle_key(&mut ui, KeyCode::Down);
        handle_key(&mut ui, KeyCode::Down);
        assert_eq!(ui.judges_scroll, 2);
        handle_key(&mut ui, KeyCode::PageUp); // -10, clamped at 0
        assert_eq!(ui.judges_scroll, 0, "scroll must never go negative");
        handle_key(&mut ui, KeyCode::Char('G')); // jump to bottom (clamped at draw time)
        assert_eq!(ui.judges_scroll, u16::MAX);
        handle_key(&mut ui, KeyCode::Char('g')); // back to top
        assert_eq!(ui.judges_scroll, 0);
    }

    #[test]
    fn scroll_targets_only_the_focused_pane() {
        let mut ui = DashboardUi::default(); // Activity focus
        handle_key(&mut ui, KeyCode::Down);
        assert_eq!(ui.activity_scroll, 1);
        assert_eq!(ui.judges_scroll, 0, "judges pane untouched while Activity is focused");
    }
}
