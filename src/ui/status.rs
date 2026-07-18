//! `agg status` — a cheap, read-only snapshot of a run.
//!
//! Unlike `agg plan` (which RE-RUNS every judge — possibly an expensive LLM call — to compute a
//! fresh scoreboard), `status` just reads the `agg/state/state.json` snapshot the running loop
//! already publishes and renders it. That matches the `/agg:status` skill's "read the digest,
//! don't re-judge" discipline, so the same word means the same (cheap) thing in both places.

use crate::state::{DashboardState, JudgeView};
use std::path::Path;

/// Render the current run snapshot to a human string, or an actionable hint if there's no
/// snapshot yet (the loop has never run in this dir).
pub fn render(dir: &Path) -> String {
    match DashboardState::read(dir) {
        Some(s) => render_state(&s),
        None => "no run snapshot yet (agg/state/state.json not found).\n  \
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
            "no run snapshot yet (agg/state/state.json not found) — run `agg run` first \
             (it publishes state as it goes)."
        ),
    }
}

/// Pure renderer over a snapshot — separated so it's unit-testable without touching disk.
fn render_state(s: &DashboardState) -> String {
    let mut out = String::new();
    let up = crate::util::fmt_dur(s.up_secs);
    let status = if s.finished { format!("done — {}", s.finish_reason) } else { format!("{} (live)", s.phase) };
    // header — the DoD-set aggregate (goals_met/total ranges over the DoD-set, not the run-set, §5.3)
    out.push_str(&format!(
        "{}  ·  judges {}/{}  ·  session #{} (#{} lifetime)  ·  up {up}  ·  {status}\n",
        s.project, s.goals_met, s.goals_total, s.session, s.lifetime_session
    ));
    // the current STEP and who ran it (§7.4) — a mixed run is uninterpretable without the agent+model.
    if !s.step.is_empty() {
        let agent = if s.step_agent.is_empty() { "—" } else { s.step_agent.as_str() };
        // empty model = agg pins no `--model`, the agent uses its own default (e.g. codex, whose
        // DEFAULT_MODEL is "" on purpose). Say so rather than showing a bare em-dash.
        let model = if s.step_model.is_empty() { "agent default" } else { s.step_model.as_str() };
        out.push_str(&format!("step   {}  ·  {agent} / {model}\n", s.step));
    }
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
        out.push_str(&format!("memory {} (LOG.md)\n", crate::util::human_bytes(s.memory_bytes)));
    }
    // per-agent token + cost breakdown (§7.4) — otherwise a mixed run's totals are uninterpretable.
    if !s.per_agent.is_empty() {
        out.push_str("\nper-agent\n");
        for (agent, u) in &s.per_agent {
            out.push_str(&format!("  {:<10} {:>8} tok   {}\n", agent, human(u.tokens), money(u.cost)));
        }
        // total cost is "—" (not "$0.00") if NO agent could report a price — never lie.
        let total_cost = if s.per_agent.values().any(|u| u.cost.is_some()) { Some(s.cost_spent) } else { None };
        out.push_str(&format!("  {:<10} {:>8} tok   {}\n", "total", human(s.tokens_spent), money(total_cost)));
    }
    out.push('\n');
    // per-judge scoreboard (§7.4) — DoD-set first, then any run-set control judges (e.g. `stalled`).
    let judges = s.judge_views();
    if judges.is_empty() {
        out.push_str("(no judges in snapshot)\n");
    } else {
        let (dod, run): (Vec<&JudgeView>, Vec<&JudgeView>) = judges.iter().partition(|j| j.in_dod);
        for j in &dod {
            out.push_str(&judge_block(j));
        }
        if !run.is_empty() {
            out.push_str("\nrun-set (not counted toward done)\n");
            for j in &run {
                out.push_str(&judge_block(j));
            }
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

/// One judge's scoreboard entry (§7.4): a compact line + its rationale, e.g.
/// ```text
/// ◑ coverage           64/80 [████████░░]  ▲+12  judge:llm
///     ↳ 64% of modules covered (was 52% last step)
/// ```
fn judge_block(j: &JudgeView) -> String {
    let delta = if j.value.is_some() && j.delta.abs() > f64::EPSILON {
        format!("  ▲{:+.0}", j.delta)
    } else {
        String::new()
    };
    let guard = if j.invariant { " (guard)" } else { "" };
    let mut b = format!(
        "{} {:<18} {}{delta}  judge:{}{guard}\n",
        judge_glyph(j),
        j.name,
        judge_measure(j),
        j.kind
    );
    if !j.rationale.is_empty() {
        b.push_str(&format!("    ↳ {}\n", j.rationale));
    }
    b
}

/// State glyph for a judge — a broken judge (`error`) gets its own ⊘, distinct from a clean "not met".
fn judge_glyph(j: &JudgeView) -> &'static str {
    if j.error.is_some() {
        return "⊘";
    }
    match j.state.as_str() {
        "met" => "✔",
        "regressed" => "⚠",
        "in_progress" => "◑",
        _ => "·",
    }
}

/// The measure column. A binary judge shows met/unmet (NOT a lying `0`, §7.4); a numeric one shows
/// value/target with a proportional bar; a broken one shows `error`.
fn judge_measure(j: &JudgeView) -> String {
    if j.error.is_some() {
        return "error".to_string();
    }
    match j.value {
        None => if j.met { "met".to_string() } else { "unmet".to_string() },
        Some(v) => {
            let frac = if j.target > 0.0 {
                (v / j.target).clamp(0.0, 1.0)
            } else if j.met {
                1.0
            } else {
                0.0
            };
            format!("{:.0}/{:.0} {}", v, j.target, bar_str(frac, 10))
        }
    }
}

/// A tiny text meter, `[████░░░░]`, for a numeric judge's progress toward its target.
fn bar_str(frac: f64, w: usize) -> String {
    let filled = ((frac * w as f64).round() as usize).min(w);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(w - filled))
}

