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
//!   ident  := goal-id | aggregate | aggregate "(" subset ")"
//!
//! Aggregates: count_met, total, met_fraction, weighted_fraction,
//!             any_regressed, count_regressed. The `(invariants)` subset restricts
//!             an aggregate to invariant goals.

use crate::model::Goal;
use anyhow::{anyhow, bail, Result};

/// The facts the evaluator needs about the current goal set + run.
pub struct StopContext<'a> {
    pub goals: &'a [Goal],
    /// cumulative output tokens spent this run (for budget guards)
    pub tokens_spent: u64,
    /// budget ceiling, if configured (`budget.total`)
    pub budget_total: Option<u64>,
    /// wall-clock hours since the loop started
    pub wall_hours: f64,
}

impl<'a> StopContext<'a> {
    /// Convenience for callers that only have goals (plan/validate paths).
    pub fn from_goals(goals: &'a [Goal]) -> Self {
        StopContext { goals, tokens_spent: 0, budget_total: None, wall_hours: 0.0 }
    }
}

impl<'a> StopContext<'a> {
    fn count_met(&self, invariants_only: bool) -> f64 {
        self.goals
            .iter()
            .filter(|g| !invariants_only || g.invariant)
            .filter(|g| g.met())
            .count() as f64
    }
    fn count_regressed(&self, invariants_only: bool) -> f64 {
        self.goals
            .iter()
            .filter(|g| !invariants_only || g.invariant)
            .filter(|g| g.regressed())
            .count() as f64
    }
    fn total(&self, invariants_only: bool) -> f64 {
        self.goals.iter().filter(|g| !invariants_only || g.invariant).count() as f64
    }
    fn weighted_fraction(&self) -> f64 {
        let total: f64 = self.goals.iter().map(|g| g.weight).sum();
        if total == 0.0 {
            return 0.0;
        }
        let met: f64 = self.goals.iter().filter(|g| g.met()).map(|g| g.weight).sum();
        met / total
    }
    fn goal_met(&self, id: &str) -> Option<bool> {
        self.goals.iter().find(|g| g.id == id).map(|g| g.met())
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

/// Validate an expression at config-load time (parse only, with a dummy context).
pub fn validate(expr: &str, goals: &[Goal]) -> Result<()> {
    let ctx = StopContext::from_goals(goals);
    evaluate(expr, &ctx).map(|_| ())
}

// ---------------- value ----------------

#[derive(Clone, Copy)]
enum Val {
    Bool(bool),
    Num(f64),
}
impl Val {
    fn as_bool(self) -> bool {
        match self {
            Val::Bool(b) => b,
            Val::Num(n) => n != 0.0,
        }
    }
    fn as_num(self) -> f64 {
        match self {
            Val::Bool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            Val::Num(n) => n,
        }
    }
}

// ---------------- tokens ----------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    LParen,
    RParen,
    Or,
    And,
    Not,
    Op(String), // == != >= <= > <
}

fn tokenize(s: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(Tok::RParen);
            i += 1;
        } else if c == '|' && i + 1 < b.len() && b[i + 1] == b'|' {
            out.push(Tok::Or);
            i += 2;
        } else if c == '&' && i + 1 < b.len() && b[i + 1] == b'&' {
            out.push(Tok::And);
            i += 2;
        } else if matches!(c, '=' | '!' | '>' | '<') {
            // two-char ops first
            let two = if i + 1 < b.len() { &s[i..i + 2] } else { "" };
            if matches!(two, "==" | "!=" | ">=" | "<=") {
                out.push(Tok::Op(two.to_string()));
                i += 2;
            } else if matches!(c, '>' | '<') {
                out.push(Tok::Op(c.to_string()));
                i += 1;
            } else {
                bail!("unexpected `{c}` in stop condition");
            }
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let n: f64 = s[start..i].parse().map_err(|_| anyhow!("bad number `{}`", &s[start..i]))?;
            out.push(Tok::Num(n));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &s[start..i];
            match word {
                "OR" | "or" => out.push(Tok::Or),
                "AND" | "and" => out.push(Tok::And),
                "NOT" | "not" => out.push(Tok::Not),
                _ => out.push(Tok::Ident(word.to_string())),
            }
        } else {
            bail!("unexpected character `{c}` in stop condition");
        }
    }
    Ok(out)
}

