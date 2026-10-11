//! The ratchet's rules and the gate history, pinned without a loop, a worker or a file.

use super::*;

fn val(v: f64) -> Verdict {
    Verdict::binary(true).with_value(v)
}

fn fresh(rows: &[(&str, Verdict)]) -> Vec<(String, Verdict)> {
    rows.iter().map(|(n, v)| (n.to_string(), v.clone())).collect()
}

fn gate(specs: &[(&str, Ratchet)]) -> Gate {
    Gate::new(specs.iter().map(|(n, s)| (n.to_string(), s.clone())).collect(), DEFAULT_OSCILLATION_WINDOW)
}

fn sig(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// The three acceptance rows, against one reference: worse beyond tolerance regresses, worse
/// within it does not, better does not — in both directions.
#[test]
fn worse_beyond_tolerance_regresses_within_it_does_not() {
    let down = Ratchet::down(0.5);
    assert!(down.regressed(10.0, 10.6), "lower is better: +0.6 is beyond 0.5");
    assert!(!down.regressed(10.0, 10.5), "exactly the tolerance is accepted");
    assert!(!down.regressed(10.0, 9.0), "an improvement never regresses");

    let up = Ratchet::up(1.0);
    assert!(up.regressed(71.0, 69.9));
    assert!(!up.regressed(71.0, 70.0));
    assert!(!up.regressed(71.0, 80.0));

    // zero tolerance still survives float noise.
    assert!(!Ratchet::down(0.0).regressed(0.3, 0.1 + 0.2));
}

/// `tolerance_pct` is relative to the reference, and the LARGER allowance applies.
#[test]
fn the_larger_of_the_absolute_and_relative_allowance_applies() {
    let r = Ratchet::down(0.5).tolerance_pct(10.0);
    assert_eq!(r.allowance(100.0), 10.0, "10% of 100 beats 0.5");
    assert_eq!(r.allowance(2.0), 0.5, "0.5 beats 10% of 2");
    assert!(!r.regressed(100.0, 109.0));
    assert!(r.regressed(100.0, 111.0));
}

#[test]
fn a_ratchet_that_cannot_mean_anything_is_refused() {
    assert!(Ratchet::down(-1.0).validate("x").is_err());
    assert!(Ratchet::down(f64::NAN).validate("x").is_err());
    assert!(Ratchet::up(0.0).tolerance_pct(-5.0).validate("x").is_err());
    assert!(Ratchet::up(0.0).baseline(f64::INFINITY).validate("x").is_err());
    assert!(Ratchet::up(0.0).baseline(-3.0).validate("x").is_ok(), "a negative seed is a real number");
}

/// The seed is the reference from the first moment, and wins over the measurement; drift beyond
/// tolerance is reported, not silently accepted.
#[test]
fn a_seed_wins_over_the_baseline_pass_and_drift_is_warned() {
    let mut g = gate(&[("mutation", Ratchet::up(1.0).baseline(71.0))]);
    assert_eq!(g.state.reference("mutation"), Some(71.0), "seeded before any judge ran");

    let quiet = g.seed(&fresh(&[("mutation", val(71.5))])).unwrap();
    assert!(quiet.is_empty(), "within tolerance is not drift: {quiet:?}");
    let drift = g.seed(&fresh(&[("mutation", val(60.0))])).unwrap();
    assert_eq!(drift.len(), 1);
    assert!(drift[0].contains("drift"), "{drift:?}");
    assert_eq!(g.state.reference("mutation"), Some(71.0), "the seed stays the reference");

    let errored = g.seed(&fresh(&[("mutation", Verdict::failed("boom"))])).unwrap();
    assert!(errored[0].contains("using the seed"), "{errored:?}");
}

/// Without a seed, the baseline pass measures the reference — and a judge that cannot be measured
/// refuses the run instead of leaving the ratchet inert.
#[test]
fn without_a_seed_the_baseline_pass_sets_the_reference_or_refuses() {
    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    assert_eq!(g.state.reference("cc"), None);
    g.seed(&fresh(&[("cc", val(12.0))])).unwrap();
    assert_eq!(g.state.reference("cc"), Some(12.0));

    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    let err = g.seed(&fresh(&[("cc", Verdict::binary(true))])).unwrap_err().to_string();
    assert!(err.contains("no `value`"), "{err}");

    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    let err = g.seed(&fresh(&[("cc", Verdict::failed("timeout"))])).unwrap_err().to_string();
    assert!(err.contains("no `baseline:` seed"), "{err}");

    // a numberless judge is refused even WITH a seed: no gate could ever hold it.
    let mut g = gate(&[("cc", Ratchet::down(0.0).baseline(3.0))]);
    assert!(g.seed(&fresh(&[("cc", Verdict::binary(true))])).is_err());

    // a judge absent from the pass (the driver has none) is left alone.
    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    assert!(g.seed(&[]).unwrap().is_empty());
    assert_eq!(g.state.reference("cc"), None);
}

/// The driver's baseline: a pre-step ask sets the reference once; a seed is checked, not replaced.
#[test]
fn a_pre_step_ask_is_the_driver_baseline() {
    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    assert!(g.observe_pre_step("cc", &val(5.0)).is_none());
    assert!(g.observe_pre_step("cc", &val(9.0)).is_none());
    assert_eq!(g.state.reference("cc"), Some(5.0), "the FIRST pre-step value is the baseline");
    assert!(g.observe_pre_step("other", &val(1.0)).is_none(), "an unratcheted judge is ignored");

    let mut g = gate(&[("cc", Ratchet::down(0.0).baseline(5.0))]);
    assert!(g.observe_pre_step("cc", &val(8.0)).unwrap().contains("drift"));
    assert_eq!(g.state.reference("cc"), Some(5.0));

    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    let w = g.observe_pre_step("cc", &Verdict::binary(true)).unwrap();
    assert!(w.contains("no `value`") && w.contains("ratchet regression"), "{w}");
}

/// `check` names the judge with its old and new value; an errored or numberless ratcheted judge
/// never passes; an unratcheted judge or one with no reference yet is never a ratchet regression.
#[test]
fn check_reports_judge_old_and_new_and_never_passes_an_unmeasured_value() {
    let g = gate(&[("cc", Ratchet::down(0.5).baseline(10.0)), ("cov", Ratchet::up(0.0))]);
    let regs = g.check(&fresh(&[("cc", val(11.0)), ("cov", val(1.0)), ("other", val(99.0))]));
    assert_eq!(regs, vec![RatchetRegression { judge: "cc".into(), from: Some(10.0), to: Some(11.0) }]);

    assert!(g.check(&fresh(&[("cc", val(10.4))])).is_empty(), "within tolerance");

    for unmeasured in [Verdict::failed("boom"), Verdict::binary(true), val(f64::NAN)] {
        let regs = g.check(&fresh(&[("cc", unmeasured.clone()), ("cov", unmeasured)]));
        assert_eq!(regs.len(), 2, "an unmeasured ratcheted judge never passes, reference or not");
        assert!(regs.iter().all(|r| r.to.is_none()));
    }
    assert_eq!(describe(&regs_of(&g)), vec!["ratchet `cc`: 10 → 12".to_string()]);
}

fn regs_of(g: &Gate) -> Vec<RatchetRegression> {
    g.check(&fresh(&[("cc", val(12.0))]))
}

/// The reference moves only on a KEEP, and only to a measured value.
#[test]
fn improving_moves_the_reference_and_a_rollback_does_not() {
    let mut g = gate(&[("cc", Ratchet::down(0.5).baseline(10.0))]);
    g.record(GateDecision::RolledBack, &fresh(&[("cc", val(12.0))]), &regs_of(&g), sig(&["cc"]));
    assert_eq!(g.state.reference("cc"), Some(10.0), "a rolled-back value never becomes the reference");
    assert!(g.state.ratchet_regressed());

    g.record(GateDecision::Kept, &fresh(&[("cc", val(8.0))]), &[], sig(&[]));
    assert_eq!(g.state.reference("cc"), Some(8.0), "the kept improvement is the new reference");
    assert!(!g.state.ratchet_regressed(), "the latest gate regressed nothing");

    g.record(GateDecision::Kept, &fresh(&[("cc", Verdict::failed("x"))]), &[], sig(&[]));
    assert_eq!(g.state.reference("cc"), Some(8.0), "an unmeasured keep leaves the reference alone");
}

#[test]
fn rollbacks_count_and_consecutive_resets_on_a_keep() {
    let mut g = Gate::default();
    let f = fresh(&[("a", Verdict::binary(false))]);
    g.record(GateDecision::RolledBack, &f, &[], sig(&["a"]));
    g.record(GateDecision::Nothing, &f, &[], sig(&["a"]));
    g.record(GateDecision::RolledBack, &f, &[], sig(&["a"]));
    assert_eq!((g.state.rollbacks, g.state.consecutive_rollbacks), (2, 2), "a no-change gate neither breaks nor extends the streak");
    g.record(GateDecision::Kept, &f, &[], sig(&["a"]));
    assert_eq!((g.state.rollbacks, g.state.consecutive_rollbacks), (2, 0));

    // a gate with no verdicts still counts, but records no signature.
    g.record(GateDecision::RolledBack, &[], &[], vec![]);
    assert_eq!(g.state.rollbacks, 3);
    assert_eq!(g.state.signatures.len(), 4);
}

fn history(sigs: &[&[&str]]) -> GateState {
    GateState { signatures: sigs.iter().map(|s| sig(s)).collect(), ..GateState::default() }
}

/// A→B→A trips it; A→A→A does not (that is a stall, not an oscillation); a monotonically
/// improving run never does; a repeat older than the window does not; all-green never counts.
#[test]
fn oscillating_is_a_b_a_within_the_window() {
    assert!(history(&[&["a"], &["b"], &["a"]]).oscillating(4));
    assert!(history(&[&["a", "b"], &["c"], &["c"], &["a", "b"]]).oscillating(4));
    assert!(!history(&[&["a"], &["a"], &["a"]]).oscillating(4), "no different set in between");
    assert!(!history(&[&["a", "b", "c"], &["b", "c"], &["c"], &[]]).oscillating(4), "monotone improvement");
    assert!(!history(&[&[], &["a"], &[]]).oscillating(4), "returning to all-green is not oscillation");
    assert!(!history(&[&["a"], &["b"]]).oscillating(4), "short history");
    assert!(!history(&[]).oscillating(4));

    let old = history(&[&["a"], &["b"], &["c"], &["d"], &["e"], &["a"]]);
    assert!(!old.oscillating(4), "the earlier `a` is five gates back");
    assert!(old.oscillating(5));
}

#[test]
fn the_signature_is_failing_in_scope_judges_plus_ratchet_regressions() {
    let f = fresh(&[
        ("b", Verdict::binary(false)),
        ("a", Verdict::failed("x")),
        ("ok", Verdict::binary(true)),
        ("detector", Verdict::binary(false)),
    ]);
    let reg = [RatchetRegression { judge: "cc".into(), from: Some(1.0), to: Some(2.0) }];
    assert_eq!(signature(&f, |n| n != "detector", &reg), sig(&["a", "b", "cc"]));
}

#[test]
fn the_history_is_bounded() {
    let mut g = Gate::default();
    let f = fresh(&[("a", Verdict::binary(false))]);
    for _ in 0..(HISTORY_CAP + 10) {
        g.record(GateDecision::Nothing, &f, &[], sig(&["a"]));
    }
    assert_eq!(g.state.signatures.len(), HISTORY_CAP);
}

#[test]
fn the_state_round_trips_through_agg_private() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load(tmp.path()).is_none(), "no file is no state, not a crash");
    let mut g = gate(&[("cc", Ratchet::down(0.0).baseline(4.0))]);
    g.record(GateDecision::RolledBack, &fresh(&[("cc", val(5.0))]), &regs_of(&g), sig(&["cc"]));
    save(tmp.path(), &g.state).unwrap();
    assert!(crate::paths::gate_json(tmp.path()).starts_with(crate::paths::private_dir(tmp.path())));
    assert_eq!(load(tmp.path()), Some(g.state));
}

