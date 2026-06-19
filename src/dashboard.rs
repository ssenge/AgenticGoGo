//! `agg dashboard` — the live TUI. A separate viewer process that polls
//! `.agg/state.json` (written by `agg run` + its worker) and repaints in place
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

use crate::state::{ActivityEvent, DashboardState, GoalView};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::{execute, terminal};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::stdout;
use std::path::Path;
use std::time::Duration;

/// Which scrollable pane currently has the keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Goals,
    Activity,
}

/// Persistent UI state across repaints (scroll positions, focus, follow mode).
struct DashboardUi {
    focus: Focus,
    goals_scroll: u16,
    activity_scroll: u16,
    /// when true, the Activity pane sticks to the newest event (the default);
    /// scrolling up in Activity turns it off, `f` or scrolling to the bottom restores it.
    activity_follow: bool,
}

impl Default for DashboardUi {
    fn default() -> Self {
        DashboardUi {
            focus: Focus::Activity,
            goals_scroll: 0,
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
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Tab => {
                        ui.focus = match ui.focus {
                            Focus::Goals => Focus::Activity,
                            Focus::Activity => Focus::Goals,
                        };
                    }
                    KeyCode::Char('f') => {
                        ui.activity_follow = !ui.activity_follow;
                    }
                    KeyCode::Up => scroll(&mut ui, -1),
                    KeyCode::Down => scroll(&mut ui, 1),
                    KeyCode::PageUp => scroll(&mut ui, -10),
                    KeyCode::PageDown => scroll(&mut ui, 10),
                    KeyCode::Char('g') | KeyCode::Home => match ui.focus {
                        Focus::Goals => ui.goals_scroll = 0,
                        Focus::Activity => {
                            ui.activity_scroll = 0;
                            ui.activity_follow = false;
                        }
                    },
                    KeyCode::Char('G') | KeyCode::End => match ui.focus {
                        Focus::Goals => ui.goals_scroll = u16::MAX, // clamped at draw time
                        Focus::Activity => ui.activity_follow = true,
                    },
                    _ => {}
                }
            }
        }
        // if the run finished, keep showing the final frame but allow q to exit.
    }
    Ok(())
}

/// Apply a relative scroll to the focused pane. Scrolling the Activity pane up
/// drops out of follow-mode; scrolling back to the top of follow re-enables it.
fn scroll(ui: &mut DashboardUi, delta: i32) {
    match ui.focus {
        Focus::Goals => {
            ui.goals_scroll = (ui.goals_scroll as i32 + delta).max(0) as u16;
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

    // Vertical split. A thin title row (name only) tops the screen; then Info +
    // Progress are fixed-height; Goals and Activity flex; Summary is compact.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title row: "AgenticGoGo" only
            Constraint::Length(4), // Info  (border + 2 lines)
            Constraint::Length(3), // Progress (border + 1 line)
            Constraint::Min(7),    // Goals  (scrollable)
            Constraint::Min(6),    // Activity (real-time tail)
            Constraint::Length(5), // Summary (border + ~3 lines)
        ])
        .split(area);

    draw_title(f, chunks[0]);
    draw_info(f, chunks[1], s);
    draw_progress(f, chunks[2], s);
    draw_goals(f, chunks[3], s, ui);
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

/// Info block: everything that used to clutter the title bar — project, when it
/// started (absolute), uptime, model, stop/halt conditions, tokens/budget, session.
fn draw_info(f: &mut Frame, area: Rect, s: &DashboardState) {
    let project = if s.project.is_empty() { "—" } else { &s.project };
    let model = if s.model.is_empty() { "—" } else { &s.model };
    let tokens = match s.budget_total {
        Some(t) => format!("{} / {}", human(s.tokens_spent), human(t)),
        None => human(s.tokens_spent),
    };
    // cost string, shown on the info line only when a cap is set or any spend exists
    // (so a token-only run keeps the line uncluttered). Mirrors the `agg status` rule.
    let cost = match s.cost_limit {
        Some(t) => Some(format!("${:.2} / ${:.2}", s.cost_spent, t)),
        None if s.cost_spent > 0.0 => Some(format!("${:.2}", s.cost_spent)),
        None => None,
    };
    let halt = if s.halt_when.is_empty() { "—".to_string() } else { s.halt_when.clone() };
    let phase = Span::styled(s.phase.clone(), Style::default().fg(phase_color(&s.phase)).bold());

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
        label("up "),
        Span::raw(fmt_dur(s.up_secs)),
        sep(),
        label("started "),
        Span::raw(fmt_started(s.started_at_epoch)),
    ]);
    let idle_color = if s.idle_secs >= 240 { Color::Red } else { Color::DarkGray };
    let mut line2_spans = vec![
        label("model "),
        Span::raw(model.to_string()),
        sep(),
        label("tokens "),
        Span::raw(tokens),
    ];
    if let Some(cost) = cost {
        line2_spans.push(sep());
        line2_spans.push(label("cost "));
        line2_spans.push(Span::styled(cost, Style::default().fg(Color::Magenta)));
    }
    line2_spans.extend(vec![
        sep(),
        label("idle "),
        Span::styled(format!("{}s", s.idle_secs), Style::default().fg(idle_color)),
        sep(),
        label("stop "),
        Span::styled(s.stop_when.clone(), Style::default().fg(Color::Green)),
        sep(),
        label("halt "),
        Span::styled(halt, Style::default().fg(Color::Yellow)),
    ]);
    let line2 = Line::from(line2_spans);

    let p = Paragraph::new(vec![line1, line2]).block(title_block(" Info "));
    f.render_widget(p, area);
}

