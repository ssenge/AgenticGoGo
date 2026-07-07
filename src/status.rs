//! `agg status` — a cheap, read-only snapshot of a run.
//!
//! Unlike `agg plan` (which RE-RUNS every judge — possibly an expensive LLM call — to compute a
//! fresh scoreboard), `status` just reads the `.agg/state.json` snapshot the running loop
//! already publishes and renders it. That matches the `/agg:status` skill's "read the digest,
//! don't re-judge" discipline, so the same word means the same (cheap) thing in both places.

use crate::state::{DashboardState, GoalView};
use std::path::Path;

/// Render the current run snapshot to a human string, or an actionable hint if there's no
/// snapshot yet (the loop has never run in this dir).
pub fn render(dir: &Path) -> String {
    match DashboardState::read(dir) {
        Some(s) => render_state(&s),
        None => "no run snapshot yet (.agg/state.json not found).\n  \
                 • `agg run` to start the loop (it publishes state as it goes), or\n  \
                 • `agg plan` to evaluate the judges once now (a dry run — this re-judges)."
            .to_string(),
    }
}

/// Render the current run snapshot as pretty JSON (the full `DashboardState`) for scripting.
/// Errors with an actionable hint if no snapshot exists yet, so `agg status --json` in a
/// fresh project fails loud rather than emitting misleading empty JSON.
pub fn render_json(dir: &Path) -> anyhow::Result<String> {
    match DashboardState::read(dir) {
        Some(s) => Ok(serde_json::to_string_pretty(&s)?),
        None => anyhow::bail!(
            "no run snapshot yet (.agg/state.json not found) — run `agg run` first \
             (it publishes state as it goes)."
        ),
    }
}

/// Pure renderer over a snapshot — separated so it's unit-testable without touching disk.
fn render_state(s: &DashboardState) -> String {
    let mut out = String::new();
    let up = fmt_dur(s.up_secs);
    let status = if s.finished { format!("done — {}", s.finish_reason) } else { format!("{} (live)", s.phase) };
    out.push_str(&format!(
        "{}  ·  goals {}/{}  ·  session #{} (#{} lifetime)  ·  up {up}  ·  {status}\n",
        s.project, s.goals_met, s.goals_total, s.session, s.lifetime_session
    ));
    // budget line (only when a ceiling is set)
    if let Some(total) = s.budget_total {
        out.push_str(&format!("tokens {} / {} ({:.0}%)\n", s.tokens_spent, total, pct(s.tokens_spent, total)));
    } else {
        out.push_str(&format!("tokens {} (no budget)\n", s.tokens_spent));
    }
    // usage line — the API-equivalent price Claude reports (`total_cost_usd`), NOT a subscription
    // charge; on a Max/Pro plan it's a usage proxy, not money billed. Shown whenever a dollar cap
    // is set or any spend is recorded, so a token-only run stays uncluttered.
    match s.cost_limit {
        Some(limit) => out.push_str(&format!(
            "usage  ${:.2} / ${:.2} ({:.0}%, API-equiv)\n",
            s.cost_spent, limit, pctf(s.cost_spent, limit)
        )),
        None if s.cost_spent > 0.0 => out.push_str(&format!("usage  ${:.2} (API-equiv, no cap)\n", s.cost_spent)),
        None => {}
    }
    // memory line — shown only once the durable file has content, so a fresh run stays clean.
    if s.memory_bytes > 0 {
        out.push_str(&format!("memory {} (AGG_MEMORY.md)\n", human_bytes(s.memory_bytes)));
    }
    out.push('\n');
    // per-goal lines
    if s.goals.is_empty() {
        out.push_str("(no goals in snapshot)\n");
    } else {
        for g in &s.goals {
            out.push_str(&goal_line(g));
            out.push('\n');
        }
    }
    // the cheap LLM summaries the loop already computed
    if !s.summary_cumulative.is_empty() {
        out.push_str(&format!("\nstory:  {}\n", s.summary_cumulative));
    }
    if !s.summary_windowed.is_empty() {
        out.push_str(&format!("recent: {}\n", s.summary_windowed));
    }
    out
}

