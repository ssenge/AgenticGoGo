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
//! Aggregates: count_met, total, met_fraction, weighted_fraction,
//!             any_regressed, count_regressed. The `(invariants)` subset restricts
//!             an aggregate to invariant goals.
//!
//! A bare goal id is its **`met` bool**. To compare the NUMBER a judge emitted, use the dotted
//! accessor — `coverage.value >= 80`, never `coverage >= 80` (the latter is a hard error: see
//! [`Val::GoalBool`]).

use crate::core::model::Goal;
use anyhow::{anyhow, bail, Context, Result};

/// The facts the evaluator needs about the current goal set + run.
pub struct StopContext<'a> {
    pub goals: &'a [Goal],
    /// ids of the judges that RAN this step and returned an `error` (backs `any_judge_error`).
    /// Empty when no judge ran — a step whose judges were skipped is not "stale true", it is
    /// honestly false: no judge ran, so no judge errored.
    pub judge_errors: &'a [String],
    /// cumulative output tokens spent this run (for budget guards)
    pub tokens_spent: u64,
    /// budget ceiling, if configured (`budget.total`)
    pub budget_total: Option<u64>,
    /// cumulative dollars spent this run (backs `over_cost`)
    pub cost_spent: f64,
    /// dollar ceiling, if configured (`cost.total`)
    pub cost_limit: Option<f64>,
    /// sessions completed so far this run (backs `over_iterations`)
    pub sessions_done: u32,
    /// the `max_sessions` cap, if any (backs `over_iterations`)
    pub max_sessions: Option<u32>,
    /// wall-clock hours since the loop started
    pub wall_hours: f64,
}