/// Progress: a custom segmented blocks bar (replaces the crude half-filled Gauge).
/// Width-proportional `█` fill on a `░` track, color-graded green→red by fraction,
/// with an `N/M  P%` label. Reads cleanly at any terminal width.
fn draw_progress(f: &mut Frame, area: Rect, s: &DashboardState) {
    let frac = if s.goals_total == 0 { 0.0 } else { s.goals_met as f64 / s.goals_total as f64 };
    let color = grade_color(frac);

    // inner width minus borders(2) minus the label we append.
    let label = format!(" {}/{} goals · {:.0}%", s.goals_met, s.goals_total, frac * 100.0);
    let inner_w = area.width.saturating_sub(2) as usize;
    let track_w = inner_w.saturating_sub(label.chars().count() + 1).max(1);
    let filled = ((frac * track_w as f64).round() as usize).min(track_w);
    let bar_filled = "█".repeat(filled);
    let bar_empty = "░".repeat(track_w - filled);

    let line = Line::from(vec![
        Span::styled(bar_filled, Style::default().fg(color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled(label, Style::default().fg(color).bold()),
    ]);
    let p = Paragraph::new(line).block(title_block(" Progress "));
    f.render_widget(p, area);
}

/// Goals: scrollable list. Each goal renders a detail line (glyph, id, type,
/// measure, ▲delta, weight, guard flag, judge) plus its full rationale, wrapped
/// (not truncated) and indented. A "(n more ↓)" hint shows when content overflows.
fn draw_goals(f: &mut Frame, area: Rect, s: &DashboardState, ui: &mut DashboardUi) {
    let focused = ui.focus == Focus::Goals;
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for g in &s.goals {
        lines.push(goal_detail_line(g));
        if !g.rationale.is_empty() {
            for wrapped in wrap_indent(&g.rationale, "    ↳ ", "      ", inner_w) {
                lines.push(Line::from(Span::styled(wrapped, Style::default().fg(Color::DarkGray))));
            }
        }
    }

    let total = lines.len() as u16;
    let view_h = area.height.saturating_sub(2); // minus borders
    let max_scroll = total.saturating_sub(view_h);
    ui.goals_scroll = ui.goals_scroll.min(max_scroll);

    let title = if max_scroll > 0 {
        format!(" Goals  [{}/{}]  ↑↓ ", (ui.goals_scroll + view_h).min(total), total)
    } else {
        " Goals ".to_string()
    };
    let p = Paragraph::new(lines)
        .block(focusable_block(&title, focused))
        .scroll((ui.goals_scroll, 0));
    f.render_widget(p, area);
}

/// One goal's detail line.
fn goal_detail_line(g: &GoalView) -> Line<'static> {
    let (glyph, color) = state_glyph(&g.state);
    let measure = measure_str(g);
    let delta = if g.delta.abs() > f64::EPSILON {
        format!("  ▲{:+.0}", g.delta)
    } else {
        String::new()
    };
    let guard = if g.invariant { "  [guard]" } else { "" };
    // 🔒 = latched (recheck: once_met, judge no longer re-runs — saves tokens)
    let lock = if g.latched { "  🔒" } else { "" };
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(format!("{:<20}", fit(&g.id, 20)), Style::default().fg(color).bold()),
        Span::raw(format!("{:<11}", g.goal_type)),
        Span::styled(format!("{:<11}", measure), Style::default().fg(color)),
        Span::styled(format!("{:<8}", delta), Style::default().fg(Color::Green)),
        Span::styled(format!("w{:<4}", fmt_num(g.weight)), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("judge:{}", g.judge_kind), Style::default().fg(Color::Blue)),
        Span::styled(guard.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(lock.to_string(), Style::default().fg(Color::Cyan)),
    ])
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
        let mut v = Vec::new();
        let story = if s.summary_cumulative.is_empty() { "(no summary yet)" } else { &s.summary_cumulative };
        v.push(Line::from(vec![
            Span::styled("story:  ", Style::default().fg(Color::Green).bold()),
            Span::raw(story.to_string()),
        ]));
        if !s.summary_windowed.is_empty() {
            v.push(Line::from(vec![
                Span::styled("recent: ", Style::default().fg(Color::Blue).bold()),
                Span::raw(s.summary_windowed.clone()),
            ]));
        }
        v
    };
    let p = Paragraph::new(body)
        .block(title_block(" Summary   (Tab=focus · ↑↓=scroll · f=follow · q=quit) "))
        .wrap(Wrap { trim: true });
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