/// `improved` mirrors `regressed`: the GOOD way beyond the allowance, in both directions — noise
/// inside the tolerance is no more an improvement than it is a regression.
#[test]
fn improved_is_better_beyond_tolerance() {
    let down = Ratchet::down(0.5);
    assert!(down.improved(10.0, 9.4), "lower is better: -0.6 is beyond 0.5");
    assert!(!down.improved(10.0, 9.5), "exactly the tolerance is noise");
    assert!(!down.improved(10.0, 11.0), "worse is never an improvement");
    let up = Ratchet::up(0.0);
    assert!(up.improved(71.0, 71.5));
    assert!(!up.improved(0.3, 0.1 + 0.2), "float noise is not an improvement");
}

/// The gate's list: only ratcheted judges with a reference and a graded number can improve.
#[test]
fn the_gate_lists_the_ratcheted_judges_that_improved() {
    let mut g = gate(&[("cc", Ratchet::down(0.5)), ("cov", Ratchet::up(1.0)), ("fresh", Ratchet::up(0.0))]);
    g.state.refs.insert("cc".into(), 10.0);
    g.state.refs.insert("cov".into(), 70.0);
    let rows = fresh(&[
        ("cc", val(9.0)),                      // improved
        ("cov", val(70.5)),                    // within tolerance
        ("fresh", val(5.0)),                   // no reference yet
        ("other", val(1.0)),                   // not ratcheted
    ]);
    assert_eq!(g.improved(&rows), vec!["cc".to_string()]);
    assert!(g.improved(&fresh(&[("cc", Verdict::failed("boom"))])).is_empty(), "an error never improves");
}

