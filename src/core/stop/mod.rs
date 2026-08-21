//! Safe stop-condition evaluator.
//!
//! A stop condition is a small boolean/numeric expression over goal verdicts —
//! e.g. `all_goals`, `goal_a OR goal_b`, `met_fraction >= 0.75`, `count_met >= 3`,
//! `any_regressed(invariants)`. This is **NOT** a general `eval`: it's a tiny
//! recursive-descent parser over a whitelisted grammar. Unknown identifiers are a
//! parse error, never arbitrary code.
//!
//! Grammar (precedence low→high):
//!   expr   := or
//!   or     := and (("OR"|"||") and)*
//!   and    := cmp (("AND"|"&&") cmp)*
//!   cmp    := atom (("=="|"!="|">="|"<="|">"|"<") atom)?
//!   atom   := NUMBER | ident | "(" expr ")" | "NOT" atom
//!   ident  := goal-id | goal-id "." ("value"|"max") | aggregate | aggregate "(" subset ")"
//!
//! Aggregates: count_met, total, met_fraction, any_regressed, count_regressed. The `(invariants)`
//! subset restricts an aggregate to invariant judges. **The aggregates range over the DoD-set**
//! (`done_if` ∪ `invariants`) — NOT the whole run-set — so `done_if: all_goals` cannot mean "done
//! when stuck" (§5.3). `weighted_fraction` is DROPPED.
//!
//! A bare judge id is its **`met` bool**, resolved over the WHOLE run-set (so `if stalled` reads
//! `stalled`). To compare the NUMBER a judge emitted, use the dotted accessor — `coverage.value >=
//! 80`, never `coverage >= 80` (the latter is a hard error: see [`Val::GoalBool`]).

use crate::core::model::Judge;
use anyhow::{bail, Context, Result};

/// The facts the evaluator needs about the current run-set + run.
mod lex;
mod parse;

use lex::{tokenize, Tok};
use parse::Parser;

pub struct StopContext<'a> {
    /// the WHOLE run-set. Aggregates filter to `judge.in_dod` (the DoD-set); bare names / accessors
    /// resolve over all of them.
    pub judges: &'a [Judge],
    /// ids of the judges that RAN this step and returned an `error` (backs `any_judge_error`).
    /// Empty when no judge ran — a step whose judges were skipped is not "stale true", it is
    /// honestly false: no judge ran, so no judge errored.
    pub judge_errors: &'a [String],
    /// cumulative output tokens spent this run (for budget guards)
    pub tokens_spent: u64,
    /// budget ceiling, if configured (source: `sequence.limits.tokens`)
    pub budget_total: Option<u64>,
    /// cumulative dollars spent this run (backs `over_cost`)
    pub cost_spent: f64,
    /// dollar ceiling, if configured (source: `sequence.limits.cost`)
    pub cost_limit: Option<f64>,
    /// sessions completed so far this run (backs `over_iterations`)
    pub sessions_done: u32,
    /// the `max_sessions` cap, if any (backs `over_iterations`)
    pub max_sessions: Option<u32>,
    /// END-TO-END seconds since the run began, human waiting INCLUDED (backs `wall_time`).
    ///
    /// Seconds, not hours, and end-to-end across resumes: `internal/HUMAN_LOOP.md` §7.4. The old
    /// `wall_hours` term is REMOVED rather than aliased, because the unit changed by 3600× and a
    /// mechanical rename would silently turn an 8-hour ceiling into an 8-second one.
    pub wall_secs: f64,
    /// seconds spent blocked inside a `hil_*` call, accumulated across resumes (backs
    /// `human_wait_time`).
    pub human_wait_secs: f64,
}

impl<'a> StopContext<'a> {
    /// Convenience for callers that only have the judge set (plan/validate paths).
    pub fn from_judges(judges: &'a [Judge]) -> Self {
        StopContext {
            judges,
            judge_errors: &[],
            tokens_spent: 0,
            budget_total: None,
            cost_spent: 0.0,
            cost_limit: None,
            sessions_done: 0,
            max_sessions: None,
            wall_secs: 0.0,
            human_wait_secs: 0.0,
        }
    }
}