/// One compact scoreboard line for a goal, e.g.
/// `✔ tests_pass        cardinal   42/42   ▲+5   judge:script   🔒`
fn goal_line(g: &GoalView) -> String {
    let glyph = match g.state.as_str() {
        "met" => "✔",
        "regressed" => "⚠",
        "in_progress" => "◑",
        _ => "·",
    };
    let measure = match g.goal_type.as_str() {
        "binary" => if g.state == "met" { "yes".to_string() } else { "no".to_string() },
        "percentage" => format!("{:.0}/{:.0}%", g.value, g.target),
        _ => format!("{:.0}/{:.0}", g.value, g.max), // cardinal
    };
    let delta = if g.delta > 0.0 { format!("▲+{:.0}", g.delta) } else { String::new() };
    let guard = if g.invariant { "(guard)" } else { "" };
    let latched = if g.latched { "🔒" } else { "" };
    format!(
        "{glyph} {:<18} {:<10} {:<10} {:<6} judge:{:<10} {latched}",
        g.id, g.goal_type, measure, delta, format!("{} {}", g.judge_kind, guard).trim()
    )
    .trim_end()
    .to_string()
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { (n as f64 / d as f64) * 100.0 }
}

fn pctf(n: f64, d: f64) -> f64 {
    if d == 0.0 { 0.0 } else { (n / d) * 100.0 }
}

/// Compact byte size, e.g. "1.2 KB" / "640 B". Used for the memory line.
fn human_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn fmt_dur(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DashboardState {
        DashboardState {
            project: "demo".into(),
            up_secs: 3 * 3600 + 12 * 60,
            session: 7,
            lifetime_session: 11,
            phase: "running".into(),
            tokens_spent: 2_100_000,
            budget_total: Some(5_000_000),
            cost_spent: 1.25,
            cost_limit: Some(5.0),
            memory_bytes: 2048,
            goals_met: 1,
            goals_total: 2,
            goals: vec![
                GoalView {
                    id: "tests_pass".into(),
                    goal_type: "cardinal".into(),
                    state: "met".into(),
                    invariant: false,
                    value: 42.0,
                    max: 42.0,
                    target: 42.0,
                    weight: 1.0,
                    delta: 5.0,
                    rationale: "all green".into(),
                    judge_kind: "script".into(),
                    latched: false,
                },
                GoalView {
                    id: "coverage".into(),
                    goal_type: "percentage".into(),
                    state: "in_progress".into(),
                    invariant: false,
                    value: 81.0,
                    max: 100.0,
                    target: 90.0,
                    weight: 1.0,
                    delta: 0.0,
                    rationale: String::new(),
                    judge_kind: "script".into(),
                    latched: false,
                },
            ],
            summary_cumulative: "Building the parser; tests green, coverage lagging.".into(),
            summary_windowed: "Fixed the nested-group case.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn renders_header_goals_and_summaries() {
        let out = render_state(&sample());
        assert!(out.contains("demo"));
        assert!(out.contains("goals 1/2"));
        assert!(out.contains("session #7 (#11 lifetime)"));
        assert!(out.contains("up 3h12m"));
        assert!(out.contains("tokens 2100000 / 5000000 (42%)"));
        assert!(out.contains("usage  $1.25 / $5.00 (25%, API-equiv)"));
        assert!(out.contains("memory 2.0 KB"));
        assert!(out.contains("✔ tests_pass"));
        assert!(out.contains("◑ coverage"));
        assert!(out.contains("▲+5"));
        assert!(out.contains("story:  Building the parser"));
        assert!(out.contains("recent: Fixed the nested-group case."));
    }

    #[test]
    fn handles_no_budget() {
        let mut s = sample();
        s.budget_total = None;
        let out = render_state(&s);
        assert!(out.contains("tokens 2100000 (no budget)"));
    }

    #[test]
    fn cost_line_hidden_when_no_cap_and_no_spend() {
        let mut s = sample();
        s.cost_limit = None;
        s.cost_spent = 0.0;
        let out = render_state(&s);
        assert!(!out.contains("usage"), "no usage line when uncapped + nothing spent: {out}");
    }

    #[test]
    fn cost_line_shows_spend_even_without_cap() {
        let mut s = sample();
        s.cost_limit = None;
        s.cost_spent = 2.40;
        let out = render_state(&s);
        assert!(out.contains("usage  $2.40 (API-equiv, no cap)"), "uncapped spend still shown: {out}");
    }

    #[test]
    fn finished_run_shows_reason() {
        let mut s = sample();
        s.finished = true;
        s.finish_reason = "2/2 goals met after 9 session(s)".into();
        let out = render_state(&s);
        assert!(out.contains("done — 2/2 goals met"));
    }
}