/// A per-agent cost cell: a real price, or "—" for an agent that cannot report one (never "$0.00").
fn money(c: Option<f64>) -> String {
    match c {
        Some(c) => format!("${c:.2}"),
        None => "—".to_string(),
    }
}

/// Compact a token count: 903411 → "903.4k", 2_100_000 → "2.1M".
fn human(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { (n as f64 / d as f64) * 100.0 }
}

fn pctf(n: f64, d: f64) -> f64 {
    if d == 0.0 { 0.0 } else { (n / d) * 100.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::{AgentUsage, JudgeView};
    use std::collections::BTreeMap;

    /// A mixed claude/codex run, mid-sequence: a binary invariant that's met, a numeric judge
    /// climbing, a broken (errored) judge, and a run-set `stalled` control judge. `copilot` is a
    /// third agent that cannot report a price — its cost must render "—".
    fn sample() -> DashboardState {
        let mut per_agent = BTreeMap::new();
        per_agent.insert("claude".to_string(), AgentUsage { tokens: 1_400_000, cost: Some(1.10) });
        per_agent.insert("codex".to_string(), AgentUsage { tokens: 600_000, cost: Some(0.15) });
        per_agent.insert("copilot".to_string(), AgentUsage { tokens: 100_000, cost: None });
        DashboardState {
            project: "demo".into(),
            step: "reconsider".into(),
            step_agent: "claude".into(),
            step_model: "claude-opus-4-8".into(),
            up_secs: 3 * 3600 + 12 * 60,
            session: 7,
            lifetime_session: 11,
            phase: crate::state::Phase::Staging,
            tokens_spent: 2_100_000,
            budget_total: Some(5_000_000),
            cost_spent: 1.25,
            cost_limit: Some(5.0),
            memory_bytes: 2048,
            goals_met: 1,
            goals_total: 3,
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
                    rationale: "cargo build: 0 warnings".into(),
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
                    rationale: "64% of modules covered (was 52%)".into(),
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
                    rationale: "judge failed: rg: command not found".into(),
                    error: Some("rg: command not found".into()),
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
                    rationale: "no state-file change across 2 sessions".into(),
                    error: None,
                },
            ],
            summary_cumulative: "Building the parser; tests green, coverage lagging.".into(),
            summary_windowed: "Fixed the nested-group case.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn renders_header_step_judges_and_summaries() {
        let out = render_state(&sample());
        assert!(out.contains("demo"));
        assert!(out.contains("judges 1/3")); // DoD-set aggregate (excludes run-set `stalled`)
        assert!(out.contains("session #7 (#11 lifetime)"));
        assert!(out.contains("up 3h12m"));
        assert!(out.contains("step   reconsider  ·  claude / claude-opus-4-8"));
        assert!(out.contains("tokens 2100000 / 5000000 (42%)"));
        assert!(out.contains("usage  $1.25 / $5.00 (25%, API-equiv)"));
        assert!(out.contains("memory 2.0 KB"));
        // per-judge scoreboard
        assert!(out.contains("✔ build_passes"));
        assert!(out.contains("(guard)")); // the invariant is flagged
        assert!(out.contains("◑ coverage"));
        assert!(out.contains("64/80")); // numeric judge shows value/target, not value/max
        assert!(out.contains("▲+12"));
        assert!(out.contains("story:  Building the parser"));
        assert!(out.contains("recent: Fixed the nested-group case."));
    }

    /// The §7.4 defect the whole migration exists to fix: a binary/errored judge must read met/unmet
    /// (or `error`), NEVER a lying `0`.
    #[test]
    fn binary_and_errored_judges_never_render_zero() {
        let out = render_state(&sample());
        // met binary judge: "met", not "1/1" or "0"
        let build_line = out.lines().find(|l| l.contains("build_passes")).unwrap();
        assert!(build_line.contains("met"), "binary judge shows met: {build_line}");
        assert!(!build_line.contains("0/1") && !build_line.contains("0/0"), "no fabricated number: {build_line}");
        // broken judge: ⊘ + "error", surfaced with its reason
        let todo_line = out.lines().find(|l| l.contains("no_todos")).unwrap();
        assert!(todo_line.contains('⊘') && todo_line.contains("error"), "broken judge flagged: {todo_line}");
        assert!(out.contains("rg: command not found"), "the error reason is surfaced");
    }

    /// The run-set control judge (`stalled`) is shown apart, so "why we stalled" is visible without
    /// polluting the DoD-set count.
    #[test]
    fn run_set_judges_are_shown_separately() {
        let out = render_state(&sample());
        assert!(out.contains("run-set (not counted toward done)"));
        assert!(out.contains("✔ stalled"));
        assert!(out.contains("no state-file change"));
    }

    /// The §7.4 per-agent breakdown: one row per agent + a total; a non-reporting agent shows "—".
    #[test]
    fn per_agent_breakdown_with_unreporting_agent() {
        let out = render_state(&sample());
        assert!(out.contains("per-agent"));
        assert!(out.contains("claude"));
        assert!(out.contains("$1.10"));
        assert!(out.contains("codex"));
        assert!(out.contains("$0.15"));
        // copilot cannot report a price → "—", never "$0.00"
        let copilot = out.lines().find(|l| l.contains("copilot")).unwrap();
        assert!(copilot.contains('—') && !copilot.contains("$0.00"), "unreporting agent shows —: {copilot}");
        assert!(out.contains("total"));
    }

    /// A pre-§7.4 state.json (only `goals`, no `judges`) still renders via the compatibility bridge.
    #[test]
    fn legacy_goals_still_render() {
        use crate::state::GoalView;
        let mut s = DashboardState { project: "legacy".into(), goals_met: 1, goals_total: 1, ..Default::default() };
        s.goals = vec![GoalView {
            id: "tests_pass".into(),
            goal_type: "cardinal".into(),
            state: "met".into(),
            value: 42.0,
            max: 42.0,
            target: 42.0,
            delta: 5.0,
            rationale: "all green".into(),
            judge_kind: "script".into(),
            ..Default::default()
        }];
        let out = render_state(&s);
        assert!(out.contains("✔ tests_pass"), "legacy goal mapped onto the judge scoreboard: {out}");
        assert!(out.contains("42/42"));
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
        // the aggregate "usage" line, not the per-agent header — check the exact prefix.
        assert!(!out.contains("usage  $"), "no usage line when uncapped + nothing spent: {out}");
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
        s.finish_reason = "3/3 judges met after 9 session(s)".into();
        let out = render_state(&s);
        assert!(out.contains("done — 3/3 judges met"));
    }
}
