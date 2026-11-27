//! Building the engine + parsed sequence from config.

use std::path::Path;
use anyhow::Result;
use crate::core::config::{RelentlessConfig, SeqStep};
use crate::core::engine::Engine;
use crate::core::model::Verifier;
use crate::core::stop;
use crate::problem::{self, Problem};

/// The engine + the sequence entries, assembled from config. Built once, before the loop (and by
/// `rlt plan`).
pub struct Assembly {
    pub engine: Engine,
    pub steps: Vec<SeqStep>,
}

/// A refused [`build`]: EVERY problem found, plus the verifiers that did resolve — so `rlt check` can
/// still run the capability checks over them and report those problems in the same pass.
pub struct Refused {
    pub problems: Vec<Problem>,
    pub verifiers: Vec<Verifier>,
}

/// Build the run-set engine + validate the sequence from `cfg` (§5.3/§5.4). Refuses at startup:
/// an unknown step name, an all-`skip_verifiers` sequence (nothing could ever merge), an unbounded or
/// zero repetition, a verifier name that resolves to no file, or a ratchet that cannot mean anything.
///
/// The startup face of [`build`]: every problem in ONE error (a lone problem reads exactly as it
/// did when the first error won).
///
/// # The DRIVER variant (BUILD.md §3.6)
///
/// An EMPTY `sequence.steps` **is** a Rust-driver project: the flow lives in the driver's own
/// `for`/`if`, so there is no entry list to validate and no `done_if` for rlt to evaluate. Every
/// refusal below is about the entry list, so with no entries there is nothing to refuse — and the
/// all-`skip_verifiers` guard would otherwise reject every driver project outright.
/// The resulting run-set is EMPTY (nothing is declared to rlt on that path: no `abort_if`, no
/// `notify_if`, no `invariants`), which is correct rather than a gap — driver verifiers are LAZY and
/// the driver asks for them by hand.
pub fn assemble(cfg: &RelentlessConfig, config_base: &Path) -> Result<Assembly> {
    build(cfg, config_base).map_err(|r| anyhow::anyhow!(problem::render(&r.problems)))
}

