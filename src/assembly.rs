//! Building the engine + parsed sequence from config.

use std::path::Path;
use anyhow::Result;
use crate::core::config::{AggConfig, SeqStep};
use crate::core::engine::Engine;
use crate::core::model::Judge;
use crate::core::stop;
use crate::problem::{self, Problem};

/// The engine + the sequence entries, assembled from config. Built once, before the loop (and by
/// `agg plan`).
pub struct Assembly {
    pub engine: Engine,
    pub steps: Vec<SeqStep>,
}

/// A refused [`build`]: EVERY problem found, plus the judges that did resolve — so `agg check` can
/// still run the capability checks over them and report those problems in the same pass.
pub struct Refused {
    pub problems: Vec<Problem>,
    pub judges: Vec<Judge>,
}

/// Build the run-set engine + validate the sequence from `cfg` (§5.3/§5.4). Refuses at startup:
/// an unknown step name, an all-`skip_judges` sequence (nothing could ever merge), an unbounded or
/// zero repetition, a judge name that resolves to no file, or a ratchet that cannot mean anything.
///
/// The startup face of [`build`]: every problem in ONE error (a lone problem reads exactly as it
/// did when the first error won).
///
/// # The DRIVER variant (BUILD.md §3.6)
///
/// An EMPTY `sequence.steps` **is** a Rust-driver project: the flow lives in the driver's own
/// `for`/`if`, so there is no entry list to validate and no `done_if` for agg to evaluate. Every
/// refusal below is about the entry list, so with no entries there is nothing to refuse — and the
/// all-`skip_judges` guard would otherwise reject every driver project outright.
/// The resulting run-set is EMPTY (nothing is declared to agg on that path: no `abort_if`, no
/// `notify_if`, no `invariants`), which is correct rather than a gap — driver judges are LAZY and
/// the driver asks for them by hand.
pub fn assemble(cfg: &AggConfig, config_base: &Path) -> Result<Assembly> {
    build(cfg, config_base).map_err(|r| anyhow::anyhow!(problem::render(&r.problems)))
}