fn state_glyph(state: &str) -> (&'static str, Color) {
    match state {
        "met" => ("✔", Color::Green),
        "regressed" => ("⚠", Color::Red),
        "in_progress" => ("◑", Color::Yellow),
        _ => ("·", Color::DarkGray),
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

fn phase_color(phase: &str) -> Color {
    match phase {
        "running" => Color::Green,
        "judging" => Color::Cyan,
        "backoff" => Color::Yellow,
        "done" => Color::Green,
        _ => Color::Gray,
    }
}

fn measure_str(g: &GoalView) -> String {
    match g.goal_type.as_str() {
        "binary" => if g.value > 0.0 { "yes".into() } else { "no".into() },
        "percentage" => format!("{:.0}/{:.0}%", g.value, g.target),
        _ => format!("{:.0}/{:.0}", g.value, g.max),
    }
}

fn fmt_dur(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{:02}s", secs % 60)
    }
}

/// Absolute local wall-clock for the loop's start, as `HH:MM:SS <zone>`. We
/// avoid a chrono dependency: the local UTC offset comes from libc's
/// `localtime_r` (cached once) so a user in CEST sees their wall clock, not UTC.
/// The zone label (e.g. `UTC+2`) names what's being shown.
fn fmt_started(epoch: u64) -> String {
    if epoch == 0 {
        return "—".to_string();
    }
    let (h, m, s) = crate::localtime::local_hms(epoch);
    format!("{h:02}:{m:02}:{s:02} {}", crate::localtime::offset_label())
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

/// Compact a weight like 1.0 → "1", 1.5 → "1.5".
fn fmt_num(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n:.1}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ActivityEvent, DashboardState, GoalView};
    use ratatui::backend::TestBackend;

    fn demo_state() -> DashboardState {
        DashboardState {
            project: "myproject".into(),
            model: "claude-opus-4-8".into(),
            stop_when: "all_goals".into(),
            halt_when: "regressed".into(),
            started_at_epoch: 1_700_000_000,
            up_secs: 11520,
            session: 7,
            lifetime_session: 7,
            phase: "running".into(),
            idle_secs: 12,
            tokens_spent: 2_100_000,
            budget_total: Some(5_000_000),
            cost_spent: 1.25,
            cost_limit: Some(5.0),
            goals_met: 1,
            goals_total: 2,
            goals: vec![
                GoalView {
                    id: "tests_pass".into(),
                    goal_type: "cardinal".into(),
                    state: "in_progress".into(),
                    invariant: false,
                    value: 18.0,
                    max: 28.0,
                    target: 28.0,
                    weight: 1.0,
                    delta: 3.0,
                    rationale: "18/28 tests passing; the remaining ten exercise the BOUND-limited \
                                instances that need the conflict-analysis bound-climb lever to land."
                        .into(),
                    judge_kind: "script".into(),
                    latched: false,
                },
                GoalView {
                    id: "no_regressions".into(),
                    goal_type: "binary".into(),
                    state: "met".into(),
                    invariant: true,
                    value: 1.0,
                    max: 1.0,
                    target: 1.0,
                    weight: 2.0,
                    delta: 0.0,
                    rationale: "build green".into(),
                    judge_kind: "script".into(),
                    latched: true,
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
        assert!(text.contains("session"));      // info label
        assert!(text.contains("#7"));            // session number
        assert!(text.contains("claude-opus-4-8")); // model in Info, not title
        assert!(text.contains("Progress"));
        assert!(text.contains("1/2 goals"));     // segmented bar label
        assert!(text.contains("Goals"));
        assert!(text.contains("tests_pass"));
        assert!(text.contains("18/28"));         // cardinal measure
        assert!(text.contains("Activity"));
        assert!(text.contains("14:07:05"));      // activity tail timestamp
        assert!(text.contains("bound improved")); // activity tail text
        assert!(text.contains("Summary"));
        assert!(text.contains("Building the parser")); // cumulative summary
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
    fn human_and_dur() {
        assert_eq!(human(2_100_000), "2.1M");
        assert_eq!(human(5_000), "5.0k");
        assert_eq!(human(42), "42");
        assert_eq!(fmt_dur(11520), "3h12m");
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
}
