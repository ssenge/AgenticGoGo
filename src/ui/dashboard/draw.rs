//! Widget rendering: the `draw()` layout and every per-pane draw helper.

use super::fmt::*;
use super::{DashboardUi, Focus};
use crate::state::{ActivityEvent, DashboardState, JudgeView};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::path::Path;

pub(super) fn draw(f: &mut Frame, dir: &Path, state: Option<&DashboardState>, ui: &mut DashboardUi) {
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
    draw_summary(f, chunks[5], s, ui);
}

pub(super) fn title_block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title.bold())
}

/// The top title row — the application name ONLY (per the redesign: project/uptime/
/// stop-condition moved out of the title into the Info block). Rendered as a bold,
/// centered banner with no border to keep it light.
pub(super) fn draw_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(Span::styled(
        "AgenticGoGo",
        Style::default().fg(Color::Cyan).bold(),
    )))
    .alignment(Alignment::Center);
    f.render_widget(title, area);
}

/// A bordered block whose title gains a cyan ▸ marker + reverse style when focused,
/// so the user can see which pane the arrow keys drive.
pub(super) fn focusable_block(title: &str, focused: bool) -> Block<'_> {
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
pub(super) fn draw_info(f: &mut Frame, area: Rect, s: &DashboardState) {
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
pub(super) fn draw_progress(f: &mut Frame, area: Rect, s: &DashboardState) {
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
pub(super) fn draw_per_agent(f: &mut Frame, area: Rect, s: &DashboardState) {
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
pub(super) fn draw_judges(f: &mut Frame, area: Rect, s: &DashboardState, ui: &mut DashboardUi) {
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
pub(super) fn judge_detail_line(j: &JudgeView) -> Line<'static> {
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
pub(super) fn judge_measure_spans(j: &JudgeView, color: Color) -> Vec<Span<'static>> {
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
pub(super) fn judge_glyph(j: &JudgeView) -> (&'static str, Color) {
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
pub(super) fn draw_activity(f: &mut Frame, area: Rect, s: &DashboardState, ui: &mut DashboardUi) {
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
pub(super) fn activity_line(ev: &ActivityEvent, width: usize) -> Line<'static> {
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
pub(super) fn draw_summary(f: &mut Frame, area: Rect, s: &DashboardState, ui: &DashboardUi) {
    // INJECT INPUT MODE takes over the panel: an editable prompt line + a hint.
    if let Some(buf) = &ui.input {
        let p = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("inject› ", Style::default().fg(Color::Yellow).bold()),
                Span::raw(buf.as_str()),
                Span::styled("▏", Style::default().fg(Color::Yellow)), // caret
            ]),
            Line::from(Span::styled(
                "prepended to the NEXT worker session (same as `agg send inject`)",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(title_block(" Inject   (Enter=send · Esc=cancel) "));
        f.render_widget(p, area);
        return;
    }

    let mut body: Vec<Line> = Vec::new();
    // A just-sent confirmation (or error) rides above the story until the next keypress.
    if let Some(msg) = &ui.flash {
        let color = if msg.starts_with('✓') { Color::Green } else { Color::Red };
        body.push(Line::from(Span::styled(msg.clone(), Style::default().fg(color).bold())));
    }
    if s.finished {
        body.push(Line::from(Span::styled(
            format!("✔ FINISHED: {}", s.finish_reason),
            Style::default().fg(Color::Green).bold(),
        )));
    } else {
        // The panel is a fixed 3 content lines. Cap the cumulative story so the windowed `recent:`
        // line survives (it used to be shoved out entirely when the story was long); the story
        // ellipsizes on clip instead of vanishing mid-sentence (§7.4). A flash line borrows one row.
        let inner_w = area.width.saturating_sub(2) as usize;
        let story = if s.summary_cumulative.is_empty() { "(no summary yet)" } else { &s.summary_cumulative };
        let story_lines = if ui.flash.is_some() { 1 } else { 2 };
        body.extend(wrapped_block("story:  ", Color::Green, story, story_lines, inner_w));
        if !s.summary_windowed.is_empty() && ui.flash.is_none() {
            body.extend(wrapped_block("recent: ", Color::Blue, &s.summary_windowed, 1, inner_w));
        }
    }
    // No `.wrap()`: `wrapped_block` has already wrapped every line to the inner width and clamped the
    // line count, and Wrap{trim} would eat the continuation indent.
    let p = Paragraph::new(body).block(title_block(" Summary   (i=inject · Tab=focus · ↑↓=scroll · f=follow · q=quit) "));
    f.render_widget(p, area);
}