/// [`assemble`], collecting EVERY problem (each with its config path) instead of stopping at the
/// first. The one validation both `agg run` and `agg check` go through.
pub fn build(cfg: &AggConfig, config_base: &Path) -> std::result::Result<Assembly, Refused> {
    use crate::core::judges;
    use crate::core::model::Lifecycle;

    // the standard library must exist before we resolve names against it (§6.1).
    if let Err(e) = judges::ensure_library() {
        eprintln!("  ⚠ could not refresh ~/.agg/judges: {e}");
    }

    let entries = &cfg.sequence.steps;
    let driver = entries.is_empty();
    let mut problems: Vec<Problem> = Vec::new();

    for (i, e) in entries.iter().enumerate() {
        problems.extend(entry_problems(cfg, i, e));
    }
    // an all-`skip_judges` sequence never merges, so `done_if` can never fire (§5.7) — refuse.
    let has_judged = entries
        .iter()
        .any(|e| cfg.steps.get(&e.step).map(|b| !b.skip_judges).unwrap_or(false));
    if !driver && !has_judged {
        problems.push(Problem::new(
            "sequence.steps",
            "every step in the sequence is skip_judges — nothing can ever merge and done_if can \
             never fire (§5.7). At least one judged step is required.",
        ));
    }

    let named = run_set(cfg, driver, &mut problems);

    // resolve every run-set name to a judge FILE (§5.1) — a name with no file is a startup error,
    // reported at the key that NAMED it.
    let mut judges_vec: Vec<Judge> = Vec::with_capacity(named.len());
    let mut unresolved: Vec<&str> = Vec::new();
    for n in &named {
        match judges::resolve(&n.name, config_base) {
            Ok(kind) => judges_vec.push(Judge {
                name: n.name.clone(),
                kind,
                invariant: cfg.sequence.invariants.contains(&n.name),
                in_dod: n.in_dod,
                // the `judges:` block's only key; absent ⇒ the run-level `judge.timeout` (300s).
                timeout: cfg.judges.get(&n.name).and_then(|j| j.timeout),
                state: Lifecycle::Pending,
                last_verdict: None,
                ever_met: false,
            }),
            Err(e) => {
                unresolved.push(&n.name);
                problems.push(Problem::new(n.path.clone(), format!("{e:#}")));
            }
        }
    }

    // `notify_if` with nothing to deliver is a silent no-op — the loop would detect and tell nobody.
    // Refuse it loudly here (`notify:` ALONE is fine: that is the "stop + notify" policy, §12.7).
    if cfg.sequence.notify_if.is_some()
        && cfg.sequence.notify.as_ref().map(|n| n.cmd.is_empty()).unwrap_or(true)
    {
        problems.push(Problem::new(
            "sequence.notify_if",
            // the suggested command is BOUNDED: delivery is foreground and untimed, and this string is
            // read at exactly the moment the user is about to write their first notify.cmd.
            "sequence.notify_if is set but sequence.notify.cmd is empty — nothing would fire. Add a \
             delivery command (e.g. notify: { cmd: [\"curl -s --max-time 10 -d {{reason}} \
             ntfy.sh/my-topic\"] }) or remove notify_if.",
        ));
    }

    // The expressions themselves — the checks `Engine::new` makes, one per key so each problem is
    // located. An expression naming an unresolved judge is skipped: its one real problem (the
    // missing file) is already listed, and "unknown identifier" would only repeat it.
    let done_if = (!driver).then(|| cfg.sequence.done_if.clone());
    let gate = crate::core::ratchet::Gate::new(cfg.sequence.ratchet.clone(), cfg.sequence.oscillation_window);
    for (path, expr) in [
        ("sequence.done_if", done_if.as_ref()),
        ("sequence.abort_if", cfg.sequence.abort_if.as_ref()),
        ("sequence.notify_if", cfg.sequence.notify_if.as_ref()),
    ] {
        let Some(expr) = expr else { continue };
        let names = stop::judge_names(expr).unwrap_or_default();
        if names.iter().any(|n| unresolved.contains(&n.as_str())) {
            continue;
        }
        if let Err(e) = stop::validate_with(expr, &judges_vec, Some(&gate)) {
            problem::push_unique(&mut problems, Problem::new(path, format!("{e:#}")));
        }
    }

    if !problems.is_empty() {
        return Err(Refused { problems, judges: judges_vec });
    }
    match Engine::with_gate(judges_vec, done_if, cfg.sequence.abort_if.clone(), cfg.sequence.notify_if.clone(), gate) {
        Ok(engine) => Ok(Assembly { engine, steps: cfg.sequence.steps.clone() }),
        Err(e) => Err(Refused { problems: vec![Problem::new("sequence", format!("{e:#}"))], judges: vec![] }),
    }
}

/// The refusals for ONE `sequence.steps` entry: an unknown step name, and a repetition that is
/// unbounded, zero, or bounded twice.
fn entry_problems(cfg: &AggConfig, i: usize, e: &SeqStep) -> Vec<Problem> {
    let at = format!("sequence.steps[{i}]");
    let mut out = Vec::new();
    // every referenced step name must be a key in `steps:` (§5.4).
    if !cfg.steps.contains_key(&e.step) {
        let defined: Vec<&str> = cfg.steps.keys().map(String::as_str).collect();
        out.push(Problem::new(
            at.clone(),
            format!("sequence references unknown step `{}` — defined steps: {}", e.step, defined.join(", ")),
        ));
    }
    // the repetition must be BOUNDED and non-zero, or the walk either spins forever on one
    // entry or dispatches nothing at all. `max` is required with `until` (§11.1): an unbounded
    // `until` in a config file is a run that cannot be reasoned about, and `limits:` is a
    // RUN-level ceiling, not a per-entry one.
    if e.times.is_some() && e.until.is_some() {
        out.push(Problem::new(
            at.clone(),
            format!(
                "step `{}`: `times:` and `until:`+`max:` are alternatives — a fixed count and a \
                 condition cannot both bound one entry.",
                e.step
            ),
        ));
    }
    if e.until.is_some() && e.max.is_none() {
        out.push(Problem::new(
            format!("{at}.until"),
            format!("step `{}`: `until:` requires `max:` — an unbounded repetition is not expressible in YAML.", e.step),
        ));
    }
    if e.max.is_some() && e.until.is_none() {
        out.push(Problem::new(
            format!("{at}.max"),
            format!("step `{}`: `max:` without `until:` bounds nothing — use `times:` for a fixed count.", e.step),
        ));
    }
    for (key, n) in [("times", e.times), ("max", e.max)] {
        if n == Some(0) {
            out.push(Problem::new(
                format!("{at}.{key}"),
                format!("step `{}`: `{key}: 0` dispatches nothing — it must be >= 1.", e.step),
            ));
        }
    }
    out
}

