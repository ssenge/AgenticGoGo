use super::*;

/// Parse a config body the way `AggConfig::load` does (minus the env overrides).
fn parse(body: &str) -> Result<AggConfig, serde_yaml::Error> {
    serde_yaml::from_str::<AggConfig>(body)
}

/// The smallest config that parses — `project` + `sequence` are the only required keys.
const MINIMAL: &str = "project: p\nsteps: { worker: {} }\nsequence: { steps: [worker] }\n";

#[test]
fn the_minimal_config_parses_with_all_defaults() {
    let cfg = parse(MINIMAL).expect("minimal config parses");
    assert_eq!(cfg.project, "p");
    assert_eq!(cfg.defaults.agent, "claude");
    assert_eq!(cfg.judge.timeout, 300);
    assert_eq!(cfg.sequence.done_if, "all_goals");
    assert!(cfg.sequence.gate_regressions, "gate_regressions defaults ON (the rename default)");
}

/// §4.1: the three ceilings are UNIFIED under `sequence.limits:`; the old `budget:`/`cost:`/
/// `max_sessions:` keys are RETIRED everywhere. A stale top-level `budget:` would be a decorative
/// spend ceiling — an unbounded loop — and even the pre-unification `sequence.budget:` is gone now
/// (the intended clean break; internal tool, no users). `deny_unknown_fields` makes each a HARD
/// ERROR instead of a silent no-op. This is THE guard the config move depends on.
#[test]
fn the_retired_ceiling_keys_are_a_hard_error_not_silently_ignored() {
    // a stray TOP-LEVEL budget: (its pre-move home) is refused.
    let top = parse(&format!("{MINIMAL}budget: {{ total: 5 }}\n")).unwrap_err().to_string();
    assert!(top.contains("unknown field `budget`"), "must reject the retired key, got: {top}");
    // and the PRE-UNIFICATION `sequence.budget:` is gone too — an OLD config hard-errors.
    let old = parse("project: p\nsteps: { worker: {} }\nsequence: { steps: [worker], budget: { total: 5 } }\n")
        .unwrap_err()
        .to_string();
    assert!(old.contains("unknown field `budget`"), "the pre-unification `sequence.budget:` is retired, got: {old}");
    // …and the unified block IS the new home and must parse (all three keys, null-able).
    parse("project: p\nsteps: { worker: {} }\nsequence: { steps: [worker], limits: { tokens: 5, cost: 1.5, sessions: 3 } }\n")
        .expect("`limits` under `sequence:` is the unified home and must parse");
}

/// Extract the bodies of every ```` ```yaml ```` fenced block from a markdown string (the fence
/// line itself is dropped). Used to pull the config scaffold out of an embedded SKILL.md.
fn fenced_yaml_blocks(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(i) = rest.find("```yaml") {
        let after = &rest[i + "```yaml".len()..];
        // the body starts after the rest of the opening-fence line.
        let Some(nl) = after.find('\n') else { break };
        let body = &after[nl + 1..];
        let Some(end) = body.find("```") else { break };
        out.push(body[..end].to_string());
        rest = &body[end + 3..];
    }
    out
}

