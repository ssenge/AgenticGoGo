//! Stop-condition parser + value type: consumes tokens and evaluates against the run context.

use super::lex::Tok;
use super::StopContext;
use anyhow::{bail, Result};

// ---------------- value ----------------

#[derive(Clone, Copy)]
pub(super) enum Val {
    Bool(bool),
    /// A JUDGE's `met` bool. Distinct from `Bool` for exactly one reason: it is a TYPE ERROR to
    /// compare it against a number (`coverage >= 80`), because the bool→num coercion below makes
    /// that `1.0 >= 80.0` — silently always false. The coercion itself stays: run-level bools
    /// (`over_budget == 1`) rely on it.
    GoalBool(bool),
    Num(f64),
    /// A judge with no usable number: `.value`/`.max` on a judge that hasn't run yet, errored, or
    /// emitted no number (`value: None`).
    /// Any comparison against it is **false** — see `parse_cmp`. Deliberately NOT NaN: a NaN
    /// operand hard-bails, and that `Err` is swallowed into `false` by `engine::eval_or_log`, so
    /// NaN here would mean a stop condition that can never be true and never says why.
    Missing,
}
impl Val {
    pub(super) fn as_bool(self) -> bool {
        match self {
            Val::Bool(b) | Val::GoalBool(b) => b,
            Val::Num(n) => n != 0.0,
            Val::Missing => false,
        }
    }
    pub(super) fn as_num(self) -> f64 {
        match self {
            Val::Bool(b) | Val::GoalBool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            Val::Num(n) => n,
            Val::Missing => 0.0, // unreachable: parse_cmp short-circuits Missing to false first
        }
    }
    pub(super) fn is_goal_bool(self) -> bool {
        matches!(self, Val::GoalBool(_))
    }
    /// A boolean result that KEEPS the `GoalBool` tag if an operand carried it. NOT/AND/OR over a
    /// judge's `met` bool is still a judge bool, so the type check in `parse_cmp` must still fire:
    /// otherwise `NOT coverage >= 80` — the most natural way to write "coverage is below 80" —
    /// slips past `validate` and is silently always false, while `(coverage) >= 80` is caught.
    /// Same rule, every shape.
    pub(super) fn boolean(b: bool, from_goal: bool) -> Val {
        if from_goal {
            Val::GoalBool(b)
        } else {
            Val::Bool(b)
        }
    }
}

// ---------------- parser ----------------

pub(super) struct Parser<'a> {
    pub(super) tokens: &'a [Tok],
    pub(super) pos: usize,
    pub(super) ctx: &'a StopContext<'a>,
}