impl<'a> StopContext<'a> {
    /// The DoD-set filter the aggregates range over (§5.3): `in_dod` normally, `invariant` when the
    /// `(invariants)` subset is named. Invariants are a subset of the DoD-set, so both are correct.
    fn in_scope(g: &Judge, invariants_only: bool) -> bool {
        if invariants_only {
            g.invariant
        } else {
            g.in_dod
        }
    }
    fn count_met(&self, invariants_only: bool) -> f64 {
        self.judges
            .iter()
            .filter(|g| Self::in_scope(g, invariants_only))
            .filter(|g| g.met())
            .count() as f64
    }
    fn count_regressed(&self, invariants_only: bool) -> f64 {
        self.judges
            .iter()
            .filter(|g| Self::in_scope(g, invariants_only))
            .filter(|g| g.regressed())
            .count() as f64
    }
    fn total(&self, invariants_only: bool) -> f64 {
        self.judges.iter().filter(|g| Self::in_scope(g, invariants_only)).count() as f64
    }
    /// Bare-name lookup ranges over the WHOLE run-set (an `if stalled` condition must read `stalled`,
    /// which is not in the DoD-set).
    fn judge_met(&self, id: &str) -> Option<bool> {
        self.judges.iter().find(|g| g.name == id).map(|g| g.met())
    }
    fn judge(&self, id: &str) -> Option<&Judge> {
        self.judges.iter().find(|g| g.name == id)
    }
}

/// Evaluate a stop expression against the current goals. Returns the bool result.
pub fn evaluate(expr: &str, ctx: &StopContext) -> Result<bool> {
    let tokens = tokenize(expr)?;
    let mut p = Parser { tokens: &tokens, pos: 0, ctx };
    let v = p.parse_expr()?;
    if p.pos != p.tokens.len() {
        bail!("trailing tokens in stop condition: `{expr}`");
    }
    Ok(v.as_bool())
}

/// Validate an expression at config-load time (with a dummy context: no verdicts yet).
///
/// This is where `coverage >= 80` — a judge's `met` bool compared to a number, which coerces to a
/// silently-always-false `1.0 >= 80.0` — is caught. The type error lives in [`Parser::parse_cmp`],
/// so it fires here at STARTUP rather than 3 sessions into a run.
pub fn validate(expr: &str, judges: &[Judge]) -> Result<()> {
    let ctx = StopContext::from_judges(judges);
    evaluate(expr, &ctx).map(|_| ()).with_context(|| format!("invalid condition `{expr}`"))
}

/// The judge NAMES an expression references — everything that is neither an aggregate, a run-level
/// term, nor the `invariants` subset keyword. Used to compute the run-set / DoD-set (§5.3): resolve
/// each returned name against the judge library. Dotted accessors (`coverage.value`) yield `coverage`.
pub fn judge_names(expr: &str) -> Result<Vec<String>> {
    const RESERVED: &[&str] = &[
        "all_goals", "count_met", "count_regressed", "total", "met_fraction", "any_regressed",
        "tokens_spent", "budget_total", "wall_time", "human_wait_time", "work_time",
        "over_budget", "cost_spent", "cost_limit",
        "over_cost", "iterations", "max_iterations", "over_iterations", "any_judge_error",
        "invariants",
    ];
    let mut out: Vec<String> = Vec::new();
    for t in tokenize(expr)? {
        if let Tok::Ident(name) = t {
            let base = name.split('.').next().unwrap_or(&name);
            if RESERVED.contains(&base) {
                continue;
            }
            if !out.iter().any(|n| n == base) {
                out.push(base.to_string());
            }
        }
    }
    Ok(out)
}

// ---------------- value ----------------


// ---------------- tokens ----------------



// ---------------- parser ----------------



#[cfg(test)]
mod tests;