/// [`assemble`], collecting EVERY problem (each with its config path) instead of stopping at the
/// first. The one validation both `rlt run` and `rlt check` go through.
pub fn build(cfg: &RelentlessConfig, config_base: &Path) -> std::result::Result<Assembly, Refused> {
    use crate::core::verifiers;
    use crate::core::model::Lifecycle;

    // the standard library must exist before we resolve names against it (§6.1).
    if let Err(e) = verifiers::ensure_library() {
        eprintln!("  ⚠ could not refresh ~/.rlt/verifiers: {e}");
    }

    let entries = &cfg.sequence.steps;
    let driver = entries.is_empty();
    let mut problems: Vec<Problem> = Vec::new();

    for (i, e) in entries.iter().enumerate() {
        problems.extend(entry_problems(cfg, i, e));
    }
    // an all-`skip_verifiers` sequence never merges, so `done_if` can never fire (§5.7) — refuse.
    let has_verified = entries
        .iter()
        .any(|e| cfg.steps.get(&e.step).map(|b| !b.skip_verifiers).unwrap_or(false));
    if !driver && !has_verified {
        problems.push(Problem::new(
            "sequence.steps",
            "every step in the sequence is skip_verifiers — nothing can ever merge and done_if can \
             never fire (§5.7). At least one verified step is required.",
        ));
    }

    // `prompt_file` / `role_prompt_file`: one form per key, and a file that can be read.
    problems.extend(crate::core::prompts::problems(cfg, config_base));

    let named = run_set(cfg, driver, &mut problems);

    // the verifiers DECLARED in `verifiers:` (a `metric:`/`score:` block, `crate::metrics`) — and the
    // ratchets whose direction such a verifier states. swe-metrics is probed only if one is in the run.
    let names: Vec<String> = named.iter().map(|n| n.name.clone()).collect();
    let declared = crate::metrics::declare::declare(cfg, &names, config_base, true);
    problems.extend(declared.problems);

    // resolve every run-set name to a declared verifier or a verifier FILE (§5.1) — a name with neither
    // is a startup error, reported at the key that NAMED it.
    let mut verifiers_vec: Vec<Verifier> = Vec::with_capacity(named.len());
    let mut unresolved: Vec<&str> = Vec::new();
    for n in &named {
        let kind = match declared.kinds.get(&n.name) {
            Some(k) => Ok(k.clone()),
            None => verifiers::resolve(&n.name, config_base),
        };
        match kind {
            Ok(kind) => verifiers_vec.push(Verifier {
                name: n.name.clone(),
                kind,
                invariant: cfg.sequence.invariants.contains(&n.name),
                in_dod: n.in_dod,
                // the `verifiers:` block's `timeout:`; absent ⇒ the run-level `verifier.timeout` (300s).
                timeout: cfg.verifiers.get(&n.name).and_then(|j| j.timeout),
                ruler: None,
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
    // located. An expression naming an unresolved verifier is skipped: its one real problem (the
    // missing file) is already listed, and "unknown identifier" would only repeat it.
    let done_if = (!driver).then(|| cfg.sequence.done_if.clone());
    let gate = crate::core::ratchet::Gate::new(declared.ratchets, cfg.sequence.oscillation_window).with_bases(declared.bases);
    let groups = groups_for(cfg, done_if.as_deref());

    // `merge_requires_improvement` with nothing that COULD improve — no DoD-set verifier, no ratchet —
    // would never merge a session. Refused like the all-`skip_verifiers` sequence, for the same reason.
    if !driver
        && cfg.sequence.merge_requires_improvement
        && cfg.sequence.ratchet.is_empty()
        && !named.iter().any(|n| n.in_dod)
    {
        problems.push(Problem::new(
            "sequence.merge_requires_improvement",
            "sequence.merge_requires_improvement is true but there is no DoD-set verifier and no ratchet — \
             nothing could ever improve, so no session would ever merge. Name a verifier in done_if or \
             invariants, add a ratchet, or drop the key.",
        ));
    }

    for (path, expr) in [
        ("sequence.done_if", done_if.as_ref()),
        ("sequence.abort_if", cfg.sequence.abort_if.as_ref()),
        ("sequence.notify_if", cfg.sequence.notify_if.as_ref()),
    ] {
        let Some(expr) = expr else { continue };
        let names = stop::verifier_names(expr).unwrap_or_default();
        if names.iter().any(|n| unresolved.contains(&n.as_str())) {
            continue;
        }
        if let Err(e) = stop::validate_with(expr, &verifiers_vec, Some(&gate), &groups) {
            problem::push_unique(&mut problems, Problem::new(path, format!("{e:#}")));
        }
    }

    if !problems.is_empty() {
        return Err(Refused { problems, verifiers: verifiers_vec });
    }
    match Engine::with_gate(verifiers_vec, done_if, cfg.sequence.abort_if.clone(), cfg.sequence.notify_if.clone(), gate, groups) {
        Ok(engine) => Ok(Assembly { engine, steps: cfg.sequence.steps.clone() }),
        Err(e) => Err(Refused { problems: vec![Problem::new("sequence", format!("{e:#}"))], verifiers: vec![] }),
    }
}

/// The configured groups, with what the gate needs: the groups `done_if` names (verified as a unit)
/// and the verifiers it holds one by one (named bare in `done_if`, or invariants).
fn groups_for(cfg: &RelentlessConfig, done_if: Option<&str>) -> crate::core::groups::Groups {
    let mut g = crate::core::groups::Groups::from_map(cfg.sequence.groups.clone());
    if let Some(d) = done_if {
        g.gated = stop::subsets(d).unwrap_or_default().into_iter().filter(|s| g.map.contains_key(s)).collect();
        g.direct = stop::verifier_names(d).unwrap_or_default().into_iter().collect();
    }
    g.direct.extend(cfg.sequence.invariants.iter().cloned());
    g
}

/// The refusals for ONE `sequence.steps` entry: an unknown step name, and a repetition that is
/// unbounded, zero, or bounded twice.
fn entry_problems(cfg: &RelentlessConfig, i: usize, e: &SeqStep) -> Vec<Problem> {
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

/// One run-set member: the verifier NAME, the config key that first named it, and DoD membership.
struct Named {
    name: String,
    path: String,
    in_dod: bool,
}

/// The run-set, in run-set order, each name with the key that first named it.
///
/// DoD-set = done_if ∪ invariants; run-set = DoD ∪ abort_if ∪ notify_if ∪ every `until:` ∪ every
/// ratchet key ∪ every group member (§5.3). A group an expression uses as a subset contributes its
/// members on that expression's terms — so a group in `done_if` is in the DoD-set.
/// On the driver path there is no DoD at all — rlt is never told what "done" means, so it must
/// not claim success. `Sequence::done_if` still carries its YAML default; it is simply not read.
///
/// `notify_if` joins the RUN-SET on exactly the same terms as `abort_if` (STUCK_NOTIFY §12.1): its
/// detectors must EXECUTE each step, but they are machinery, not goals — never in the DoD-set, so
/// `all_goals` can't be blocked by `stuck` and the regression gate (scoped to `in_dod`) can't roll
/// a session back because a detector flipped. An `until:` expression joins on the same terms: its
/// verifiers must execute every step or the condition could never become true.
fn run_set(cfg: &RelentlessConfig, driver: bool, problems: &mut Vec<Problem>) -> Vec<Named> {
    let seq = &cfg.sequence;
    let mut out: Vec<Named> = Vec::new();
    if !driver {
        add_names(&mut out, problems, "sequence.done_if".into(), &seq.done_if, true, &seq.groups);
    }
    for (i, inv) in seq.invariants.iter().enumerate() {
        add_name(&mut out, inv, format!("sequence.invariants[{i}]"), true);
    }
    if let Some(a) = &seq.abort_if {
        add_names(&mut out, problems, "sequence.abort_if".into(), a, false, &seq.groups);
    }
    if let Some(n) = &seq.notify_if {
        add_names(&mut out, problems, "sequence.notify_if".into(), n, false, &seq.groups);
    }
    for (i, e) in seq.steps.iter().enumerate() {
        if let Some(c) = &e.until {
            add_names(&mut out, problems, format!("sequence.steps[{i}].until"), c, false, &seq.groups);
        }
    }
    // A RATCHETED verifier joins the run-set on the same terms as an `until:` verifier: it must EXECUTE
    // every verified step or there is no value to hold to its reference — and it stays OUT of the
    // DoD-set, because the ratchet is a gate rule, not a goal. On the driver path verifiers are lazy
    // and named by the driver, so the ratchet is kept but nothing joins a run-set.
    for (name, spec) in &seq.ratchet {
        if let Err(e) = spec.validate(name) {
            problems.push(Problem::new(format!("sequence.ratchet.{name}"), format!("{e:#}")));
        }
        if !driver {
            add_name(&mut out, name, format!("sequence.ratchet.{name}"), false);
        }
    }
    // Every member of every GROUP joins the run-set, named by a condition or not (a group in
    // `done_if` already put its members in the DoD-set above). A group that cannot mean anything —
    // a name the grammar cannot read, no members, a member twice — is refused at its own key.
    for (g, members) in &seq.groups {
        for (path, msg) in crate::core::groups::problems(g, members) {
            problems.push(Problem::new(path, msg));
        }
        if !driver {
            for (i, m) in members.iter().enumerate() {
                add_name(&mut out, m, format!("sequence.groups.{g}[{i}]"), false);
            }
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

/// Add every verifier an expression names — bare, or as a member of a group it uses as a subset (a
/// group is its members) — on the expression's DoD terms. An expression that does not even tokenize
/// is a problem; an unknown group is left to the expression check, which names the defined ones.
fn add_names(
    out: &mut Vec<Named>,
    problems: &mut Vec<Problem>,
    path: String,
    expr: &str,
    dod: bool,
    groups: &std::collections::BTreeMap<String, Vec<String>>,
) {
    match stop::verifier_names(expr) {
        Ok(names) => names.iter().for_each(|n| add_name(out, n, path.clone(), dod)),
        Err(e) => return problems.push(Problem::new(path, format!("{e:#}"))),
    }
    for g in stop::subsets(expr).unwrap_or_default() {
        for (i, m) in groups.get(&g).into_iter().flatten().enumerate() {
            add_name(out, m, format!("sequence.groups.{g}[{i}]"), dod);
        }
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

    /// A throwaway config_base (the `rlt/` folder) whose `verifiers/` holds one script per name. Only
    /// the FILE's existence matters — `assemble` resolves names to files, it never runs them.
    fn base_with_verifiers(names: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("verifiers");
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            std::fs::write(dir.join(format!("{n}.sh")), "#!/bin/sh\necho '{\"met\":true}'\n").unwrap();
        }
        tmp
    }

    /// The smallest assemblable config, plus `seq_extra` lines under `sequence:` (each already
    /// 2-space indented and newline-terminated). `done_if` is always the verifier `goal`.
    fn cfg_with(seq_extra: &str) -> RelentlessConfig {
        let body = format!(
            "project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: [worker]\n  done_if: \"goal\"\n{seq_extra}"
        );
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("test config must parse: {e}\n--- body ---\n{body}"))
    }

    /// `cfg_with`'s twin for the entry shape: `seq_steps` is the whole `sequence.steps` list as it
    /// would be written inline in YAML. One verified step (`worker`) is always defined.
    fn cfg_with_steps(seq_steps: &str) -> RelentlessConfig {
        let body = format!(
            "project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: {seq_steps}\n  done_if: \"goal\"\n"
        );
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("test config must parse: {e}\n--- body ---\n{body}"))
    }

    /// The message of an `assemble` that must FAIL. Hand-rolled because `Assembly` is deliberately
    /// not `Debug` (it holds verifier state), so `Result::unwrap_err` is unavailable here.
    fn assemble_err(cfg: &RelentlessConfig, base: &Path) -> String {
        match assemble(cfg, base) {
            Ok(_) => panic!("assemble was expected to REFUSE this config, but it succeeded"),
            Err(e) => format!("{e:#}"),
        }
    }

    /// THE PORTED REFUSAL (BUILD.md §4 row 12, RUST_API §11.3(2)). An all-`skip_verifiers` sequence can
    /// never merge, so `done_if` can never fire — and the failure is INVISIBLE until the ceiling
    /// fires hours later, which is why this stays a STARTUP check. It now reads the serde entry
    /// structs (`e.step`) instead of a parsed statement list; the fixture is all that changed.
    #[test]
    fn an_all_skip_verifiers_sequence_is_refused_at_startup() {
        let tmp = base_with_verifiers(&["goal"]);
        // the new shape, in both spellings: the mapping form and the scalar shorthand.
        for steps in ["[{ step: stage }]", "[stage]"] {
            let body = format!(
                "project: p\nsteps: {{ stage: {{ skip_verifiers: true }} }}\nsequence:\n  steps: {steps}\n  done_if: \"goal\"\n"
            );
            let cfg: RelentlessConfig = serde_yaml::from_str(&body).expect("the fixture must parse");
            let err = assemble_err(&cfg, tmp.path());
            assert!(
                err.contains("skip_verifiers") && err.contains("never fire"),
                "an all-skip_verifiers sequence must be refused at STARTUP, got: {err}"
            );
        }
    }

    /// The other half of the same rule: ONE verified step is enough, whatever the entry looks like.
    /// A repeated (`times:`) or bounded (`until:`+`max:`) entry is still a verified step.
    #[test]
    fn one_verified_step_anywhere_in_the_sequence_is_enough() {
        let tmp = base_with_verifiers(&["goal"]);
        let body = "project: p\nsteps: { stage: { skip_verifiers: true }, worker: {} }\nsequence:\n  \
                    steps: [{ step: stage }, { step: worker, until: goal, max: 3 }]\n  done_if: \"goal\"\n";
        let cfg: RelentlessConfig = serde_yaml::from_str(body).expect("the fixture must parse");
        assemble(&cfg, tmp.path()).expect("one verified step must be enough");
    }

    /// BUILD.md §3.6 / §12.6: an EMPTY `sequence:` is a DRIVER project — it starts, and NEITHER
    /// startup refusal fires (the all-`skip_verifiers` one would otherwise reject every driver project,
    /// which has no `steps:` at all).
    #[test]
    fn an_empty_sequence_assembles_and_neither_refusal_fires() {
        let tmp = base_with_verifiers(&["goal"]);
        for body in ["project: p\n", "project: p\nsequence: { steps: [] }\n"] {
            let cfg: RelentlessConfig = serde_yaml::from_str(body).expect("an empty sequence must parse");
            let asm = assemble(&cfg, tmp.path()).expect("an empty sequence is a driver project and must assemble");
            assert!(asm.engine.verifiers.is_empty(), "a driver project declares nothing to rlt — the run-set is EMPTY");
        }
    }

    /// A name in an entry that is not a key in `steps:` is the same hard startup error it always was
    /// (§5.4) — the check moved from a statement's `step_names()` to the one `e.step` field, not away.
    #[test]
    fn an_entry_naming_an_unknown_step_is_refused() {
        let tmp = base_with_verifiers(&["goal"]);
        let cfg = cfg_with_steps("[worker, { step: ghost }]");
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.contains("ghost"), "the refusal must NAME the unknown step, got: {err}");
    }

    /// The bounds that keep the walk finite. `until:` without `max:` is an unbounded repetition in a
    /// config file (§11.1: "`max` is required"), and a zero count dispatches nothing — the old
    /// parser refused `worker x0` for the same reason.
    #[test]
    fn the_repetition_bounds_are_validated_at_startup() {
        let tmp = base_with_verifiers(&["goal"]);
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
    /// its verifier must EXECUTE every step or the condition could never become true, and it must stay
    /// out of the DoD-set (it is machinery, not a goal).
    #[test]
    fn an_until_condition_verifier_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_verifiers(&["goal", "converged"]);
        let cfg = cfg_with_steps("[{ step: worker, until: \"converged.value >= 85\", max: 3 }]");
        let asm = assemble(&cfg, tmp.path()).expect("an until-condition verifier must assemble");
        let c = asm
            .engine
            .verifiers
            .iter()
            .find(|g| g.name == "converged")
            .expect("`converged` must be in the run-set — it has to EXECUTE or `until:` can never fire");
        assert!(!c.in_dod, "an until-condition verifier is machinery, not a goal");
        assert_eq!(asm.engine.tally(), (0, 1), "the scoreboard counts the goal only");
    }

    /// THE §12.1 REGRESSION — the gap that would have made the whole feature dead on arrival. A verifier
    /// named ONLY in `notify_if` (in no `done_if`, no `invariants`, no `abort_if`, no `if` condition)
    /// must join the RUN-SET and must stay OUT of the DoD-set. The two halves fail in opposite
    /// directions:
    ///
    /// * missing from the run-set ⇒ the name resolves to no verifier file, so the `stop::validate` call
    ///   in `Engine::new` rejects the expression as an unknown identifier and every `rlt
    ///   run`/`plan`/`doctor` dies at startup — you could never USE `notify_if` at all;
    /// * present but `in_dod` ⇒ a detector silently becomes a GOAL: it would hold `all_goals` down,
    ///   inflate the N/M scoreboard, and hand `GateKeepRollback`'s regression check (scoped to
    ///   `in_dod`) a reason to roll a good session back the moment the detector flipped.
    #[test]
    fn a_notify_if_only_verifier_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_verifiers(&["goal", "detector"]);
        let cfg = cfg_with("  notify_if: \"detector.value >= 85\"\n  notify: { cmd: [\"true\"] }\n");
        let asm = assemble(&cfg, tmp.path()).expect("a notify_if-only verifier must assemble");

        let detector = asm
            .engine
            .verifiers
            .iter()
            .find(|g| g.name == "detector")
            .expect("`detector` must be in the run-set — it has to EXECUTE each step or notify_if can never be true");
        assert!(!detector.in_dod, "a detector is machinery, not a goal — it must stay out of the DoD-set");
        // …and the DoD-set is not merely empty: `goal` IS in it, so `detector` is the one exclusion.
        assert!(
            asm.engine.verifiers.iter().any(|g| g.name == "goal" && g.in_dod),
            "the done_if verifier must still be in the DoD-set"
        );
        assert_eq!(asm.engine.tally(), (0, 1), "the N/M scoreboard must count the goal only, never the detector");
    }

    /// A `notify_if` name that resolves to no file is the same hard startup error as a bad `done_if`
    /// name. Run-set membership (§12.1) buys `notify_if` the validation — it must not buy it a pass.
    #[test]
    fn a_notify_if_verifier_with_no_file_is_a_startup_error_naming_it() {
        let tmp = base_with_verifiers(&["goal"]);
        let cfg = cfg_with("  notify_if: \"ghost_detector\"\n  notify: { cmd: [\"true\"] }\n");
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.contains("ghost_detector"), "the error must NAME the unresolvable verifier, got: {err}");
    }

    /// A RATCHETED verifier must EXECUTE every verified step (or there is nothing to hold to its
    /// reference), so it joins the run-set — and it is a gate rule, not a goal, so it stays out of
    /// the DoD-set. `.ref` is accepted on it and refused on anything else, at startup.
    #[test]
    fn a_ratcheted_verifier_joins_the_run_set_but_never_the_dod() {
        let tmp = base_with_verifiers(&["goal", "cc"]);
        let cfg = cfg_with("  ratchet: { cc: { direction: down, tolerance: 0.5 } }\n  abort_if: \"cc.ref > 100 OR rollbacks >= 2\"\n");
        let asm = assemble(&cfg, tmp.path()).expect("a ratcheted verifier must assemble");
        let cc = asm.engine.verifiers.iter().find(|g| g.name == "cc").expect("`cc` must be in the run-set");
        assert!(!cc.in_dod, "a ratchet is a gate rule, not a goal");
        assert!(asm.engine.gate.is_ratcheted("cc"));
        assert_eq!(asm.engine.tally(), (0, 1));

        let err = assemble_err(&cfg_with("  abort_if: \"goal.ref > 1\"\n"), tmp.path());
        assert!(err.contains("has no ratchet"), "`.ref` on an unratcheted verifier is a startup error: {err}");
    }

    /// The refusals: a ratchet naming no verifier file, a meaningless tolerance, a missing direction,
    /// an unknown key, and an oscillation window too short to hold A→B→A.
    #[test]
    fn a_ratchet_that_cannot_mean_anything_is_refused_at_startup() {
        let tmp = base_with_verifiers(&["goal", "cc"]);
        let err = assemble_err(&cfg_with("  ratchet: { ghost: { direction: up } }\n"), tmp.path());
        assert!(err.contains("ghost"), "{err}");
        let err = assemble_err(&cfg_with("  ratchet: { cc: { direction: up, tolerance: -1 } }\n"), tmp.path());
        assert!(err.contains("tolerance"), "{err}");
        let err = assemble_err(&cfg_with("  oscillation_window: 1\n"), tmp.path());
        assert!(err.contains("oscillation_window"), "{err}");
        let bad = "  ratchet: { cc: { direction: up, best: true } }\n";
        let body = format!("project: p\nsteps: {{ worker: {{}} }}\nsequence:\n  steps: [worker]\n  done_if: \"goal\"\n{bad}");
        assert!(serde_yaml::from_str::<RelentlessConfig>(&body).is_err(), "deny_unknown_fields: {bad}");
        // a script verifier states no direction of its own, so leaving it out is refused at startup —
        // at the key to edit (only a metric or score verifier may leave it to the verifier).
        let cfg = cfg_with("  ratchet: { cc: { tolerance: 1 } }\n");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.ratchet.cc.direction"]);
        assert!(assemble_err(&cfg, tmp.path()).contains("`direction` (up | down) is required"));
    }

    /// The §12.7 validity matrix, all four cells. Row 3 is the trap: `notify:` ALONE is not a
    /// mistake, it is the entire "stop + notify" policy (§8.5 — ping only when `abort_if` halts), so
    /// a symmetrical "each key requires the other" check would silently delete a shipped feature.
    #[test]
    fn the_notify_validity_matrix_covers_all_four_rows() {
        let tmp = base_with_verifiers(&["goal", "detector"]);
        let base = tmp.path();

        // row 1 — notify_if + non-empty notify.cmd: the notify ladder.
        assemble(&cfg_with("  notify_if: \"detector\"\n  notify: { cmd: [\"true\"] }\n"), base)
            .expect("row 1: notify_if with a delivery command is the supported shape");

        // row 2 — notify_if with nothing to deliver: a hard error naming BOTH halves, so the operator
        // knows which one to fix. `detector` RESOLVES here, which is what proves this is the notify
        // check firing rather than verifier resolution failing first.
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
    fn refused_paths(cfg: &RelentlessConfig, base: &Path) -> Vec<String> {
        match build(cfg, base) {
            Ok(_) => panic!("build was expected to REFUSE this config"),
            Err(r) => r.problems.into_iter().map(|p| p.path).collect(),
        }
    }

    /// `build` no longer stops at the first problem: every entry, every unresolved verifier and the
    /// notify pair are all reported, each at the key an operator would edit.
    #[test]
    fn build_collects_every_problem_with_its_path() {
        let tmp = base_with_verifiers(&["goal"]);
        let body = "project: p\nsteps: { worker: {} }\nsequence:\n  steps: [ghost, { step: worker, times: 0 }, \
                    { step: worker, max: 2 }]\n  done_if: \"goal AND missing\"\n  invariants: [lost]\n  \
                    abort_if: \"(\"\n  notify_if: \"goal\"\n";
        let cfg: RelentlessConfig = serde_yaml::from_str(body).expect("the fixture must parse");
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

    /// Several problems render as one numbered startup error; the verifiers that DID resolve survive
    /// the refusal so the capability check can still look at them.
    #[test]
    fn a_refusal_keeps_the_resolved_verifiers_and_renders_every_problem() {
        let tmp = base_with_verifiers(&["goal"]);
        let cfg = cfg_with_steps("[ghost, { step: worker, times: 0 }]");
        match build(&cfg, tmp.path()) {
            Ok(_) => panic!("must refuse"),
            Err(r) => assert_eq!(r.verifiers.iter().map(|j| j.name.as_str()).collect::<Vec<_>>(), ["goal"]),
        }
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.starts_with("2 problems") && err.contains("ghost") && err.contains(">= 1"), "{err}");
    }

    /// A bad EXPRESSION over resolved verifiers is located at its key (`Engine::new`'s check, split).
    #[test]
    fn an_invalid_condition_is_located_at_its_key() {
        let tmp = base_with_verifiers(&["goal"]);
        let cfg = cfg_with("  abort_if: \"goal >= \"\n");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.abort_if"]);
    }

    /// The ratchet refusals are located like every other: at the ratchet's own key, at
    /// `oscillation_window`, and a `.ref` on an unratcheted verifier at the expression using it.
    #[test]
    fn ratchet_problems_are_located() {
        let tmp = base_with_verifiers(&["goal", "cc"]);
        let cfg = cfg_with("  ratchet: { cc: { direction: up, tolerance: -1 } }\n  oscillation_window: 1\n");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.ratchet.cc", "sequence.oscillation_window"]);
        let cfg = cfg_with("  abort_if: \"goal.ref > 1\"\n");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.abort_if"]);
    }

    /// A group is its members: a group named in `done_if` puts them in the DoD-set, one named only
    /// in `abort_if` (or named nowhere) puts them in the run-set alone — and the gate learns which
    /// groups to verify as a unit and which verifiers it holds one by one.
    #[test]
    fn group_members_join_the_run_set_and_the_dod_only_through_done_if() {
        let tmp = base_with_verifiers(&["goal", "a", "b", "c", "d", "e"]);
        let cfg = cfg_with(
            "  groups: { qs: [a, b, c], ops: [d], spare: [e] }\n  invariants: [a]\n  abort_if: \"count_met(ops) == 0\"\n",
        );
        let mut cfg = cfg;
        cfg.sequence.done_if = "goal AND count_met(qs) >= 2".into();
        let asm = assemble(&cfg, tmp.path()).expect("groups must assemble");
        let dod = |n: &str| asm.engine.verifiers.iter().find(|j| j.name == n).map(|j| j.in_dod);
        assert_eq!([dod("a"), dod("b"), dod("c")], [Some(true); 3], "a done_if group is its members");
        assert_eq!(dod("d"), Some(false), "an abort_if group is run-set only");
        assert_eq!(dod("e"), Some(false), "a group named nowhere still runs");
        let g = &asm.engine.groups;
        assert_eq!(g.gated.iter().collect::<Vec<_>>(), ["qs"]);
        assert!(g.group_only("b") && !g.group_only("a"), "a is an invariant — held on its own");
        assert!(!g.group_only("goal") && !g.group_only("d"));
    }

    /// The refusals, each at the key to edit: an unknown member (listing the verifiers that exist), an
    /// empty group, a bad name, an unknown group in a condition (listing the groups), k > n.
    #[test]
    fn group_problems_are_located() {
        let tmp = base_with_verifiers(&["goal", "a", "b"]);
        let cfg = cfg_with("  groups: { q: [a, ghost], empty: [], invariants: [a] }\n");
        let r = match build(&cfg, tmp.path()) {
            Ok(_) => panic!("must refuse"),
            Err(r) => r.problems,
        };
        let at = |p: &str| r.iter().find(|x| x.path == p).map(|x| x.message.clone()).unwrap_or_else(|| panic!("no problem at {p}: {r:?}"));
        assert!(at("sequence.groups.q[1]").contains("ghost") && at("sequence.groups.q[1]").contains("available verifiers"));
        assert!(at("sequence.groups.empty").contains("empty"));
        assert!(at("sequence.groups.invariants").contains("reserved"));

        let cfg = cfg_with("  groups: { q: [a, b] }\n  abort_if: \"count_met(qq) >= 1\"\n");
        let err = assemble_err(&cfg, tmp.path());
        assert!(err.contains("unknown group `qq`") && err.contains("defined groups: q"), "{err}");
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.abort_if"]);

        let mut cfg = cfg_with("  groups: { q: [a, b] }\n");
        cfg.sequence.done_if = "count_met(q) >= 3".into();
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.done_if"]);
        assert!(assemble_err(&cfg, tmp.path()).contains("can never hold"));
    }

    /// `merge_requires_improvement` with nothing that could ever improve is refused; with a DoD verifier
    /// or a ratchet it assembles, and a driver project is never refused (it has no DoD by design).
    #[test]
    fn merge_requires_improvement_needs_something_that_can_improve() {
        let tmp = base_with_verifiers(&["goal", "cc"]);
        let mut cfg = cfg_with("  merge_requires_improvement: true\n");
        cfg.sequence.done_if = "over_iterations".into();
        assert_eq!(refused_paths(&cfg, tmp.path()), ["sequence.merge_requires_improvement"]);
        cfg.sequence.ratchet.insert("cc".into(), crate::core::ratchet::Ratchet::down(0.0).into());
        assemble(&cfg, tmp.path()).expect("a ratchet can improve");
        assemble(&cfg_with("  merge_requires_improvement: true\n"), tmp.path()).expect("a DoD verifier can improve");
        let driver: RelentlessConfig = serde_yaml::from_str("project: p\nsequence: { merge_requires_improvement: true }\n").unwrap();
        assemble(&driver, tmp.path()).expect("a driver project declares no DoD");
    }
}
