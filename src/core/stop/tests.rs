//! Stop-expression evaluator tests.

use super::*;
use crate::core::groups::Groups;
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
        timeout: None,
        ruler: None,
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
        mixed: false, vacuous: false, blocked_by: None,
    });
    goal
}

fn regressed(id: &str, invariant: bool) -> Judge {
    let mut goal = g(id, true, invariant);
    // flip to not-met -> regressed
    goal.apply(Verdict { met: false, value: Some(0.0), max: Some(1.0), target: 1.0, rationale: String::new(), evidence: vec![], error: None, mixed: false, vacuous: false, blocked_by: None });
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
        wall_secs: hours * 3600.0,
        human_wait_secs: 0.0,
        ..StopContext::from_judges(goals)
    };
    evaluate(expr, &ctx).unwrap()
}

/// The three clock terms, and the reason all three exist: the grammar has comparisons but NO
/// arithmetic, so `wall_time - human_wait_time >= N` cannot be written and `work_time` must be its
/// own term. A run blocked overnight on a human has to be able to survive an EFFORT ceiling while
/// still failing a DEADLINE.
#[test]
fn work_time_excludes_human_wait_so_a_slow_human_does_not_blow_an_effort_ceiling() {
    let goals = [g("a", false, false)];
    let ctx = |wall: f64, waited: f64| StopContext {
        wall_secs: wall,
        human_wait_secs: waited,
        ..StopContext::from_judges(&goals)
    };

    // 10h e2e, 9h of it waiting for a person to wake up.
    let overnight = ctx(10.0 * 3600.0, 9.0 * 3600.0);
    assert!(evaluate("wall_time >= 28800", &overnight).unwrap(), "a DEADLINE counts the waiting");
    assert!(
        !evaluate("work_time >= 28800", &overnight).unwrap(),
        "an EFFORT ceiling must NOT be consumed by a human who was asleep — this is the whole \
         reason an indefinite hil_* block is survivable"
    );
    assert!(evaluate("human_wait_time >= 32400", &overnight).unwrap());

    // Same wall time, no waiting: now the effort ceiling is genuinely blown.
    assert!(evaluate("work_time >= 28800", &ctx(10.0 * 3600.0, 0.0)).unwrap());

    // work_time can never go negative, however the clock behaves.
    assert!(!evaluate("work_time > 0", &ctx(100.0, 9_999.0)).unwrap());
}

/// A-18.5: waiting out an agent's rate limit is not work either. A subscription window that closes
/// for four hours must not eat four hours of an effort ceiling — while a DEADLINE still counts it.
#[test]
fn work_time_excludes_rate_limit_waiting_too() {
    let goals = [g("a", false, false)];
    let ctx = StopContext {
        wall_secs: 5.0 * 3600.0,
        human_wait_secs: 3600.0,
        rate_limit_wait_secs: 4.0 * 3600.0,
        ..StopContext::from_judges(&goals)
    };
    assert!(evaluate("wall_time >= 18000", &ctx).unwrap(), "a deadline counts every second");
    assert!(!evaluate("work_time > 0", &ctx).unwrap(), "neither wait is effort");
    assert!(evaluate("human_wait_time >= 3600", &ctx).unwrap(), "human_wait_time stays the human's alone");
    assert!(!evaluate("human_wait_time > 3600", &ctx).unwrap());
}

/// `wall_hours` is REMOVED, not aliased. It must fail the parser as an unknown judge name rather
/// than silently resolving to a number in the wrong unit — a 3600x mistake is worse than an error.
#[test]
fn the_removed_wall_hours_term_is_not_a_silent_alias() {
    let goals = [g("a", false, false)];
    let ctx = StopContext::from_judges(&goals);
    // It is now an ordinary identifier, so it resolves as a JUDGE name — and there is no such judge.
    assert!(
        evaluate("wall_hours >= 8", &ctx).is_err(),
        "`wall_hours` must not evaluate to anything; the unit changed and a silent read is a trap"
    );
    assert!(
        crate::core::stop::judge_names("wall_hours >= 8").unwrap().contains(&"wall_hours".to_string()),
        "it is reported as an unknown JUDGE name, which is what makes agg hard-error at startup"
    );
}