/// REGRESSION (the guard that would have caught the shipped bug): the `/agg:new` scaffold that
/// the binary EMBEDS must still parse as a CURRENT config. The `/agg:*` skills are
/// `include_str!`'d into the binary as string LITERALS (`crate::skills::SKILLS`), so a stale
/// scaffold compiles cleanly — the green build does NOT catch a scaffold whose generated config
/// is invalid. That gap already shipped one real bug: the scaffold emitted `budget:`/
/// `max_sessions:` configs that now HARD-ERROR under `deny_unknown_fields`.
///
/// This reads the SAME embedded body the binary installs (NOT a fresh file read), extracts the
/// `agg.yaml` scaffold block, fills its human placeholders with concrete VALID values, and
/// asserts it parses through the REAL config parser. If the scaffold ever reintroduces a retired
/// key (budget / cost / max_sessions / stop_when / halt_when / goals), `deny_unknown_fields`
/// rejects it and this test FAILS — which is the whole point. (We use the real parse, as
/// preferred over a substring assertion: a substring check for `cost:` would false-positive on
/// the scaffold's own `# cost:` comment line, whereas the parser ignores comments and rejects
/// only a live retired key.)
#[test]
fn the_embedded_new_scaffold_parses_as_a_current_config() {
    // the SAME include_str! body the binary ships — guard what actually installs.
    let (_, new_skill) = crate::skills::SKILLS
        .iter()
        .find(|(name, _)| *name == "agg-new")
        .expect("the agg-new skill must be embedded in SKILLS");

    // the scaffold is the fenced yaml block carrying the whole config: sequence.limits + done_if.
    let scaffold = fenced_yaml_blocks(new_skill)
        .into_iter()
        .find(|b| b.contains("sequence:") && b.contains("limits:") && b.contains("done_if"))
        .expect("the /agg:new scaffold (a ```yaml block with sequence.limits + done_if) must be present");

    // fill the human placeholders with concrete VALID values (an agent / a number / a judge name).
    let filled = scaffold
        .replace("<name>", "p")
        .replace("<claude|codex|copilot>", "claude")
        .replace("<int or null>", "100")
        .replace("<judge names that must STAY met>", "all_tests_pass")
        .replace("<expression over judge names>", "all_tests_pass")
        .replace("<ceiling expression>", "over_iterations");

    // any `<…>` left on a NON-comment line means the scaffold grew a placeholder this test does
    // not fill — fail loudly here rather than let serde emit something cryptic.
    for line in filled.lines() {
        let code = line.split('#').next().unwrap_or("");
        assert!(
            !code.contains('<'),
            "unfilled placeholder on a live scaffold line — add a substitution to this test:\n  {line}"
        );
    }

    // THE assertion: the filled scaffold parses through the real deny_unknown_fields parser. A
    // retired key anywhere in it makes this fail.
    let cfg = parse(&filled).unwrap_or_else(|e| {
        panic!("the /agg:new scaffold must parse as a current AggConfig, got:\n{e}\n--- filled scaffold ---\n{filled}")
    });

    // …and it is the CURRENT shape, not a coincidental parse: the unified ceiling home + the
    // renamed Definition-of-Done keys are the ones a retired-key reversion would break.
    assert_eq!(cfg.sequence.limits.tokens, Some(100), "sequence.limits.tokens is the unified token-ceiling home");
    assert_eq!(cfg.sequence.done_if, "all_tests_pass", "done_if (the rename of stop_when) must carry the scaffold's DoD");
    assert_eq!(
        cfg.sequence.abort_if.as_deref(),
        Some("over_iterations"),
        "abort_if (the rename of halt_when) must be present in the scaffold"
    );
}

/// §4.1: the RULER block is immutable; naming any `judge*` key (or any non-[`StepBody`] key) in a
/// step body is a HARD ERROR — a grader that moves makes verdicts incomparable across cycles.
#[test]
fn a_judge_key_in_a_step_body_is_a_hard_error() {
    let err = parse("project: p\nsteps: { worker: { judge: mine } }\nsequence: { steps: [worker] }\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown field `judge`"), "a step may not name a judge, got: {err}");
    // a stray recheck-era key in a step body is likewise refused (deny_unknown catches all of them).
    let err2 = parse("project: p\nsteps: { worker: { recheck: once_met } }\nsequence: { steps: [worker] }\n")
        .unwrap_err()
        .to_string();
    assert!(err2.contains("unknown field `recheck`"), "got: {err2}");
}

/// §4.1/§7.3: `resume_sessions` is refused unconditionally — a per-agent session id cannot cross
/// a mixed sequence, so the key is rejected at PARSE time (there is no field for it anywhere).
#[test]
fn resume_sessions_is_refused_unconditionally() {
    // top level
    let top = parse(&format!("{MINIMAL}resume_sessions: [1, 2]\n")).unwrap_err().to_string();
    assert!(top.contains("unknown field `resume_sessions`"), "got: {top}");
    // and inside a step body
    let step =
        parse("project: p\nsteps: { worker: { resume_sessions: [1] } }\nsequence: { steps: [worker] }\n")
            .unwrap_err()
            .to_string();
    assert!(step.contains("unknown field `resume_sessions`"), "got: {step}");
}

