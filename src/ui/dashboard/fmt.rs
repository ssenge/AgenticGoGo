//! Pure formatting + color helpers for the dashboard TUI.

use crate::state::{DashboardState, Phase};
use ratatui::prelude::*;

pub(super) fn label(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().fg(Color::DarkGray))
}
pub(super) fn sep() -> Span<'static> {
    Span::styled("  ", Style::default())
}

/// Green at 100%, through yellow, to red near 0% — the "approaching red" spec.
pub(super) fn grade_color(frac: f64) -> Color {
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
pub(super) fn money(c: Option<f64>) -> String {
    match c {
        Some(c) => format!("${c:.2}"),
        None => "—".to_string(),
    }
}

/// The total cost to show for the per-agent panel: the aggregate spend, or `None` (→ "—") when NO
/// agent could report a price — so a fully non-reporting run never prints a lying "$0.00".
pub(super) fn agg_cost(s: &DashboardState) -> Option<f64> {
    let reported = if s.per_agent.is_empty() {
        s.cost_spent > 0.0
    } else {
        s.per_agent.values().any(|u| u.cost.is_some())
    };
    reported.then_some(s.cost_spent)
}

/// A stable accent per agent so claude/codex/copilot read apart at a glance.
pub(super) fn agent_color(agent: &str) -> Color {
    match agent {
        "claude" => Color::Magenta,
        "codex" => Color::Green,
        "copilot" => Color::Blue,
        _ => Color::Cyan,
    }
}

pub(super) fn activity_glyph(kind: &str) -> (&'static str, Color) {
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
pub(super) fn phase_color(phase: &Phase) -> Color {
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
pub(super) fn fmt_started(epoch: u64) -> String {
    if epoch == 0 {
        return "—".to_string();
    }
    let (h, m, s) = crate::ui::localtime::local_hms(epoch);
    format!("{h:02}:{m:02}:{s:02} {}", crate::ui::localtime::offset_label())
}

pub(super) fn human(n: u64) -> String {
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
pub(super) fn fit(s: &str, n: usize) -> String {
    if n == 0 || s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Word-wrap `text` to `width`, prefixing the first line with `first` and every
/// continuation line with `cont` (so a rationale reads as an indented block).
pub(super) fn wrap_indent(text: &str, first: &str, cont: &str, width: usize) -> Vec<String> {
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
pub(super) fn wrapped_block(prefix: &'static str, color: Color, text: &str, max: usize, inner_w: usize) -> Vec<Line<'static>> {
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
