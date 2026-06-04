//! `agg dashboard` — the live TUI. A separate viewer process that polls
//! `.agg/state.json` (written by `agg run`) and repaints in place with color.
//!
//! It is a *view*: it never drives the loop, never reads the firehose. If the
//! state file is missing it shows "waiting for `agg run`…". Quit with `q`/Esc.

use crate::state::DashboardState;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::{execute, terminal};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use std::io::stdout;
use std::path::Path;
use std::time::Duration;

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
    loop {
        let state = DashboardState::read(dir);
        term.draw(|f| draw(f, dir, state.as_ref()))?;

        // poll input ~4x/sec; repaint regardless to pick up state changes.
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ => {}
                }
            }
        }
        // if the run finished, keep showing the final frame but allow q to exit.
    }
    Ok(())
}

fn draw(f: &mut Frame, dir: &Path, state: Option<&DashboardState>) {
    let area = f.area();
    let Some(s) = state else {
        let msg = Paragraph::new(format!(
            "waiting for `agg run`…\n\n(no {} yet — start the loop in another terminal)",
            DashboardState::path(dir).display()
        ))
        .block(title_block(" AgenticGoGo "));
        f.render_widget(msg, area);
        return;
    };

    // layout: header gauge | goals table | footer (session+now+think) | summaries
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                       // header gauge
            Constraint::Min(5),                          // goals
            Constraint::Length(5),                       // session/now/think
            Constraint::Length(4),                       // summaries
        ])
        .split(area);

    draw_header(f, chunks[0], s);
    draw_goals(f, chunks[1], s);
    draw_status(f, chunks[2], s);
    draw_summaries(f, chunks[3], s);
}

fn title_block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title.bold())
}

/// Header: the big Goals N/M gauge, color graded green→red by fraction met.
fn draw_header(f: &mut Frame, area: Rect, s: &DashboardState) {
    let frac = if s.goals_total == 0 { 0.0 } else { s.goals_met as f64 / s.goals_total as f64 };
    let up = fmt_dur(s.up_secs);
    let title = format!(
        " AgenticGoGo · {} · up {up} · stop: {} ",
        if s.project.is_empty() { "—" } else { &s.project },
        s.stop_when
    );
    let label = format!("Goals {}/{}   {:.0}%", s.goals_met, s.goals_total, frac * 100.0);
    let gauge = Gauge::default()
        .block(title_block(&title))
        .gauge_style(Style::default().fg(grade_color(frac)).bg(Color::Black))
        .ratio(frac.clamp(0.0, 1.0))
        .label(label.bold());
    f.render_widget(gauge, area);
}

/// Goals table: one row per goal, glyph + measure + delta + judge, colored by state.
fn draw_goals(f: &mut Frame, area: Rect, s: &DashboardState) {
    let mut lines: Vec<Line> = Vec::new();
    for g in &s.goals {
        let (glyph, color) = state_glyph(&g.state);
        let measure = measure_str(g);
        let delta = if g.delta.abs() > f64::EPSILON {
            format!("  ▲{:+.0}", g.delta)
        } else {
            String::new()
        };
        let inv = if g.invariant { " (guard)" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::styled(format!("{:<18}", g.id), Style::default().fg(color).bold()),
            Span::raw(format!("{:<11}", g.goal_type)),
            Span::styled(format!("{:<10}", measure), Style::default().fg(color)),
            Span::styled(delta, Style::default().fg(Color::Green)),
            Span::styled(inv.to_string(), Style::default().fg(Color::Yellow)),
            Span::raw(format!("   judge:{}", g.judge_kind)),
        ]);
        lines.push(line);
        if !g.rationale.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("      ↳ {}", truncate(&g.rationale, area.width.saturating_sub(8) as usize)),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    let p = Paragraph::new(lines).block(title_block(" Goals "));
    f.render_widget(p, area);
}