impl<'a> Parser<'a> {
    pub(super) fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    pub(super) fn next(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    pub(super) fn parse_expr(&mut self) -> Result<Val> {
        self.parse_or()
    }

    pub(super) fn parse_or(&mut self) -> Result<Val> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Val::boolean(left.as_bool() || right.as_bool(), left.is_goal_bool() || right.is_goal_bool());
        }
        Ok(left)
    }

    pub(super) fn parse_and(&mut self) -> Result<Val> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.pos += 1;
            let right = self.parse_cmp()?;
            left = Val::boolean(left.as_bool() && right.as_bool(), left.is_goal_bool() || right.is_goal_bool());
        }
        Ok(left)
    }

    pub(super) fn parse_cmp(&mut self) -> Result<Val> {
        let left = self.parse_atom()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.pos += 1;
            let right = self.parse_atom()?;
            // THE TYPE CHECK. A bare judge name is a `met` BOOL, and a bool has no ordering against
            // a number: `coverage >= 80` coerces met→1.0 and is silently ALWAYS FALSE. `validate`
            // runs this at config load, so it is a startup error, not a run that never finishes.
            if matches!(left, Val::GoalBool(_)) || matches!(right, Val::GoalBool(_)) {
                bail!(
                    "a judge's name is its `met` BOOL — comparing it to a number is always false. \
                     Use the accessor: `judge.value >= 80`, not `judge >= 80`"
                );
            }
            // A judge with no usable number (never ran, or errored) makes the comparison FALSE.
            // `any_judge_error` is what surfaces the error case — this must not error, because an
            // Err is swallowed into `false` upstream and would look identical while hiding why.
            if matches!(left, Val::Missing) || matches!(right, Val::Missing) {
                return Ok(Val::Bool(false));
            }
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

    pub(super) fn parse_atom(&mut self) -> Result<Val> {
        match self.next().cloned() {
            Some(Tok::Num(n)) => Ok(Val::Num(n)),
            // NOT preserves the GoalBool tag: `NOT coverage` is still a judge's bool, so
            // `NOT coverage >= 80` is still the always-false compare parse_cmp rejects.
            Some(Tok::Not) => {
                let v = self.parse_atom()?;
                Ok(Val::boolean(!v.as_bool(), v.is_goal_bool()))
            }
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

    /// Resolve an identifier to a value: a goal id, a dotted accessor on one (`coverage.value`),
    /// or an aggregate (optionally with an `(invariants)` subset).
    pub(super) fn resolve_ident(&mut self, name: &str) -> Result<Val> {
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
        // "accepted but ignored" subset is a real footgun in a user's agg.yaml.
        if invariants_only
            && !matches!(name, "count_met" | "count_regressed" | "total" | "met_fraction" | "any_regressed")
        {
            bail!("`{name}` does not take an (invariants) subset");
        }

        // Dotted accessor: the ONLY way to read the number a judge emitted. (The bare name is its
        // `met` bool — see `Val::GoalBool`.)
        if let Some((base, field)) = name.split_once('.') {
            return self.resolve_accessor(base, field);
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
            "any_regressed" => Val::Bool(c.count_regressed(invariants_only) > 0.0),
            // ── run-level ceiling guards ──────────────────────────────────────────────
            // Each ceiling has ONE user-facing predicate: over_budget (tokens),
            // over_cost (dollars), over_iterations (sessions). Same `over_<noun>` shape;
            // that trio is all a user needs to memorize. The raw counters below back them
            // and stay available for custom comparisons (e.g. `cost_spent > 3.5`).
            "tokens_spent" => Val::Num(c.tokens_spent as f64),
            "budget_total" => Val::Num(c.budget_total.map(|t| t as f64).unwrap_or(f64::INFINITY)),
            "wall_hours" => Val::Num(c.wall_hours),
            "over_budget" => Val::Bool(match c.budget_total {
                Some(t) => c.tokens_spent > t,
                None => false, // no budget set => never over
            }),
            // dollar-cost ceiling (#2). Claude prices each session; we sum total_cost_usd.
            "cost_spent" => Val::Num(c.cost_spent),
            "cost_limit" => Val::Num(c.cost_limit.unwrap_or(f64::INFINITY)),
            "over_cost" => Val::Bool(match c.cost_limit {
                Some(t) => c.cost_spent > t,
                None => false, // no cost cap set => never over
            }),
            // iteration ceiling: trips once sessions reach the max_sessions cap. `>=`
            // (not `>`) mirrors the loop's own `session >= max_sessions` stop check, so
            // `halt_when: ... OR over_iterations` and the hard cap fire on the same session.
            "iterations" => Val::Num(c.sessions_done as f64),
            "max_iterations" => Val::Num(c.max_sessions.map(|t| t as f64).unwrap_or(f64::INFINITY)),
            "over_iterations" => Val::Bool(match c.max_sessions {
                Some(t) => t > 0 && c.sessions_done >= t,
                None => false, // no cap (0/unset) => never over
            }),
            // "any judge that RAN this step returned error". No judge ran => false: a skipped
            // step reports no errors because there were none to have, not because it forgot.
            "any_judge_error" => Val::Bool(!c.judge_errors.is_empty()),
            // otherwise: a judge name -> its met bool. GoalBool, not Bool: comparing it to a number
            // is a type error (`coverage >= 80`), while `over_budget == 1` stays legal.
            other => match c.judge_met(other) {
                Some(b) => Val::GoalBool(b),
                None => bail!("unknown judge or aggregate `{other}` in condition"),
            },
        };
        Ok(v)
    }

    /// `judge.value` / `judge.max` — the NUMBER the judge emitted, not its `met` bool.
    pub(super) fn resolve_accessor(&self, base: &str, field: &str) -> Result<Val> {
        let goal = match self.ctx.judge(base) {
            Some(g) => g,
            None => bail!("unknown judge `{base}` in condition (in `{base}.{field}`)"),
        };
        // The FIELD NAME is checked before the verdict is read, and that order is load-bearing: at
        // `validate` time no judge has run, so a bad accessor would otherwise short-circuit to
        // `Missing` and sail through startup.
        match field {
            "value" | "max" => {}
            // `.target` is NOT an accessor, deliberately: a judge's target is presentational (it
            // draws the progress bar). A threshold has ONE owner — the condition. Two would
            // silently disagree, and the loop would obey the one you weren't reading.
            "target" => bail!(
                "`{base}.target` is not readable — a judge's `target` is presentational only. \
                 Put the threshold in the condition itself: `{base}.value >= <n>`"
            ),
            other => bail!("unknown accessor `{base}.{other}` — a judge exposes `.value` and `.max`"),
        }

        // No verdict yet, the judge ERRORED, or it emitted no number => no usable number. `Missing`
        // makes the comparison false; `any_judge_error` is what tells you the judge blew up. A real
        // `Some(0.0)` is a MEASURED zero and stays a `Num`, so `coverage.value == 0` reads true and
        // is finally distinct from an absent number — the ceiling the old ponytail note flagged is
        // lifted now that `Verdict.value`/`max` are `Option<f64>` (§5.2).
        match goal.last_verdict.as_ref() {
            Some(v) if v.error.is_none() => match if field == "value" { v.value } else { v.max } {
                Some(n) => Ok(Val::Num(n)),
                None => Ok(Val::Missing),
            },
            _ => Ok(Val::Missing),
        }
    }
}