/// A `NoImprovement` gate is not a rollback: neither counter moves and no reference moves, but the
/// signature is still recorded (the session still showed where the worker went).
#[test]
fn a_no_improvement_gate_counts_for_no_rollback_and_moves_no_reference() {
    let mut g = gate(&[("cc", Ratchet::down(0.0).baseline(10.0))]);
    g.record(GateDecision::RolledBack, &fresh(&[("cc", val(11.0))]), &[], sig(&["cc"]));
    g.record(GateDecision::NoImprovement, &fresh(&[("cc", val(10.0))]), &[], sig(&[]));
    assert_eq!((g.state.rollbacks, g.state.consecutive_rollbacks), (1, 1), "neither extended nor broken");
    assert_eq!((g.state.no_improvements, g.state.consecutive_no_improvements), (1, 1));
    assert_eq!(g.state.reference("cc"), Some(10.0), "a no_improvement value never becomes the reference");
    g.record(GateDecision::RolledBack, &fresh(&[("cc", val(11.0))]), &[], sig(&["cc"]));
    assert_eq!(g.state.consecutive_no_improvements, 1, "a rollback does not break the no_improvement streak");
    g.record(GateDecision::Kept, &fresh(&[("cc", val(9.0))]), &[], sig(&[]));
    assert_eq!((g.state.no_improvements, g.state.consecutive_no_improvements, g.state.consecutive_rollbacks), (1, 0, 0));
    assert_eq!(g.state.signatures.len(), 4);
}