/// One run-set member: the judge NAME, the config key that first named it, and DoD membership.
struct Named {
    name: String,
    path: String,
    in_dod: bool,
}

/// The run-set, in run-set order, each name with the key that first named it.
///
/// DoD-set = done_if ∪ invariants; run-set = DoD ∪ abort_if ∪ notify_if ∪ every `until:` (§5.3).
/// On the driver path there is no DoD at all — agg is never told what "done" means, so it must
/// not claim success. `Sequence::done_if` still carries its YAML default; it is simply not read.
///
/// `notify_if` joins the RUN-SET on exactly the same terms as `abort_if` (STUCK_NOTIFY §12.1): its
/// detectors must EXECUTE each step, but they are machinery, not goals — never in the DoD-set, so
/// `all_goals` can't be blocked by `stuck` and the regression gate (scoped to `in_dod`) can't roll
/// a session back because a detector flipped. An `until:` expression joins on the same terms: its
/// judges must execute every step or the condition could never become true.
fn run_set(cfg: &AggConfig, driver: bool, problems: &mut Vec<Problem>) -> Vec<Named> {
    let seq = &cfg.sequence;
    let mut out: Vec<Named> = Vec::new();
    if !driver {
        add_names(&mut out, problems, "sequence.done_if".into(), &seq.done_if, true);
    }
    for (i, inv) in seq.invariants.iter().enumerate() {
        add_name(&mut out, inv, format!("sequence.invariants[{i}]"), true);
    }
    if let Some(a) = &seq.abort_if {
        add_names(&mut out, problems, "sequence.abort_if".into(), a, false);
    }
    if let Some(n) = &seq.notify_if {
        add_names(&mut out, problems, "sequence.notify_if".into(), n, false);
    }
    for (i, e) in seq.steps.iter().enumerate() {
        if let Some(c) = &e.until {
            add_names(&mut out, problems, format!("sequence.steps[{i}].until"), c, false);
        }
    }
    // A RATCHETED judge joins the run-set on the same terms as an `until:` judge: it must EXECUTE
    // every judged step or there is no value to hold to its reference — and it stays OUT of the
    // DoD-set, because the ratchet is a gate rule, not a goal. On the driver path judges are lazy
    // and named by the driver, so the ratchet is kept but nothing joins a run-set.
    for (name, spec) in &seq.ratchet {
        if let Err(e) = spec.validate(name) {
            problems.push(Problem::new(format!("sequence.ratchet.{name}"), format!("{e:#}")));
        }
        if !driver {
            add_name(&mut out, name, format!("sequence.ratchet.{name}"), false);
        }
    }
    if seq.oscillation_window < 2 {
        problems.push(Problem::new(
            "sequence.oscillation_window",
            format!(
                "sequence.oscillation_window must be >= 2 — an A→B→A repeat needs two earlier sessions to look back over, got {}",
                seq.oscillation_window
            ),
        ));
    }
    out
}

/// Add every judge an expression names; an expression that does not even tokenize is a problem.
fn add_names(out: &mut Vec<Named>, problems: &mut Vec<Problem>, path: String, expr: &str, dod: bool) {
    match stop::judge_names(expr) {
        Ok(names) => names.iter().for_each(|n| add_name(out, n, path.clone(), dod)),
        Err(e) => problems.push(Problem::new(path, format!("{e:#}"))),
    }
}