/// Status: session/phase/idle/tokens + the live now/think lines.
fn draw_status(f: &mut Frame, area: Rect, s: &DashboardState) {
    let idle_color = if s.idle_secs >= 240 { Color::Red } else { Color::Gray };
    let tokens = match s.budget_total {
        Some(t) => format!("{} / {}", human(s.tokens_spent), human(t)),
        None => human(s.tokens_spent),
    };
    let header = Line::from(vec![
        Span::styled(format!("session #{}  ", s.session), Style::default().bold()),
        Span::styled(format!("{}  ", s.phase), Style::default().fg(phase_color(&s.phase))),
        Span::styled(format!("idle {}s  ", s.idle_secs), Style::default().fg(idle_color)),
        Span::raw(format!("tokens {tokens}")),
    ]);
    let w = area.width.saturating_sub(10) as usize;
    let now = Line::from(vec![
        Span::styled("now:   ", Style::default().fg(Color::Cyan)),
        Span::raw(truncate(&s.now, w)),
    ]);
    let think = Line::from(vec![
        Span::styled("think: ", Style::default().fg(Color::Magenta)),
        Span::styled(truncate(&s.think, w), Style::default().fg(Color::Gray)),
    ]);
    let p = Paragraph::new(vec![header, now, think]).block(title_block(" Activity "));
    f.render_widget(p, area);
}

/// Summaries: the cumulative + windowed LLM lines.
fn draw_summaries(f: &mut Frame, area: Rect, s: &DashboardState) {
    let w = area.width.saturating_sub(14) as usize;
    let cumulative = Line::from(vec![
        Span::styled("story: ", Style::default().fg(Color::Green)),
        Span::raw(truncate(&s.summary_cumulative, w)),
    ]);
    let windowed = Line::from(vec![
        Span::styled("recent: ", Style::default().fg(Color::Blue)),
        Span::raw(truncate(&s.summary_windowed, w)),
    ]);
    let body = if s.finished {
        vec![Line::from(Span::styled(
            format!("✔ FINISHED: {}", s.finish_reason),
            Style::default().fg(Color::Green).bold(),
        ))]
    } else {
        vec![cumulative, windowed]
    };
    let p = Paragraph::new(body).block(title_block(" Summary  (q to quit) "));
    f.render_widget(p, area);
}

// ---------------- styling helpers ----------------

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

fn phase_color(phase: &str) -> Color {
    match phase {
        "running" => Color::Green,
        "judging" => Color::Cyan,
        "backoff" => Color::Yellow,
        "done" => Color::Green,
        _ => Color::Gray,
    }
}

fn measure_str(g: &crate::state::GoalView) -> String {
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

fn human(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if n == 0 || s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DashboardState, GoalView};
    use ratatui::backend::TestBackend;

    fn demo_state() -> DashboardState {
        DashboardState {
            project: "myproject".into(),
            stop_when: "all_goals".into(),
            up_secs: 11520,
            session: 7,
            phase: "running".into(),
            idle_secs: 12,
            tokens_spent: 2_100_000,
            budget_total: Some(5_000_000),
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
                    delta: 3.0,
                    rationale: "18/28 tests passing".into(),
                    judge_kind: "script".into(),
                },
                GoalView {
                    id: "no_regressions".into(),
                    goal_type: "binary".into(),
                    state: "met".into(),
                    invariant: true,
                    value: 1.0,
                    max: 1.0,
                    target: 1.0,
                    delta: 0.0,
                    rationale: "build green".into(),
                    judge_kind: "script".into(),
                },
            ],
            now: "🔧 $ Run the test suite".into(),
            think: "implementing the remaining parser cases".into(),
            summary_cumulative: "Building the parser; 18/28 tests pass, no regressions.".into(),
            summary_windowed: "Fixed an edge case; the suite is greener.".into(),
            seq: 5,
            finished: false,
            finish_reason: String::new(),
        }
    }

    /// Render the full dashboard into a headless TestBackend buffer and assert key
    /// content appears — proves draw() doesn't panic and lays out the right text.
    #[test]
    fn renders_into_test_backend() {
        let backend = TestBackend::new(90, 24);
        let mut term = Terminal::new(backend).unwrap();
        let s = demo_state();
        term.draw(|f| draw(f, Path::new("."), Some(&s))).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("AgenticGoGo"));
        assert!(text.contains("Goals 1/2"));
        assert!(text.contains("tests_pass"));
        assert!(text.contains("18/28"));      // cardinal measure
        assert!(text.contains("session #7"));
        assert!(text.contains("test suite")); // now: line
        assert!(text.contains("Building the parser")); // cumulative summary
    }

    #[test]
    fn missing_state_shows_waiting() {
        let backend = TestBackend::new(80, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, Path::new("/nonexistent"), None)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("waiting for"));
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
}