/// A `NoChanges` gate (the worker changed nothing git-visible) counts in its own terms only: no
/// rollback, no reference moves, the other streaks are neither extended nor broken, and a kept
/// gate ends its streak.
#[test]
fn a_no_changes_gate_counts_in_its_own_terms_and_a_keep_resets_the_streak() {
    let mut g = gate(&[("cc", Ratchet::down(0.0).baseline(10.0))]);
    g.record(GateDecision::NoImprovement, &fresh(&[("cc", val(10.0))]), &[], sig(&[]));
    g.record(GateDecision::NoChanges, &fresh(&[("cc", val(8.0))]), &[], sig(&[]));
    g.record(GateDecision::RolledBack, &fresh(&[("cc", val(11.0))]), &[], sig(&["cc"]));
    g.record(GateDecision::NoChanges, &fresh(&[("cc", val(8.0))]), &[], sig(&[]));
    assert_eq!((g.state.no_changes, g.state.consecutive_no_changes), (2, 2), "a rollback does not break it");
    assert_eq!((g.state.rollbacks, g.state.consecutive_rollbacks), (1, 1), "not a rollback, nor a break");
    assert_eq!(g.state.consecutive_no_improvements, 1, "nor a break of the no_improvement streak");
    assert_eq!(g.state.reference("cc"), Some(10.0), "nothing merged, so no reference moves");
    g.record(GateDecision::Nothing, &fresh(&[("cc", val(8.0))]), &[], sig(&[]));
    assert_eq!(g.state.consecutive_no_changes, 2, "a veto or conflict is not a no-change session");
    g.record(GateDecision::Kept, &fresh(&[("cc", val(9.0))]), &[], sig(&[]));
    assert_eq!((g.state.no_changes, g.state.consecutive_no_changes), (2, 0));
    assert_eq!(g.state.signatures.len(), 6, "every judged gate still records where the worker was");
}

