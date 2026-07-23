//! Stop-expression evaluator tests.

use super::*;
use crate::core::model::{Judge, JudgeKind, Lifecycle, Verdict};

/// A judge that has never been evaluated (no verdict). The state `validate` sees at startup.
/// `in_dod: true` everywhere — the old `Goal` had no run-set/DoD-set split and every goal
/// counted toward the aggregates, so these judges must range under `count_met`/`met_fraction`.
fn unjudged(id: &str, invariant: bool) -> Judge {
    Judge {
        name: id.into(),
        kind: JudgeKind::Script { path: "true".into() },
        invariant,
        in_dod: true,
        state: Lifecycle::Pending,
        last_verdict: None,
        ever_met: false,
    }
}

fn g(id: &str, met: bool, invariant: bool) -> Judge {
    let mut goal = unjudged(id, invariant);
    goal.apply(Verdict {
        met,
        value: Some(if met { 1.0 } else { 0.0 }),
        max: Some(1.0),
        target: 1.0,
        rationale: String::new(),
        evidence: vec![],
        error: None,
    });
    goal
}

fn regressed(id: &str, invariant: bool) -> Judge {
    let mut goal = g(id, true, invariant);
    // flip to not-met -> regressed
    goal.apply(Verdict { met: false, value: Some(0.0), max: Some(1.0), target: 1.0, rationale: String::new(), evidence: vec![], error: None });
    assert!(goal.regressed());
    goal
}

fn ev(expr: &str, goals: &[Judge]) -> bool {
    evaluate(expr, &StopContext::from_judges(goals)).unwrap()
}

fn ev_run(expr: &str, goals: &[Judge], tokens: u64, budget: Option<u64>, hours: f64) -> bool {
    let ctx = StopContext {
        tokens_spent: tokens,
        budget_total: budget,
        wall_hours: hours,
        ..StopContext::from_judges(goals)
    };
    evaluate(expr, &ctx).unwrap()
}