#[test]
fn nan_comparison_is_an_error_not_a_silent_invert() {
    // wall_hours = NaN: `wall_hours >= 8` must ERROR, not silently return false/true.
    let ctx = StopContext { wall_secs: f64::NAN, ..StopContext::from_judges(&[]) };
    assert!(evaluate("wall_time >= 28800", &ctx).is_err());
    assert!(evaluate("wall_time != 0", &ctx).is_err()); // the dangerous `!=` case
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
    assert!(ev_run("wall_time >= 28800", &goals, 0, None, 8.5));   // SECONDS now: 8h = 28800
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
    // THE quantifier split (§5.3): `stalled` is a run-set-only judge (named in an `until:`/
    // `notify_if` condition, so in_dod:false) and is UNMET; `feature` is in the DoD-set and met.
    // If the aggregates ranged
    // over the run-set, `done_if: all_goals` could not fire until `stalled` was met — i.e. the
    // loop would "succeed" only once it got stuck. They must range over the DoD-set, so:
    let feature = g("feature", true, false); // in_dod:true (via the helper)
    let mut stalled = g("stalled", false, false);
    stalled.in_dod = false; // run-set only — a condition judge, never a goal
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
    // weighted_fraction / a bare judge must NOT silently accept-and-ignore the subset
    assert!(evaluate("weighted_fraction(invariants) >= 0.5", &StopContext::from_judges(&[])).is_err());
    let goals = [g("a", true, true), g("b", false, false)];
    let err = format!("{:#}", evaluate("a(invariants)", &StopContext::from_judges(&goals)).unwrap_err());
    assert!(err.contains("does not take a subset"), "{err}");
    // count_met / any_regressed / all_goals DO support it — all_goals(invariants) is "every
    // invariant met", whatever the rest of the DoD-set says.
    assert!(evaluate("count_met(invariants) >= 1", &StopContext::from_judges(&goals)).is_ok());
    assert!(ev("all_goals(invariants) AND NOT all_goals", &goals));
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
        mixed: false, vacuous: false, blocked_by: None,
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
        mixed: false, vacuous: false, blocked_by: None,
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

// ---------------- the gate's own terms (`core::ratchet`) ----------------

/// A gate with `cc` ratcheted (down, reference 10) and the given history.
fn gate_with(rollbacks: u32, consecutive: u32, signatures: &[&[&str]], regressed: bool) -> crate::core::ratchet::Gate {
    use crate::core::ratchet::{Gate, Ratchet, RatchetRegression};
    let mut gate = Gate::new([("cc".to_string(), Ratchet::down(0.5).baseline(10.0))].into(), 4);
    gate.state.rollbacks = rollbacks;
    gate.state.consecutive_rollbacks = consecutive;
    gate.state.signatures = signatures.iter().map(|s| s.iter().map(|n| n.to_string()).collect()).collect();
    if regressed {
        gate.state.last_ratchet = vec![RatchetRegression { judge: "cc".into(), from: Some(10.0), to: Some(12.0) }];
    }
    gate
}

fn ev_gate(expr: &str, goals: &[Judge], gate: &crate::core::ratchet::Gate) -> bool {
    evaluate(expr, &StopContext { gate: Some(gate), ..StopContext::from_judges(goals) }).unwrap()
}

/// `rollbacks` / `consecutive_rollbacks` are NUMBERS and `oscillating` / `ratchet_regressed` are
/// BOOLS, read from the gate — the same shapes `iterations` and `over_iterations` have.
#[test]
fn the_gate_terms_read_the_gate_state() {
    let goals = [g("a", false, false), g("cc", true, false)];
    let quiet = gate_with(0, 0, &[&["a"], &["a"]], false);
    assert!(ev_gate("rollbacks == 0 AND consecutive_rollbacks == 0", &goals, &quiet));
    assert!(!ev_gate("oscillating OR ratchet_regressed", &goals, &quiet));

    let busy = gate_with(3, 2, &[&["a"], &["b"], &["a"]], true);
    assert!(ev_gate("rollbacks >= 2", &goals, &busy));
    assert!(ev_gate("consecutive_rollbacks >= 2 AND NOT (consecutive_rollbacks >= 3)", &goals, &busy));
    assert!(ev_gate("oscillating", &goals, &busy));
    assert!(ev_gate("ratchet_regressed", &goals, &busy));

    // with no gate behind the context: a run that has gated nothing.
    assert!(ev("rollbacks == 0 AND NOT oscillating AND NOT ratchet_regressed", &goals));
}

/// `<judge>.ref` is the ratchet reference — a number, comparable like `.value`, and refused on a
/// judge that has no ratchet, at validate time.
#[test]
fn dot_ref_reads_the_ratchet_reference_and_only_on_a_ratcheted_judge() {
    let goals = [g("a", false, false), g("cc", true, false)];
    let gate = gate_with(0, 0, &[], false);
    assert!(ev_gate("cc.ref == 10", &goals, &gate));
    assert!(ev_gate("cc.ref <= 10.5", &goals, &gate));
    assert!(validate_with("cc.ref > 1", &goals, Some(&gate), Groups::none()).is_ok());

    let err = format!("{:#}", validate_with("a.ref > 1", &goals, Some(&gate), Groups::none()).unwrap_err());
    assert!(err.contains("has no ratchet"), "{err}");
    assert!(validate("cc.ref > 1", &goals).is_err(), "no gate ⇒ no ratchet ⇒ refused");

    // ratcheted but no reference yet ⇒ Missing ⇒ every comparison false, never an error.
    let mut unset = gate_with(0, 0, &[], false);
    unset.state.refs.clear();
    assert!(!ev_gate("cc.ref >= 0", &goals, &unset));
    assert!(!ev_gate("cc.ref < 0", &goals, &unset));
}

/// The requirement's `rollbacks.value >= 2` spelling is REFUSED with the one that works — a built-in
/// number has no accessor, and an alias would be a second way to write the same thing.
#[test]
fn a_gate_number_has_no_accessor_and_says_how_to_write_it() {
    let err = format!("{:#}", validate("rollbacks.value >= 2", &[]).unwrap_err());
    assert!(err.contains("`rollbacks >= 2`"), "{err}");
    assert!(validate("consecutive_rollbacks.value >= 2", &[]).is_err());
    // …and the terms are not judge names: they never join the run-set.
    assert_eq!(
        judge_names("rollbacks >= 2 OR consecutive_rollbacks >= 1 OR oscillating OR ratchet_regressed OR cc.ref > 1").unwrap(),
        vec!["cc".to_string()]
    );
}

// ── judge groups (`sequence.groups`) ─────────────────────────────────────────────────────────────

fn groups(pairs: &[(&str, &[&str])]) -> Groups {
    Groups::from_map(pairs.iter().map(|(g, m)| (g.to_string(), m.iter().map(|s| s.to_string()).collect())).collect())
}

fn ev_groups(expr: &str, goals: &[Judge], gs: &Groups) -> bool {
    evaluate(expr, &StopContext { groups: gs, ..StopContext::from_judges(goals) }).unwrap()
}

/// THE ACCEPTANCE ROW: `count_met(qs) >= 2` over three judges passes with two met and fails with one
/// — and the group's members are counted over the whole run-set, DoD-set or not.
#[test]
fn count_met_over_a_group_is_k_of_n() {
    let gs = groups(&[("qs", &["a", "b", "c"])]);
    let mut two = [g("a", true, false), g("b", true, false), g("c", false, false), g("other", true, false)];
    assert!(ev_groups("count_met(qs) >= 2", &two, &gs));
    two[1] = g("b", false, false);
    assert!(!ev_groups("count_met(qs) >= 2", &two, &gs), "one of three is not two of three");
    // not in the DoD-set: still a member.
    two.iter_mut().for_each(|j| j.in_dod = false);
    assert!(ev_groups("count_met(qs) == 1 AND total(qs) == 3", &two, &gs));
}

/// all / any / fraction / regression over a group — every aggregate takes one.
#[test]
fn every_aggregate_takes_a_group() {
    let gs = groups(&[("q", &["a", "b"]), ("r", &["c"])]);
    let goals = [g("a", true, false), regressed("b", false), g("c", true, false)];
    assert!(!ev_groups("all_goals(q)", &goals, &gs));
    assert!(ev_groups("all_goals(r)", &goals, &gs));
    assert!(ev_groups("count_met(q) >= 1", &goals, &gs), "any = count_met >= 1");
    assert!(ev_groups("met_fraction(q) == 0.5", &goals, &gs));
    assert!(ev_groups("any_regressed(q) AND count_regressed(q) == 1", &goals, &gs));
    assert!(!ev_groups("any_regressed(r)", &goals, &gs));
}

/// An unknown group is an error that LISTS the groups that exist (or says there are none).
#[test]
fn an_unknown_group_names_the_defined_ones() {
    let gs = groups(&[("quality", &["a"]), ("speed", &["b"])]);
    let goals = [g("a", true, false), g("b", true, false)];
    let err = format!("{:#}", validate_with("count_met(qualty) >= 1", &goals, None, &gs).unwrap_err());
    assert!(err.contains("unknown group `qualty`") && err.contains("quality, speed"), "{err}");
    let err = format!("{:#}", validate("count_met(quality) >= 1", &goals).unwrap_err());
    assert!(err.contains("none — define one under `sequence.groups`"), "{err}");
    assert!(validate("count_met(7) >= 1", &goals).is_err(), "a subset is a name");
}

/// k > n: a threshold the group can never reach is refused at validate time, in either operand
/// order — and only against a LITERAL (a run term like `budget_total` reads infinity at startup).
#[test]
fn a_threshold_above_the_group_size_is_refused() {
    let gs = groups(&[("q", &["a", "b"])]);
    let goals = [g("a", true, false), g("b", true, false)];
    for bad in ["count_met(q) >= 3", "count_met(q) > 2", "count_met(q) == 3", "3 <= count_met(q)", "2 < count_met(q)", "count_regressed(q) >= 5"] {
        let err = format!("{:#}", validate_with(bad, &goals, None, &gs).unwrap_err());
        assert!(err.contains("can never hold") && err.contains("2 member"), "`{bad}`: {err}");
    }
    for ok in ["count_met(q) >= 2", "count_met(q) > 1", "2 >= count_met(q)", "count_met(q) <= 5", "count_met(q) >= budget_total", "count_met(q) != 7", "(count_met(q)) >= 2"] {
        assert!(validate_with(ok, &goals, None, &gs).is_ok(), "`{ok}` must validate");
    }
}

/// A subset argument names a group, never a judge: it is not in `judge_names` (so it is never
/// resolved to a file), and `subsets` returns it — once, and never `invariants`.
#[test]
fn a_group_in_subset_position_is_not_a_judge_name() {
    let expr = "count_met(quality) >= 2 AND build AND any_regressed(invariants) AND total(quality) == 3 AND all_goals(speed)";
    assert_eq!(judge_names(expr).unwrap(), vec!["build".to_string()]);
    assert_eq!(subsets(expr).unwrap(), vec!["quality".to_string(), "speed".to_string()]);
    // a bare name in parentheses is grouping, not a subset.
    assert_eq!(judge_names("(build) AND NOT (lint)").unwrap(), vec!["build".to_string(), "lint".to_string()]);
    assert!(subsets("(build)").unwrap().is_empty());
}

/// `no_improvements` / `consecutive_no_improvements` read the gate like `rollbacks`, have no
/// accessor, and never join the run-set.
#[test]
fn the_no_improvement_terms_read_the_gate_state() {
    let goals = [g("a", false, false)];
    let mut gate = gate_with(0, 0, &[], false);
    gate.state.no_improvements = 3;
    gate.state.consecutive_no_improvements = 2;
    assert!(ev_gate("no_improvements == 3 AND consecutive_no_improvements >= 2 AND rollbacks == 0", &goals, &gate));
    assert!(ev("no_improvements == 0 AND consecutive_no_improvements == 0", &goals), "no gate ⇒ none");
    let err = format!("{:#}", validate("consecutive_no_improvements.value >= 2", &[]).unwrap_err());
    assert!(err.contains("`consecutive_no_improvements >= 2`"), "{err}");
    assert!(judge_names("no_improvements >= 1 OR consecutive_no_improvements >= 1").unwrap().is_empty());
}

/// `no_changes` / `consecutive_no_changes` (sessions that changed nothing git-visible) read the gate
/// like the other history numbers: no accessor, no judge of that name, `0` before any gate.
#[test]
fn the_no_changes_terms_read_the_gate_state() {
    let goals = [g("a", false, false)];
    let mut gate = gate_with(0, 0, &[], false);
    gate.state.no_changes = 5;
    gate.state.consecutive_no_changes = 3;
    assert!(ev_gate("no_changes == 5 AND consecutive_no_changes >= 3 AND no_improvements == 0", &goals, &gate));
    assert!(ev("no_changes == 0 AND consecutive_no_changes == 0", &goals), "no gate ⇒ none");
    let err = format!("{:#}", validate("consecutive_no_changes.value >= 2", &[]).unwrap_err());
    assert!(err.contains("`consecutive_no_changes >= 2`"), "{err}");
    assert!(judge_names("no_changes >= 1 OR consecutive_no_changes >= 1").unwrap().is_empty());
}
