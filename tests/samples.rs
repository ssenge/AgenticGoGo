//! The shipped samples are REAL, not prose.
//!
//! `examples/workflow.rs` and `examples/selfimprove.rs` are cargo examples, so `cargo build
//! --examples` is what keeps the Rust surface honest. `examples/workflow.yaml` has no compiler, so
//! this file is its compiler: it loads the sample through the SAME `AggConfig::load` the binary
//! uses and asserts the shape the sample's own comments promise.
//!
//! Why a test and not a doc line: the sample is the specification of the YAML surface. A key that
//! quietly stopped existing (or a `deny_unknown_fields` struct that grew a required field) would
//! otherwise be found by the first user who copied the file, not by CI.

use agg::core::config::AggConfig;
use std::path::PathBuf;

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples").join(name)
}

/// The whole file parses. `deny_unknown_fields` is on every struct in `core::config`, so this fails
/// on a stale key rather than silently ignoring it — which is exactly what it is here to catch.
#[test]
fn the_yaml_sample_loads() {
    let cfg = AggConfig::load(&sample("workflow.yaml")).expect("examples/workflow.yaml must load");

    assert_eq!(cfg.project, "rate-limiting");

    // ---- defaults: the one inherited block ----
    assert_eq!(cfg.defaults.agent, "claude");
    assert_eq!(cfg.defaults.model.as_deref(), Some("claude-opus-4-8[1m]"));
    assert_eq!(cfg.defaults.effort.as_deref(), Some("high"));

    // ---- steps: the palette ----
    let names: Vec<&str> = cfg.steps.keys().map(String::as_str).collect();
    assert_eq!(names, ["document", "fix", "harden", "implement", "spec", "survey"], "BTreeMap ⇒ sorted");

    // `spec` escapes the `defaults:` model with the EMPTY-STRING sentinel, not `null`: `null`
    // deserializes to `None`, which `resolve_step` then fills from `defaults`. The sample's comment
    // says so; this asserts the file actually spells it that way.
    assert_eq!(cfg.steps["spec"].agent.as_deref(), Some("codex"));
    assert_eq!(cfg.steps["spec"].model.as_deref(), Some(""), "the opt-out sentinel is \"\", not null");

    // the three sandboxed steps, and the readonly/writable asymmetry the sample is built around
    for s in ["implement", "fix", "harden"] {
        assert_eq!(
            cfg.steps[s].isolation,
            Some(agg::isolation::Isolation::Sandbox),
            "`{s}` must name a confining tier — readonly binds to nothing under `none`"
        );
        // the BODY keeps the author's spelling; `resolve_step` is where it is normalised, so the
        // two lists cannot miss each other by a trailing slash.
        assert_eq!(cfg.steps[s].readonly, ["tests/", "agg/judges/"]);
        let r = cfg.resolve_step(s).unwrap();
        assert_eq!(r.readonly, ["tests", "agg/judges"], "normalised by resolve_step, as the driver's builder does it");
    }
    assert_eq!(cfg.resolve_step("implement").unwrap().writable, ["tests"], "the step that SHOULD add tests re-grants that one deny");
    assert!(cfg.steps["fix"].writable.is_empty(), "`fix` re-grants nothing — deleting a failing test is the shortcut it would take");

    // ---- judges: per-judge config, and `timeout:` is the only key ----
    assert_eq!(cfg.judges["load_ok"].timeout, Some(2700), "a 45-minute load test dies under the 300s run default");
    assert_eq!(cfg.judges["p99_ok"].timeout, Some(60));
    assert_eq!(cfg.judge.timeout, 300, "the RUN-level default is untouched by a per-judge override");

    // ---- sequence: the walk, the DoD, the ceilings ----
    let seq = &cfg.sequence;
    /// one `sequence.steps` entry, flattened: name · times · until · max. These four ARE the
    /// schema since the 2026-08-04 cut — serde fields, not a grammar.
    type Entry<'a> = (&'a str, Option<u32>, Option<&'a str>, Option<u32>);
    let walk: Vec<Entry<'_>> =
        seq.steps.iter().map(|e| (e.step.as_str(), e.times, e.until.as_deref(), e.max)).collect();
    assert_eq!(
        walk,
        [
            ("survey", None, Some("survey_good.value >= 85"), Some(3)),
            ("spec", None, None, None),
            ("implement", None, None, None),
            ("fix", None, Some("tests_pass AND lint_clean"), Some(8)),
            ("harden", None, None, None),
            ("document", None, None, None),
        ],
        "the lap runs every entry in order — there is no `if:` in YAML since the 2026-08-04 cut"
    );

    assert_eq!(seq.done_if, "tests_pass AND lint_clean AND load_ok AND p99_ok");
    assert_eq!(seq.abort_if.as_deref(), Some("over_budget OR wall_hours >= 12 OR any_regressed(invariants)"));
    assert_eq!(seq.invariants, ["builds"]);
    assert_eq!(seq.notify_if.as_deref(), Some("stalled"));
    let notify = seq.notify.as_ref().expect("notify_if without notify.cmd is a startup refusal");
    assert_eq!(notify.cooldown_sessions, 5);
    assert_eq!(notify.cmd.len(), 1);
    assert!(notify.cmd[0].contains("--max-time"), "delivery is foreground and untimed — the sample must bound it");

    // ---- limits, including the field `Limits` grew for the Rust path ----
    assert_eq!(seq.limits.tokens, Some(40_000_000));
    assert_eq!(seq.limits.sessions, Some(400));
    assert_eq!(seq.limits.cost, None);
    assert_eq!(seq.limits.wall_hours, None, "this file says `wall_hours` as an abort_if TERM, not a limit");

    // ---- on_regression: `annotate` = do not discard a regressed span ----
    assert!(!seq.gate_regressions, "`annotate` is the sample's policy: always merge, tell the next session");
}

/// The sample is not just parseable, it is RUNNABLE-shaped: `assemble()` is what `agg run` calls
/// before the first session, and it is where an unknown step name, an unbounded `until:` or a judge
/// that resolves to no file becomes a startup refusal.
///
/// ⚠ Judge resolution reads `<config_base>/judges/`, so this points at the sample's own judge
/// directory. It is the one part of the sample that is prose — the sample ships no judge scripts —
/// so this test asserts the refusal is about a MISSING FILE and nothing else. That is still worth
/// asserting: it proves every judge name in the file reached resolution, i.e. the expressions
/// parsed and named the judges the sample says they do.
#[test]
fn the_yaml_samples_expressions_name_real_judges() {
    let cfg = AggConfig::load(&sample("workflow.yaml")).unwrap();
    let names = agg::core::stop::judge_names(&cfg.sequence.done_if).expect("done_if parses");
    assert_eq!(names, ["tests_pass", "lint_clean", "load_ok", "p99_ok"]);

    let names = agg::core::stop::judge_names(cfg.sequence.abort_if.as_deref().unwrap()).expect("abort_if parses");
    assert!(names.is_empty(), "over_budget/wall_hours are run-level scalars and any_regressed(invariants) is an aggregate, not a judge name: {names:?}");

    let names = agg::core::stop::judge_names(cfg.sequence.steps[0].until.as_deref().unwrap()).unwrap();
    assert_eq!(names, ["survey_good"], "`until:` judges join the run-set — they must execute every step");
}