impl<'a> StopContext<'a> {
    /// Convenience for callers that only have goals (plan/validate paths).
    pub fn from_goals(goals: &'a [Goal]) -> Self {
        StopContext {
            goals,
            judge_errors: &[],
            tokens_spent: 0,
            budget_total: None,
            cost_spent: 0.0,
            cost_limit: None,
            sessions_done: 0,
            max_sessions: None,
            wall_hours: 0.0,
        }
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
    fn goal(&self, id: &str) -> Option<&Goal> {
        self.goals.iter().find(|g| g.id == id)
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
pub fn validate(expr: &str, goals: &[Goal]) -> Result<()> {
    let ctx = StopContext::from_goals(goals);
    evaluate(expr, &ctx).map(|_| ()).with_context(|| format!("invalid condition `{expr}`"))
}

// ---------------- value ----------------

#[derive(Clone, Copy)]
enum Val {
    Bool(bool),
    /// A JUDGE's `met` bool. Distinct from `Bool` for exactly one reason: it is a TYPE ERROR to
    /// compare it against a number (`coverage >= 80`), because the bool→num coercion below makes
    /// that `1.0 >= 80.0` — silently always false. The coercion itself stays: run-level bools
    /// (`over_budget == 1`) rely on it.
    GoalBool(bool),
    Num(f64),
    /// A judge with no usable number: `.value`/`.max` on a judge that hasn't run yet or errored.
    /// Any comparison against it is **false** — see `parse_cmp`. Deliberately NOT NaN: a NaN
    /// operand hard-bails, and that `Err` is swallowed into `false` by `engine::eval_or_log`, so
    /// NaN here would mean a stop condition that can never be true and never says why.
    Missing,
}
impl Val {
    fn as_bool(self) -> bool {
        match self {
            Val::Bool(b) | Val::GoalBool(b) => b,
            Val::Num(n) => n != 0.0,
            Val::Missing => false,
        }
    }
    fn as_num(self) -> f64 {
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
        // This loop walks RAW BYTES and slices the &str on those byte offsets. A multi-byte char
        // (one umlaut in a user's YAML) would make `b[i] as char` a lone continuation byte, and the
        // slice below would land mid-character and PANIC. The grammar is ASCII-only — say so, and
        // error instead. `s[i..]` is safe here: every byte consumed so far was ASCII, so `i` is on
        // a char boundary.
        if !b[i].is_ascii() {
            let ch = s[i..].chars().next().unwrap_or('?');
            bail!("unexpected character `{ch}` in stop condition (the grammar is ASCII-only)");
        }
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
            // two-char ops first. The `is_ascii` guard is not cosmetic: `>ö` would slice s[i..i+2]
            // straight through the middle of `ö` and panic. A non-ASCII second byte falls through
            // to the single-char op (or the error), and the loop head above then rejects it.
            let two = if i + 1 < b.len() && b[i + 1].is_ascii() { &s[i..i + 2] } else { "" };
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
            // A LEADING `.` still starts a number, so the leading-dot float `.5` keeps working.
            // That is what makes the accessor below safe to fold into the identifier charset:
            // `.` after an identifier char continues the identifier, `.` anywhere else is a number.
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let n: f64 = s[start..i].parse().map_err(|_| anyhow!("bad number `{}`", &s[start..i]))?;
            out.push(Tok::Num(n));
        } else if c.is_ascii_alphabetic() || c == '_' {
            // `.` is an identifier CONTINUATION char (never a start char), so `coverage.value`
            // lexes as one Ident and `met_fraction >= 0.75` still lexes as Ident, Op, Num. This is
            // why there is no `Dot` token: a Dot token would have to steal `.` from the number
            // branch and would silently break `.5`.
            //
            // `is_ascii_alphanumeric`, NOT `is_alphanumeric`: the latter is true for the lead byte
            // of `ö` (0xC3 as char == 'Ã'), so it would swallow half a char and then slice on the
            // boundary — the panic this guards. Stopping here hands the byte back to the loop head,
            // which errors.
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
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

    /// Resolve an identifier to a value: a goal id, a dotted accessor on one (`coverage.value`),
    /// or an aggregate (optionally with an `(invariants)` subset).
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
            "weighted_fraction" => Val::Num(c.weighted_fraction()),
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
            // otherwise: a goal id -> its met bool. GoalBool, not Bool: comparing it to a number
            // is a type error (`coverage >= 80`), while `over_budget == 1` stays legal.
            other => match c.goal_met(other) {
                Some(b) => Val::GoalBool(b),
                None => bail!("unknown goal or aggregate `{other}` in stop condition"),
            },
        };
        Ok(v)
    }

    /// `judge.value` / `judge.max` — the NUMBER the judge emitted, not its `met` bool.
    fn resolve_accessor(&self, base: &str, field: &str) -> Result<Val> {
        let goal = match self.ctx.goal(base) {
            Some(g) => g,
            None => bail!("unknown judge `{base}` in stop condition (in `{base}.{field}`)"),
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

        // No verdict yet, or the judge ERRORED => no usable number. `Missing` makes the comparison
        // false; `any_judge_error` is what tells you the judge blew up.
        //
        // ponytail: "the judge emitted no `value`" is NOT distinguishable here — `Verdict.value` is
        // a `#[serde(default)]` f64, so an absent field and a real `0` are the same 0.0. Treating
        // that as Missing would break honest `judge.value == 0` checks. Ceiling: it needs
        // `Option<f64>` on the wire (verdicts.jsonl + state.json), a bigger cut than this commit.
        match goal.last_verdict.as_ref() {
            Some(v) if v.error.is_none() => {
                Ok(Val::Num(if field == "value" { v.value } else { v.max }))
            }
            _ => Ok(Val::Missing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Goal, GoalType, JudgeSpec, Lifecycle, Verdict};

    /// A goal that has never been judged (no verdict). The state `validate` sees at startup.
    fn unjudged(id: &str, invariant: bool) -> Goal {
        Goal {
            id: id.into(),
            goal_type: GoalType::Binary,
            judge: JudgeSpec::Script { cmd: "true".into(), timeout: 1 },
            target: 1.0,
            weight: 1.0,
            invariant,
            description: String::new(),
            recheck: crate::core::model::RecheckPolicy::Always,
            recheck_inputs: vec![],
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
            latched: false,
            recheck_sig: None,
        }
    }

    fn g(id: &str, met: bool, invariant: bool) -> Goal {
        let mut goal = unjudged(id, invariant);
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
        let ctx = StopContext {
            tokens_spent: tokens,
            budget_total: budget,
            wall_hours: hours,
            ..StopContext::from_goals(goals)
        };
        evaluate(expr, &ctx).unwrap()
    }

    #[test]
    fn nan_comparison_is_an_error_not_a_silent_invert() {
        // wall_hours = NaN: `wall_hours >= 8` must ERROR, not silently return false/true.
        let ctx = StopContext { wall_hours: f64::NAN, ..StopContext::from_goals(&[]) };
        assert!(evaluate("wall_hours >= 8", &ctx).is_err());
        assert!(evaluate("wall_hours != 0", &ctx).is_err()); // the dangerous `!=` case
        // infinity is well-defined for ordering and must NOT error
        let ctx2 = StopContext { tokens_spent: 5, ..StopContext::from_goals(&[]) };
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
    fn over_cost_guard() {
        let goals = [g("a", false, false)];
        let cost = |spent: f64, limit: Option<f64>| StopContext {
            cost_spent: spent,
            cost_limit: limit,
            ..StopContext::from_goals(&goals)
        };
        // under the dollar cap → not over
        assert!(!evaluate("over_cost", &cost(3.50, Some(5.0))).unwrap());
        // past the cap → over
        assert!(evaluate("over_cost", &cost(5.01, Some(5.0))).unwrap());
        // no cap set → never over (even at a huge spend)
        assert!(!evaluate("over_cost", &cost(1000.0, None)).unwrap());
        // raw counter is usable in a custom comparison
        assert!(evaluate("cost_spent > 4.99", &cost(5.0, None)).unwrap());
        // compound halt guard: goals not met but money blown
        assert!(evaluate("all_goals OR over_cost", &cost(9.0, Some(5.0))).unwrap());
    }

    #[test]
    fn over_iterations_guard() {
        let goals = [g("a", false, false)];
        let iter = |done: u32, max: Option<u32>| StopContext {
            sessions_done: done,
            max_sessions: max,
            ..StopContext::from_goals(&goals)
        };
        // below the cap → not over
        assert!(!evaluate("over_iterations", &iter(3, Some(5))).unwrap());
        // AT the cap → over (>=, matches the loop's own session>=max check)
        assert!(evaluate("over_iterations", &iter(5, Some(5))).unwrap());
        assert!(evaluate("over_iterations", &iter(6, Some(5))).unwrap());
        // no cap (None) → never over
        assert!(!evaluate("over_iterations", &iter(9999, None)).unwrap());
        // a 0 cap means "unlimited" (matches max_sessions==0 convention) → never over
        assert!(!evaluate("over_iterations", &iter(10, Some(0))).unwrap());
        // raw counter usable directly
        assert!(evaluate("iterations >= 5", &iter(5, None)).unwrap());
    }

    #[test]
    fn all_three_ceilings_compose_in_one_halt() {
        // the canonical user expression: stop on success OR any ceiling.
        let goals = [g("a", false, false)];
        let expr = "all_goals OR over_budget OR over_cost OR over_iterations";
        // a fresh "nothing tripped" context; callers override the one field they're testing.
        let base = |cost_spent: f64, sessions_done: u32| StopContext {
            tokens_spent: 10,
            budget_total: Some(1000),
            cost_spent,
            cost_limit: Some(5.0),
            sessions_done,
            max_sessions: Some(20),
            ..StopContext::from_goals(&goals)
        };
        // nothing tripped yet → keep going
        assert!(!evaluate(expr, &base(1.0, 1)).unwrap());
        // only the dollar cap blown → halt
        assert!(evaluate(expr, &base(6.0, 1)).unwrap());
        // only the iteration cap reached → halt
        assert!(evaluate(expr, &base(1.0, 20)).unwrap());
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

    // ---------------- dotted accessors ----------------

    /// A judge that ran and emitted a number.
    fn scored(id: &str, value: f64, max: f64) -> Goal {
        let mut goal = unjudged(id, false);
        goal.apply(Verdict {
            met: value >= max,
            value,
            max,
            target: max,
            rationale: String::new(),
            evidence: vec![],
            error: None,
        });
        goal
    }

    /// A judge that FAILED TO RUN — no usable number.
    fn errored(id: &str) -> Goal {
        let mut goal = unjudged(id, false);
        goal.apply(Verdict::failed("judge exploded"));
        goal
    }

    #[test]
    fn dotted_accessor_reads_the_judges_number() {
        // THE BUG THIS FIXES: `coverage` is a bool, so `coverage >= 80` was `1.0 >= 80.0` — always
        // false. `coverage.value` is the number the judge actually emitted.
        let goals = [scored("coverage", 87.0, 100.0)];
        assert!(ev("coverage.value >= 80", &goals));
        assert!(!ev("coverage.value >= 90", &goals));
        assert!(ev("coverage.max == 100", &goals));
        // the bare name still means `met` (87 < 100 → not met), and still composes as a bool
        assert!(!ev("coverage", &goals));
        assert!(ev("NOT coverage", &goals));
        // a met judge: bare name true, and the number is still readable
        let done = [scored("coverage", 100.0, 100.0)];
        assert!(ev("coverage AND coverage.value >= 80", &done));
    }

    #[test]
    fn bare_judge_name_compared_to_a_number_is_a_startup_error() {
        // The headline fix: this used to parse, evaluate `1.0 >= 80.0`, and be silently always false.
        let goals = [unjudged("coverage", false)];
        assert!(validate("coverage >= 80", &goals).is_err());
        assert!(validate("coverage == 1", &goals).is_err());
        assert!(validate("all_goals OR coverage > 0.5", &goals).is_err());
        // the accessor is the way to say it, and it validates clean
        assert!(validate("coverage.value >= 80", &goals).is_ok());
        // ...but the bool→num coercion is STILL load-bearing for run-level terms. Do not regress it.
        assert!(validate("over_budget == 1", &goals).is_ok());
        assert!(evaluate("over_budget == 0", &StopContext::from_goals(&goals)).unwrap());
    }

    #[test]
    fn target_accessor_is_rejected_at_startup() {
        // a judge's `target` is presentational; the threshold belongs in the condition.
        let goals = [scored("coverage", 87.0, 100.0)];
        assert!(validate("coverage.target >= 80", &goals).is_err());
        assert!(validate("coverage.value >= coverage.target", &goals).is_err());
        // any other accessor is a startup error too — note this must fire with NO verdict present,
        // which is exactly the state `validate` runs in.
        assert!(validate("coverage.rationale == 1", &[unjudged("coverage", false)]).is_err());
        assert!(validate("coverage.value", &[unjudged("coverage", false)]).is_ok());
        // unknown judge behind an accessor
        assert!(validate("nope.value >= 1", &goals).is_err());
    }

    #[test]
    fn accessor_with_no_usable_number_makes_the_comparison_false() {
        // A judge that errored has no number. The comparison is FALSE — never an Err (an Err is
        // swallowed into `false` by engine::eval_or_log anyway, but silently) and never NaN (which
        // hard-bails). Crucially `!=` must ALSO be false, or a guard silently inverts.
        let goals = [errored("coverage")];
        assert!(!ev("coverage.value >= 80", &goals));
        assert!(!ev("coverage.value < 80", &goals));
        assert!(!ev("coverage.value != 80", &goals));
        assert!(!ev("coverage.value == 0", &goals));
        // same for a judge that has not run yet (this is the startup state)
        let fresh = [unjudged("coverage", false)];
        assert!(!ev("coverage.value >= 80", &fresh));
        assert!(!ev("coverage.value != 80", &fresh));
    }

    #[test]
    fn float_literals_survive_the_dotted_lexer() {
        let goals = [g("a", true, false), g("b", true, false), g("c", true, false), g("d", false, false)];
        // the regression the accessor could have broken: a normal float literal
        assert!(ev("met_fraction >= 0.75", &goals));
        assert!(!ev("met_fraction >= 0.8", &goals));
        // ...and the LEADING-DOT float, which the lexer accepts today. Pinned: `.` still STARTS a
        // number. It only continues an identifier, which is what keeps `coverage.value` lexable.
        assert!(ev("met_fraction >= .75", &goals));
        assert!(!ev("met_fraction >= .8", &goals));
        assert!(ev("coverage.value >= .5", &[scored("coverage", 0.9, 1.0)]));
    }

    // ---------------- any_judge_error ----------------

    #[test]
    fn any_judge_error_is_true_only_when_a_judge_that_ran_errored() {
        let goals = [g("a", true, false)];
        // no judge errored this step
        assert!(!evaluate("any_judge_error", &StopContext::from_goals(&goals)).unwrap());
        // a judge that RAN this step errored
        let errs = vec!["coverage".to_string()];
        let ctx = StopContext { judge_errors: &errs, ..StopContext::from_goals(&goals) };
        assert!(evaluate("any_judge_error", &ctx).unwrap());
        // composes into a real abort guard
        assert!(evaluate("all_goals OR any_judge_error", &ctx).unwrap());
        // NOT stale: the set is per-step, so a step where no judge ran is false even though the
        // goal's own last_verdict may still carry an old error.
        let stale = [errored("coverage")];
        assert!(!evaluate("any_judge_error", &StopContext::from_goals(&stale)).unwrap());
    }

    // ---------------- tokenizer robustness ----------------

    #[test]
    fn non_ascii_input_errors_instead_of_panicking() {
        // The tokenizer walks raw bytes and slices on them; a multi-byte char used to slice
        // mid-character and PANIC. Expressions come straight from user YAML.
        for expr in ["cöverage", "all_goals ✓", "goal_ä.value >= 1", "count_met ≥ 3"] {
            assert!(
                evaluate(expr, &StopContext::from_goals(&[])).is_err(),
                "`{expr}` must error, not panic"
            );
        }
    }
}