/// A step body overrides the keys it names and INHERITS the rest from `defaults:` (§4). Proves
/// the per-step agent override resolves, and that an un-named key falls through.
#[test]
fn a_step_overrides_what_it_names_and_inherits_the_rest() {
    let cfg = parse(
        "project: p\n\
         defaults: { agent: claude, model: opus, effort: high, worker_args: [\"--sandbox\"], state: S.md }\n\
         steps:\n  plan: {}\n  build: { agent: codex, model: gpt, skip_judges: true }\n\
         sequence: { steps: [plan, build] }\n",
    )
    .expect("config parses");

    // `plan` names nothing → pure defaults.
    let plan = cfg.resolve_step("plan").unwrap();
    assert_eq!(plan.agent, "claude");
    assert_eq!(plan.model.as_deref(), Some("opus"));
    assert_eq!(plan.effort.as_deref(), Some("high"));
    assert_eq!(plan.worker_args, vec!["--sandbox".to_string()]);
    assert_eq!(plan.state, "S.md");
    assert!(!plan.skip_judges);

    // `build` overrides agent + model + skip_judges; effort/worker_args/state inherit.
    let build = cfg.resolve_step("build").unwrap();
    assert_eq!(build.agent, "codex", "the per-step agent override resolves");
    assert_eq!(build.model.as_deref(), Some("gpt"));
    assert_eq!(build.effort.as_deref(), Some("high"), "effort inherits from defaults");
    assert_eq!(build.worker_args, vec!["--sandbox".to_string()], "worker_args inherits");
    assert!(build.skip_judges);

    // an unknown step name is a hard error that lists the palette (never a runtime surprise).
    let err = cfg.resolve_step("nope").unwrap_err().to_string();
    assert!(err.contains("unknown step `nope`") && err.contains("plan") && err.contains("build"), "got: {err}");
}

/// §10.2/§10.7: per-step blast-radius `isolation:` resolves like every other step key — a step
/// overrides what it names, inherits `defaults.isolation` otherwise, and falls back to `None` when
/// neither names it. (This is a DIFFERENT axis from git session isolation — see ISOLATION.md §10.8.)
#[test]
fn resolve_step_merges_isolation_step_over_defaults_default_none() {
    use crate::isolation::Isolation;

    // no isolation named anywhere ⇒ the tier defaults to None (today's direct-subprocess behaviour).
    let bare = parse(MINIMAL).unwrap();
    assert_eq!(bare.resolve_step("worker").unwrap().isolation, Isolation::None, "unset ⇒ None");

    // defaults names sandbox; a step overrides back to none; a bare step inherits the default.
    let cfg = parse(
        "project: p\n\
         defaults: { isolation: sandbox }\n\
         steps:\n  plan: { isolation: none }\n  build: {}\n\
         sequence: { steps: [plan, build] }\n",
    )
    .expect("isolation parses on both defaults and a step body");

    assert_eq!(cfg.resolve_step("plan").unwrap().isolation, Isolation::None, "step override wins over defaults");
    assert_eq!(cfg.resolve_step("build").unwrap().isolation, Isolation::Sandbox, "an un-named step inherits defaults.isolation");
}

/// `agent_names` returns EVERY distinct agent the sequence names (defaults + ruler + per-step),
/// sorted and de-duped — so `doctor`/capability can cover them all (§7.3).
#[test]
fn agent_names_collects_every_distinct_agent() {
    let cfg = parse(
        "project: p\ndefaults: { agent: claude }\njudge: { agent: claude }\n\
         steps:\n  plan: {}\n  build: { agent: codex }\n  review: { agent: copilot }\n\
         sequence: { steps: [plan, build, review] }\n",
    )
    .unwrap();
    assert_eq!(cfg.agent_names(), vec!["claude", "codex", "copilot"]);
}

/// The env overrides re-home onto the new shape (§4.1): `AGG_MODEL` → `defaults.model`,
/// `AGG_COST_TOTAL` → `sequence.limits.cost`, `AGG_TOKEN_BUDGET` → `sequence.limits.tokens`.
/// (Serial: mutates process env.)
#[test]
fn env_overrides_land_on_the_new_shape() {
    // guard against parallel env races by scoping tightly and restoring.
    std::env::set_var("AGG_MODEL", "haiku-from-env");
    std::env::set_var("AGG_COST_TOTAL", "12.5");
    std::env::set_var("AGG_TOKEN_BUDGET", "700000");
    let mut cfg = parse(MINIMAL).unwrap();
    cfg.apply_env_overrides();
    std::env::remove_var("AGG_MODEL");
    std::env::remove_var("AGG_COST_TOTAL");
    std::env::remove_var("AGG_TOKEN_BUDGET");
    assert_eq!(cfg.defaults.model.as_deref(), Some("haiku-from-env"));
    assert_eq!(cfg.sequence.limits.cost, Some(12.5));
    assert_eq!(cfg.sequence.limits.tokens, Some(700_000));
}