/// A `mixed` verdict — its own tools moved in different directions — is never an improvement,
/// however far its number moved; it still regresses like any other.
#[test]
fn a_mixed_verdict_is_no_improvement_but_can_still_regress() {
    let mut g = gate(&[("cc", Ratchet::down(0.0))]);
    g.state.refs.insert("cc".into(), 10.0);
    let mut better = val(5.0);
    better.mixed = true;
    assert!(g.improved(&fresh(&[("cc", better)])).is_empty(), "tools disagree: not an improvement");
    let mut worse = val(12.0);
    worse.mixed = true;
    assert_eq!(g.check(&fresh(&[("cc", worse)])).len(), 1, "…but a regression is still one");
}

/// A YAML ratchet may leave its direction to its judge; a driver's always states one.
#[test]
fn a_ratchet_cfg_resolves_with_the_judges_direction() {
    let cfg = RatchetCfg { direction: None, tolerance: 0.5, tolerance_pct: None, baseline: Some(3.0) };
    assert_eq!(cfg.resolve(None), None, "nobody states a direction");
    assert_eq!(cfg.resolve(Some(Direction::Down)), Some(Ratchet::down(0.5).baseline(3.0)));
    let written: RatchetCfg = Ratchet::up(1.0).into();
    assert_eq!(written.resolve(Some(Direction::Down)).map(|r| r.direction), Some(Direction::Up), "a written direction wins");
    assert_eq!(resolve_explicit(&[("w".to_string(), written), ("n".to_string(), cfg.clone())].into()).len(), 1);
    assert!(RatchetCfg { tolerance: -1.0, ..cfg }.validate("cc").is_err());
}

/// THE BASIS: a score's reference is stamped with its definition; a resumed state whose reference
/// was taken under ANOTHER definition is reset — to what this gate already measured, else to none.
#[test]
fn a_reference_of_another_scoring_definition_is_reset_on_restore() {
    let bases = |b: &str| [("q".to_string(), b.to_string())].into_iter().collect::<BTreeMap<_, _>>();
    let mut old = gate(&[("q", Ratchet::up(0.0)), ("cc", Ratchet::down(0.0))]).with_bases(bases("swe-default@1"));
    old.seed(&fresh(&[("q", val(70.0)), ("cc", val(9.0))])).unwrap();
    old.record(GateDecision::Kept, &fresh(&[("q", val(75.0)), ("cc", val(8.0))]), &[], sig(&[]));
    assert_eq!(old.state.bases["q"], "swe-default@1", "stamped when kept");
    let persisted = old.state.clone();

    // the same definition: restored as it was.
    let mut same = gate(&[("q", Ratchet::up(0.0)), ("cc", Ratchet::down(0.0))]).with_bases(bases("swe-default@1"));
    assert!(same.restore(persisted.clone()).is_empty());
    assert_eq!(same.state.reference("q"), Some(75.0));

    // a changed definition, after a baseline pass under it: that measurement is the reference.
    let mut changed = gate(&[("q", Ratchet::up(0.0)), ("cc", Ratchet::down(0.0))]).with_bases(bases("swe-default@2"));
    changed.seed(&fresh(&[("q", val(60.0)), ("cc", val(8.0))])).unwrap();
    let warned = changed.restore(persisted.clone());
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].contains("swe-default@1") && warned[0].contains("swe-default@2"), "{warned:?}");
    assert_eq!(changed.state.reference("q"), Some(60.0), "the incomparable 75 is gone");
    assert_eq!(changed.state.reference("cc"), Some(8.0), "a judge without a basis is restored as is");
    assert_eq!(changed.state.bases["q"], "swe-default@2");

    // …and without a measurement of its own, no reference at all until the next keep.
    let mut bare = gate(&[("q", Ratchet::up(0.0))]).with_bases(bases("swe-default@2"));
    bare.restore(persisted);
    assert_eq!(bare.state.reference("q"), None);
    assert!(bare.check(&fresh(&[("q", val(1.0))])).is_empty(), "nothing to regress against");
}