// ---------------- parser ----------------

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    ctx: &'a StopContext<'a>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn next(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_expr(&mut self) -> Result<Val> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Val> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Val::Bool(left.as_bool() || right.as_bool());
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Val> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.pos += 1;
            let right = self.parse_cmp()?;
            left = Val::Bool(left.as_bool() && right.as_bool());
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Val> {
        let left = self.parse_atom()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.pos += 1;
            let right = self.parse_atom()?;
            let (l, r) = (left.as_num(), right.as_num());
            // A NaN operand makes EVERY comparison meaningless (and `!=` would return
            // true, silently inverting a guard). Fail loud rather than mislead — a
            // judge that emits a NaN value should surface as an error, not flip the loop.
            if l.is_nan() || r.is_nan() {
                bail!("comparison with NaN in stop condition (a judge likely emitted a non-finite value)");
            }
            let res = match op.as_str() {
                "==" => l == r,
                "!=" => l != r,
                ">=" => l >= r,
                "<=" => l <= r,
                ">" => l > r,
                "<" => l < r,
                _ => bail!("unknown operator `{op}`"),
            };
            return Ok(Val::Bool(res));
        }
        Ok(left)
    }

    fn parse_atom(&mut self) -> Result<Val> {
        match self.next().cloned() {
            Some(Tok::Num(n)) => Ok(Val::Num(n)),
            Some(Tok::Not) => Ok(Val::Bool(!self.parse_atom()?.as_bool())),
            Some(Tok::LParen) => {
                let v = self.parse_expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => bail!("expected `)`"),
                }
            }
            Some(Tok::Ident(name)) => self.resolve_ident(&name),
            other => bail!("unexpected token in stop condition: {:?}", other),
        }
    }

    /// Resolve an identifier to a value: a goal id, or an aggregate (optionally
    /// with an `(invariants)` subset).
    fn resolve_ident(&mut self, name: &str) -> Result<Val> {
        // optional subset arg, e.g. any_regressed(invariants)
        let mut invariants_only = false;
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.pos += 1;
            match self.next().cloned() {
                Some(Tok::Ident(s)) if s == "invariants" => invariants_only = true,
                other => bail!("aggregate subset must be `invariants`, got {:?}", other),
            }
            match self.next() {
                Some(Tok::RParen) => {}
                _ => bail!("expected `)` after aggregate subset"),
            }
        }

        // The `(invariants)` subset is only meaningful on count/fraction aggregates.
        // Reject it on anything else rather than silently ignoring it — a silent
        // "accepted but ignored" subset is a real footgun in a user's goals.yaml.
        if invariants_only
            && !matches!(name, "count_met" | "count_regressed" | "total" | "met_fraction" | "any_regressed")
        {
            bail!("`{name}` does not take an (invariants) subset");
        }

        let c = self.ctx;
        let v = match name {
            "all_goals" => Val::Bool(c.total(false) > 0.0 && c.count_met(false) == c.total(false)),
            "count_met" => Val::Num(c.count_met(invariants_only)),
            "count_regressed" => Val::Num(c.count_regressed(invariants_only)),
            "total" => Val::Num(c.total(invariants_only)),
            "met_fraction" => {
                let t = c.total(invariants_only);
                Val::Num(if t == 0.0 { 0.0 } else { c.count_met(invariants_only) / t })
            }
            "weighted_fraction" => Val::Num(c.weighted_fraction()),
            "any_regressed" => Val::Bool(c.count_regressed(invariants_only) > 0.0),
            // run-level guards (budget #5)
            "tokens_spent" => Val::Num(c.tokens_spent as f64),
            "budget_total" => Val::Num(c.budget_total.map(|t| t as f64).unwrap_or(f64::INFINITY)),
            "wall_hours" => Val::Num(c.wall_hours),
            "over_budget" => Val::Bool(match c.budget_total {
                Some(t) => c.tokens_spent > t,
                None => false, // no budget set => never over
            }),
            // otherwise: a goal id -> its met bool
            other => match c.goal_met(other) {
                Some(b) => Val::Bool(b),
                None => bail!("unknown goal or aggregate `{other}` in stop condition"),
            },
        };
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Goal, GoalType, JudgeSpec, Lifecycle, Verdict};

    fn g(id: &str, met: bool, invariant: bool) -> Goal {
        let mut goal = Goal {
            id: id.into(),
            goal_type: GoalType::Binary,
            judge: JudgeSpec::Script { cmd: "true".into(), timeout: 1 },
            target: 1.0,
            weight: 1.0,
            invariant,
            description: String::new(),
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        };
        goal.apply(Verdict {
            met,
            value: if met { 1.0 } else { 0.0 },
            max: 1.0,
            target: 1.0,
            rationale: String::new(),
            evidence: vec![],
            error: None,
        });
        goal
    }

    fn regressed(id: &str, invariant: bool) -> Goal {
        let mut goal = g(id, true, invariant);
        // flip to not-met -> regressed
        goal.apply(Verdict { met: false, value: 0.0, max: 1.0, target: 1.0, rationale: String::new(), evidence: vec![], error: None });
        assert!(goal.regressed());
        goal
    }

    fn ev(expr: &str, goals: &[Goal]) -> bool {
        evaluate(expr, &StopContext::from_goals(goals)).unwrap()
    }

    fn ev_run(expr: &str, goals: &[Goal], tokens: u64, budget: Option<u64>, hours: f64) -> bool {
        let ctx = StopContext { goals, tokens_spent: tokens, budget_total: budget, wall_hours: hours };
        evaluate(expr, &ctx).unwrap()
    }

    #[test]
    fn nan_comparison_is_an_error_not_a_silent_invert() {
        // wall_hours = NaN: `wall_hours >= 8` must ERROR, not silently return false/true.
        let ctx = StopContext { goals: &[], tokens_spent: 0, budget_total: None, wall_hours: f64::NAN };
        assert!(evaluate("wall_hours >= 8", &ctx).is_err());
        assert!(evaluate("wall_hours != 0", &ctx).is_err()); // the dangerous `!=` case
        // infinity is well-defined for ordering and must NOT error
        let ctx2 = StopContext { goals: &[], tokens_spent: 5, budget_total: None, wall_hours: 0.0 };
        assert!(!ev_run_ctx(&ctx2, "tokens_spent > budget_total")); // budget_total = inf when unset
    }

    fn ev_run_ctx(ctx: &StopContext, expr: &str) -> bool {
        evaluate(expr, ctx).unwrap()
    }

    #[test]
    fn budget_and_wall_guards() {
        let goals = [g("a", false, false)];
        // over_budget: true once tokens exceed the ceiling
        assert!(!ev_run("over_budget", &goals, 100, Some(500), 0.0));
        assert!(ev_run("over_budget", &goals, 600, Some(500), 0.0));
        // no budget set => never over
        assert!(!ev_run("over_budget", &goals, 1_000_000, None, 0.0));
        // explicit comparisons
        assert!(ev_run("tokens_spent > 500", &goals, 600, Some(500), 0.0));
        assert!(ev_run("wall_hours >= 8", &goals, 0, None, 8.5));
        // compound halt guard: goals not met but budget blown
        assert!(ev_run("all_goals OR over_budget", &goals, 600, Some(500), 0.0));
    }

    #[test]
    fn all_goals() {
        assert!(ev("all_goals", &[g("a", true, false), g("b", true, false)]));
        assert!(!ev("all_goals", &[g("a", true, false), g("b", false, false)]));
    }

    #[test]
    fn boolean_over_ids() {
        let goals = [g("goal_a", true, false), g("goal_b", false, false)];
        assert!(ev("goal_a OR goal_b", &goals));
        assert!(!ev("goal_a AND goal_b", &goals));
        assert!(ev("goal_a AND NOT goal_b", &goals));
    }

    #[test]
    fn statistical() {
        let goals = [g("a", true, false), g("b", true, false), g("c", true, false), g("d", false, false)];
        assert!(ev("met_fraction >= 0.75", &goals));
        assert!(!ev("met_fraction >= 0.8", &goals));
        assert!(ev("count_met >= 3", &goals));
        assert!(!ev("count_met >= 4", &goals));
    }

    #[test]
    fn invariants_subset_rejected_on_unsupported_aggregates() {
        // all_goals + weighted_fraction must NOT silently accept-and-ignore the subset
        assert!(evaluate("all_goals(invariants)", &StopContext::from_goals(&[])).is_err());
        assert!(evaluate("weighted_fraction(invariants) >= 0.5", &StopContext::from_goals(&[])).is_err());
        // count_met / any_regressed DO support it
        let goals = [g("a", true, true)];
        assert!(evaluate("count_met(invariants) >= 1", &StopContext::from_goals(&goals)).is_ok());
    }

    #[test]
    fn invariant_subset_and_guard() {
        let goals = [g("a", true, false), regressed("inv", true)];
        assert!(ev("any_regressed(invariants)", &goals));
        assert!(!ev("any_regressed", &[g("a", true, false)]));
    }

    #[test]
    fn compound_with_guard() {
        let goals = [g("a", true, false), g("b", true, false), g("c", true, false), regressed("inv", true)];
        // 3/4 met but an invariant regressed
        assert!(ev("met_fraction >= 0.7 OR any_regressed(invariants)", &goals));
    }

    #[test]
    fn unknown_identifier_is_error() {
        assert!(evaluate("frobnicate", &StopContext::from_goals(&[])).is_err());
        assert!(evaluate("goal_x", &StopContext::from_goals(&[g("goal_y", true, false)])).is_err());
    }
}