/// Add one name, keeping the FIRST key that named it; DoD membership accumulates.
fn add_name(out: &mut Vec<Named>, name: &str, path: String, dod: bool) {
    match out.iter_mut().find(|x| x.name == name) {
        Some(x) => x.in_dod |= dod,
        None => out.push(Named { name: name.to_string(), path, in_dod: dod }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway config_base (the `agg/` folder) whose `judges/` holds one script per name. Only
    /// the FILE's existence matters — `assemble` resolves names to files, it never runs them.
    fn base_with_judges(names: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("judges");
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            std::fs::write(dir.join(format!("{n}.sh")), "#!/bin/sh\necho '{\"met\":true}'\n").unwrap();
        }
        tmp
    }

    /// The smallest assemblable config, plus `seq_extra` lines under `sequence:` (each already
    /// 2-space indented and newline-terminated). `done_if` is always the judge `goal`.
    fn cfg_with(seq_extra: &str) -> AggConfig {
        let body = format!(
            "project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: [worker]\n  done_if: \"goal\"\n{seq_extra}"
        );
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("test config must parse: {e}\n--- body ---\n{body}"))
    }

    /// `cfg_with`'s twin for the entry shape: `seq_steps` is the whole `sequence.steps` list as it
    /// would be written inline in YAML. One judged step (`worker`) is always defined.
    fn cfg_with_steps(seq_steps: &str) -> AggConfig {
        let body = format!(
            "project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: {seq_steps}\n  done_if: \"goal\"\n"
        );
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("test config must parse: {e}\n--- body ---\n{body}"))
    }

    /// The message of an `assemble` that must FAIL. Hand-rolled because `Assembly` is deliberately
    /// not `Debug` (it holds judge state), so `Result::unwrap_err` is unavailable here.
    fn assemble_err(cfg: &AggConfig, base: &Path) -> String {
        match assemble(cfg, base) {
            Ok(_) => panic!("assemble was expected to REFUSE this config, but it succeeded"),
            Err(e) => format!("{e:#}"),
        }
    }

    /// THE PORTED REFUSAL (BUILD.md §4 row 12, RUST_API §11.3(2)). An all-`skip_judges` sequence can
    /// never merge, so `done_if` can never fire — and the failure is INVISIBLE until the ceiling
    /// fires hours later, which is why this stays a STARTUP check. It now reads the serde entry
    /// structs (`e.step`) instead of a parsed statement list; the fixture is all that changed.
    #[test]
    fn an_all_skip_judges_sequence_is_refused_at_startup() {
        let tmp = base_with_judges(&["goal"]);
        // the new shape, in both spellings: the mapping form and the scalar shorthand.
        for steps in ["[{ step: stage }]", "[stage]"] {
            let body = format!(
                "project: p\nsteps: {{ stage: {{ skip_judges: true }} }}\nsequence:\n  steps: {steps}\n  done_if: \"goal\"\n"
            );
            let cfg: AggConfig = serde_yaml::from_str(&body).expect("the fixture must parse");
            let err = assemble_err(&cfg, tmp.path());
            assert!(
                err.contains("skip_judges") && err.contains("never fire"),
                "an all-skip_judges sequence must be refused at STARTUP, got: {err}"
            );
        }
    }

    /// The other half of the same rule: ONE judged step is enough, whatever the entry looks like.
    /// A repeated (`times:`) or bounded (`until:`+`max:`) entry is still a judged step.
    #[test]
    fn one_judged_step_anywhere_in_the_sequence_is_enough() {
        let tmp = base_with_judges(&["goal"]);
        let body = "project: p\nsteps: { stage: { skip_judges: true }, worker: {} }\nsequence:\n  \
                    steps: [{ step: stage }, { step: worker, until: goal, max: 3 }]\n  done_if: \"goal\"\n";
        let cfg: AggConfig = serde_yaml::from_str(body).expect("the fixture must parse");
        assemble(&cfg, tmp.path()).expect("one judged step must be enough");
    }

    /// BUILD.md §3.6 / §12.6: an EMPTY `sequence:` is a DRIVER project — it starts, and NEITHER
    /// startup refusal fires (the all-`skip_judges` one would otherwise reject every driver project,
    /// which has no `steps:` at all).
    #[test]
    fn an_empty_sequence_assembles_and_neither_refusal_fires() {
        let tmp = base_with_judges(&["goal"]);
        for body in ["project: p\n", "project: p\nsequence: { steps: [] }\n"] {
            let cfg: AggConfig = serde_yaml::from_str(body).expect("an empty sequence must parse");
            let asm = assemble(&cfg, tmp.path()).expect("an empty sequence is a driver project and must assemble");
            assert!(asm.engine.judges.is_empty(), "a driver project declares nothing to agg — the run-set is EMPTY");
        }
    }

    /// A name in an entry that is not a key in `steps:` is the same hard startup error it always was
    /// (§5.4) — the check moved from a statement's `step_names()` to the one `e.step` field, not away.
    #[test]
    fn an_entry_naming_an_unknown_step_is_refused() {
        let tmp = base_with_judges(&["goal"]);
        let cfg = cfg_with_steps("[worker, { step: ghost }]");
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.contains("ghost"), "the refusal must NAME the unknown step, got: {err}");
    }

    /// The bounds that keep the walk finite. `until:` without `max:` is an unbounded repetition in a
    /// config file (§11.1: "`max` is required"), and a zero count dispatches nothing — the old
    /// parser refused `worker x0` for the same reason.
    #[test]
    fn the_repetition_bounds_are_validated_at_startup() {
        let tmp = base_with_judges(&["goal"]);
        for (steps, needle) in [
            ("[{ step: worker, until: goal }]", "max"),
            ("[{ step: worker, times: 0 }]", ">= 1"),
            ("[{ step: worker, until: goal, max: 0 }]", ">= 1"),
            ("[{ step: worker, times: 2, until: goal, max: 3 }]", "alternatives"),
        ] {
            let err = assemble_err(&cfg_with_steps(steps), tmp.path());
            assert!(err.contains(needle), "`{steps}` must be refused mentioning `{needle}`, got: {err}");
        }
    }

    /// An `until:` expression is a RUN-SET member on exactly the terms the old `if` condition was:
    /// its judge must EXECUTE every step or the condition could never become true, and it must stay
    /// out of the DoD-set (it is machinery, not a goal).
    #[test]
    fn an_until_condition_judge_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_judges(&["goal", "converged"]);
        let cfg = cfg_with_steps("[{ step: worker, until: \"converged.value >= 85\", max: 3 }]");
        let asm = assemble(&cfg, tmp.path()).expect("an until-condition judge must assemble");
        let c = asm
            .engine
            .judges
            .iter()
            .find(|g| g.name == "converged")
            .expect("`converged` must be in the run-set — it has to EXECUTE or `until:` can never fire");
        assert!(!c.in_dod, "an until-condition judge is machinery, not a goal");
        assert_eq!(asm.engine.tally(), (0, 1), "the scoreboard counts the goal only");
    }

    /// THE §12.1 REGRESSION — the gap that would have made the whole feature dead on arrival. A judge
    /// named ONLY in `notify_if` (in no `done_if`, no `invariants`, no `abort_if`, no `if` condition)
    /// must join the RUN-SET and must stay OUT of the DoD-set. The two halves fail in opposite
    /// directions:
    ///
    /// * missing from the run-set ⇒ the name resolves to no judge file, so the `stop::validate` call
    ///   in `Engine::new` rejects the expression as an unknown identifier and every `agg
    ///   run`/`plan`/`doctor` dies at startup — you could never USE `notify_if` at all;
    /// * present but `in_dod` ⇒ a detector silently becomes a GOAL: it would hold `all_goals` down,
    ///   inflate the N/M scoreboard, and hand `GateKeepRollback`'s regression check (scoped to
    ///   `in_dod`) a reason to roll a good session back the moment the detector flipped.
    #[test]
    fn a_notify_if_only_judge_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_judges(&["goal", "detector"]);
        let cfg = cfg_with("  notify_if: \"detector.value >= 85\"\n  notify: { cmd: [\"true\"] }\n");
        let asm = assemble(&cfg, tmp.path()).expect("a notify_if-only judge must assemble");

        let detector = asm
            .engine
            .judges
            .iter()
            .find(|g| g.name == "detector")
            .expect("`detector` must be in the run-set — it has to EXECUTE each step or notify_if can never be true");
        assert!(!detector.in_dod, "a detector is machinery, not a goal — it must stay out of the DoD-set");
        // …and the DoD-set is not merely empty: `goal` IS in it, so `detector` is the one exclusion.
        assert!(
            asm.engine.judges.iter().any(|g| g.name == "goal" && g.in_dod),
            "the done_if judge must still be in the DoD-set"
        );
        assert_eq!(asm.engine.tally(), (0, 1), "the N/M scoreboard must count the goal only, never the detector");
    }

    /// A `notify_if` name that resolves to no file is the same hard startup error as a bad `done_if`
    /// name. Run-set membership (§12.1) buys `notify_if` the validation — it must not buy it a pass.
    #[test]
    fn a_notify_if_judge_with_no_file_is_a_startup_error_naming_it() {
        let tmp = base_with_judges(&["goal"]);
        let cfg = cfg_with("  notify_if: \"ghost_detector\"\n  notify: { cmd: [\"true\"] }\n");
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.contains("ghost_detector"), "the error must NAME the unresolvable judge, got: {err}");
    }

    /// A RATCHETED judge must EXECUTE every judged step (or there is nothing to hold to its
    /// reference), so it joins the run-set — and it is a gate rule, not a goal, so it stays out of
    /// the DoD-set. `.ref` is accepted on it and refused on anything else, at startup.
    #[test]
    fn a_ratcheted_judge_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_judges(&["goal", "cc"]);
        let cfg = cfg_with("  ratchet: { cc: { direction: down, tolerance: 0.5 } }\n  abort_if: \"cc.ref > 100 OR rollbacks >= 2\"\n");
        let asm = assemble(&cfg, tmp.path()).expect("a ratcheted judge must assemble");
        let cc = asm.engine.judges.iter().find(|g| g.name == "cc").expect("`cc` must be in the run-set");
        assert!(!cc.in_dod, "a ratchet is a gate rule, not a goal");
        assert!(asm.engine.gate.is_ratcheted("cc"));
        assert_eq!(asm.engine.tally(), (0, 1));

        let err = assemble_err(&cfg_with("  abort_if: \"goal.ref > 1\"\n"), tmp.path());
        assert!(err.contains("has no ratchet"), "`.ref` on an unratcheted judge is a startup error: {err}");
    }

    /// The refusals: a ratchet naming no judge file, a meaningless tolerance, a missing direction,
    /// an unknown key, and an oscillation window too short to hold A→B→A.
    #[test]
    fn a_ratchet_that_cannot_mean_anything_is_refused_at_startup() {
        let tmp = base_with_judges(&["goal", "cc"]);
        let err = assemble_err(&cfg_with("  ratchet: { ghost: { direction: up } }\n"), tmp.path());
        assert!(err.contains("ghost"), "{err}");
        let err = assemble_err(&cfg_with("  ratchet: { cc: { direction: up, tolerance: -1 } }\n"), tmp.path());
        assert!(err.contains("tolerance"), "{err}");
        let err = assemble_err(&cfg_with("  oscillation_window: 1\n"), tmp.path());
        assert!(err.contains("oscillation_window"), "{err}");
        for bad in ["  ratchet: { cc: { tolerance: 1 } }\n", "  ratchet: { cc: { direction: up, best: true } }\n"] {
            let body = format!("project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: [worker]\n  done_if: \"goal\"\n{bad}");
            assert!(serde_yaml::from_str::<AggConfig>(&body).is_err(), "deny_unknown_fields / required direction: {bad}");
        }
    }

    /// The §12.7 validity matrix, all four cells. Row 3 is the trap: `notify:` ALONE is not a
    /// mistake, it is the entire "stop + notify" policy (§8.5 — ping only when `abort_if` halts), so
    /// a symmetrical "each key requires the other" check would silently delete a shipped feature.
    #[test]
    fn the_notify_validity_matrix_covers_all_four_rows() {
        let tmp = base_with_judges(&["goal", "detector"]);
        let base = tmp.path();

        // row 1 — notify_if + non-empty notify.cmd: the notify ladder.
        assemble(&cfg_with("  notify_if: \"detector\"\n  notify: { cmd: [\"true\"] }\n"), base)
            .expect("row 1: notify_if with a delivery command is the supported shape");

        // row 2 — notify_if with nothing to deliver: a hard error naming BOTH halves, so the operator
        // knows which one to fix. `detector` RESOLVES here, which is what proves this is the notify
        // check firing rather than judge resolution failing first.
        for cell in ["", "  notify: { cmd: [] }\n"] {
            let err = assemble_err(&cfg_with(&format!("  notify_if: \"detector\"\n{cell}")), base);
            assert!(
                err.contains("notify_if") && err.contains("notify.cmd"),
                "the refusal must name both halves of the broken pair, got: {err}"
            );
        }

        // row 3 — `notify:` alone: VALID, and the whole point of §8.5.
        assemble(&cfg_with("  notify: { cmd: [\"true\"] }\n"), base)
            .expect("row 3: `notify:` without `notify_if` is the stop+notify policy and must load");

        // row 4 — neither key: today's pure autonomy, untouched by the feature.
        assemble(&cfg_with(""), base).expect("row 4: a config naming neither key must be unaffected");
    }

    /// The paths of a REFUSED build — hand-rolled for the same reason as `assemble_err`.
    fn refused_paths(cfg: &AggConfig, base: &Path) -> Vec<String> {
        match build(cfg, base) {
            Ok(_) => panic!("build was expected to REFUSE this config"),
            Err(r) => r.problems.into_iter().map(|p| p.path).collect(),
        }
    }

    /// `build` no longer stops at the first problem: every entry, every unresolved judge and the
    /// notify pair are all reported, each at the key an operator would edit.
    #[test]
    fn build_collects_every_problem_with_its_path() {
        let tmp = base_with_judges(&["goal"]);
        let body = "project: p\nsteps: { worker: {} }\nsequence:\n  steps: [ghost, { step: worker, times: 0 }, \
                    { step: worker, max: 2 }]\n  done_if: \"goal AND missing\"\n  invariants: [lost]\n  \
                    abort_if: \"(\"\n  notify_if: \"goal\"\n";
        let cfg: AggConfig = serde_yaml::from_str(body).expect("the fixture must parse");
        assert_eq!(
            refused_paths(&cfg, tmp.path()),
            [
                "sequence.steps[0]",
                "sequence.steps[1].times",
                "sequence.steps[2].max",
                "sequence.done_if",
                "sequence.invariants[0]",
                "sequence.notify_if",
                "sequence.abort_if",
            ]
        );
    }

    /// Several problems render as one numbered startup error; the judges that DID resolve survive
    /// the refusal so the capability check can still look at them.
    #[test]
    fn a_refusal_keeps_the_resolved_judges_and_renders_every_problem() {
        let tmp = base_with_judges(&["goal"]);
        let cfg = cfg_with_steps("[ghost, { step: worker, times: 0 }]");
        match build(&cfg, tmp.path()) {
            Ok(_) => panic!("must refuse"),
            Err(r) => assert_eq!(r.judges.iter().map(|j| j.name.as_str()).collect::<Vec<_>>(), ["goal"]),
        }
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.starts_with("2 problems") && err.contains("ghost") && err.contains(">= 1"), "{err}");
    }

    /// A bad EXPRESSION over resolved judges is located at its key (`Engine::new`'s check, split).
    #[test]
    fn an_invalid_condition_is_located_at_its_key() {
        let tmp = base_with_judges(&["goal"]);
        let cfg = cfg_with("  abort_if: \"goal >= \"\n");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.abort_if"]);
    }
}