#[test]
fn nan_comparison_is_an_error_not_a_silent_invert() {
    // wall_hours = NaN: `wall_hours >= 8` must ERROR, not silently return false/true.
    let ctx = StopContext { wall_hours: f64::NAN, ..StopContext::from_judges(&[]) };
    assert!(evaluate("wall_hours >= 8", &ctx).is_err());
    assert!(evaluate("wall_hours != 0", &ctx).is_err()); // the dangerous `!=` case
    // infinity is well-defined for ordering and must NOT error
    let ctx2 = StopContext { tokens_spent: 5, ..StopContext::from_judges(&[]) };
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
        ..StopContext::from_judges(&goals)
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
        ..StopContext::from_judges(&goals)
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
        ..StopContext::from_judges(&goals)
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
fn aggregates_range_over_the_dod_set_not_the_run_set() {
    // THE quantifier split (§5.3): `stalled` is a run-set-only judge (in an `if` condition, so
    // in_dod:false) and is UNMET; `feature` is in the DoD-set and met. If the aggregates ranged
    // over the run-set, `done_if: all_goals` could not fire until `stalled` was met — i.e. the
    // loop would "succeed" only once it got stuck. They must range over the DoD-set, so:
    let feature = g("feature", true, false); // in_dod:true (via the helper)
    let mut stalled = g("stalled", false, false);
    stalled.in_dod = false; // run-set only — an `if stalled then …` condition judge
    let judges = [feature, stalled];

    // all_goals ignores the unmet run-set-only judge → the DoD (just `feature`) is fully met.
    assert!(ev("all_goals", &judges), "all_goals ranges over the DoD-set only");
    // count_met / total / met_fraction likewise ignore it.
    assert!(ev("count_met >= 1", &judges) && !ev("count_met >= 2", &judges));
    assert!(ev("met_fraction >= 1.0", &judges), "1/1 of the DoD-set, not 1/2 of the run-set");
    // but the BARE name still reads the run-set judge's (unmet) met bool — an `if` can see it.
    assert!(!ev("stalled", &judges), "the bare name reads stalled's own met state");
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
    assert!(evaluate("all_goals(invariants)", &StopContext::from_judges(&[])).is_err());
    assert!(evaluate("weighted_fraction(invariants) >= 0.5", &StopContext::from_judges(&[])).is_err());
    // count_met / any_regressed DO support it
    let goals = [g("a", true, true)];
    assert!(evaluate("count_met(invariants) >= 1", &StopContext::from_judges(&goals)).is_ok());
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
    assert!(evaluate("frobnicate", &StopContext::from_judges(&[])).is_err());
    assert!(evaluate("goal_x", &StopContext::from_judges(&[g("goal_y", true, false)])).is_err());
}

// ---------------- dotted accessors ----------------

/// A judge that ran and emitted a number.
fn scored(id: &str, value: f64, max: f64) -> Judge {
    let mut goal = unjudged(id, false);
    goal.apply(Verdict {
        met: value >= max,
        value: Some(value),
        max: Some(max),
        target: max,
        rationale: String::new(),
        evidence: vec![],
        error: None,
    });
    goal
}

/// A judge that FAILED TO RUN — no usable number.
fn errored(id: &str) -> Judge {
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
    assert!(evaluate("over_budget == 0", &StopContext::from_judges(&goals)).unwrap());
}

#[test]
fn the_type_check_survives_not_and_or() {
    // The hole this closes: the tag was dropped by NOT/AND/OR, so `NOT coverage >= 80` — how a
    // user naturally writes "coverage is below 80" — validated clean and was silently always
    // false, while `(coverage) >= 80` was caught. The rule is the same in every shape.
    let goals = [unjudged("coverage", false), unjudged("lint", false)];
    assert!(validate("NOT coverage >= 80", &goals).is_err());
    assert!(validate("NOT (coverage) >= 80", &goals).is_err());
    assert!(validate("(coverage OR lint) >= 80", &goals).is_err());
    assert!(validate("(coverage AND lint) == 1", &goals).is_err());
    assert!(validate("lint AND NOT coverage >= 80", &goals).is_err());
    // ...and the tag must not leak into run-level bools: `over_budget == 1` still parses, and
    // so does a NOT/OR over them.
    assert!(validate("NOT over_budget == 1", &goals).is_ok());
    assert!(validate("(over_budget OR over_cost) == 0", &goals).is_ok());
    // a judge bool still COMPOSES as a bool — it is only comparing it to a number that is out.
    assert!(validate("NOT coverage", &goals).is_ok());
    assert!(validate("coverage AND NOT lint", &goals).is_ok());
    assert!(validate("NOT coverage AND coverage.value >= 80", &goals).is_ok());
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
fn a_measured_zero_is_distinct_from_no_number() {
    // The capability `Option<f64>` buys us: a judge that emitted `value: 0` is a MEASURED zero,
    // so `coverage.value == 0` is TRUE. A judge that emitted NO number has no usable value, so
    // the SAME comparison is FALSE — Missing, not a smuggled 0.0. Before Option, both were 0.0
    // and indistinguishable.
    let measured_zero = [scored("coverage", 0.0, 100.0)];
    assert!(ev("coverage.value == 0", &measured_zero));
    assert!(ev("coverage.value < 1", &measured_zero));
    assert!(!ev("coverage.value >= 1", &measured_zero));

    // a genuine (non-errored) verdict that carries no number at all
    let mut numberless = unjudged("coverage", false);
    numberless.apply(Verdict {
        met: true, value: None, max: None, target: 1.0,
        rationale: String::new(), evidence: vec![], error: None,
    });
    let numberless = [numberless];
    assert!(!ev("coverage.value == 0", &numberless));
    assert!(!ev("coverage.value >= 0", &numberless));
    // the bare name still reads its `met` bool — a numberless verdict is not a broken one.
    assert!(ev("coverage", &numberless));
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
    assert!(!evaluate("any_judge_error", &StopContext::from_judges(&goals)).unwrap());
    // a judge that RAN this step errored
    let errs = vec!["coverage".to_string()];
    let ctx = StopContext { judge_errors: &errs, ..StopContext::from_judges(&goals) };
    assert!(evaluate("any_judge_error", &ctx).unwrap());
    // composes into a real abort guard
    assert!(evaluate("all_goals OR any_judge_error", &ctx).unwrap());
    // NOT stale: the set is per-step, so a step where no judge ran is false even though the
    // goal's own last_verdict may still carry an old error.
    let stale = [errored("coverage")];
    assert!(!evaluate("any_judge_error", &StopContext::from_judges(&stale)).unwrap());
}

// ---------------- tokenizer robustness ----------------

#[test]
fn non_ascii_input_errors_instead_of_panicking() {
    // The tokenizer walks raw bytes and slices on them; a multi-byte char used to slice
    // mid-character and PANIC. Expressions come straight from user YAML.
    for expr in ["cöverage", "all_goals ✓", "goal_ä.value >= 1", "count_met ≥ 3"] {
        assert!(
            evaluate(expr, &StopContext::from_judges(&[])).is_err(),
            "`{expr}` must error, not panic"
        );
    }
}
