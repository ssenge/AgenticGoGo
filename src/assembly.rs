//! Building the engine + parsed sequence from config.

use std::path::Path;
use anyhow::Result;
use crate::core::config::AggConfig;
use crate::core::engine::Engine;
use crate::core::sequence::{self, Statement};
use crate::core::stop;

/// The engine + parsed sequence, assembled from config. Built once, before the loop (and by
/// `agg plan`).
pub struct Assembly {
    pub engine: Engine,
    pub statements: Vec<Statement>,
}

/// Build the run-set engine + parse the sequence from `cfg` (§5.3/§5.4). Refuses at startup:
/// an unknown step name, an all-`skip_judges` sequence (nothing could ever merge), or a judge name
/// that resolves to no file.
///
/// # The DRIVER variant (BUILD.md §3.6)
///
/// An EMPTY `sequence.steps` **is** a Rust-driver project: the flow lives in the driver's own
/// `for`/`if`, so there is no statement list to validate and no `done_if` for agg to evaluate. Both
/// startup refusals below are about the statement list, so with no statements there is nothing to
/// refuse — and the all-`skip_judges` guard would otherwise reject every driver project outright.
/// The resulting run-set is EMPTY (nothing is declared to agg on that path: no `abort_if`, no
/// `notify_if`, no `invariants`), which is correct rather than a gap — driver judges are LAZY and
/// the driver asks for them by hand.
pub fn assemble(cfg: &AggConfig, config_base: &Path) -> Result<Assembly> {
    use crate::core::judges;
    use crate::core::model::{Judge, Lifecycle};

    // the standard library must exist before we resolve names against it (§6.1).
    if let Err(e) = judges::ensure_library() {
        eprintln!("  ⚠ could not refresh ~/.agg/judges: {e}");
    }

    let driver = cfg.sequence.steps.is_empty();
    // `sequence::parse` refuses an empty list ("a run needs at least one step") — true of a YAML
    // run and false of a driver one, where the steps are `agg.step(&s)` calls in Rust.
    let statements = if driver { Vec::new() } else { sequence::parse(&cfg.sequence.steps)? };

    // every referenced step name must be a key in `steps:` (§5.4).
    for st in &statements {
        for name in st.step_names() {
            if !cfg.steps.contains_key(name) {
                let defined: Vec<&str> = cfg.steps.keys().map(String::as_str).collect();
                anyhow::bail!(
                    "sequence references unknown step `{name}` — defined steps: {}",
                    defined.join(", ")
                );
            }
        }
    }
    // an all-`skip_judges` sequence never merges, so `done_if` can never fire (§5.7) — refuse.
    let has_judged = statements
        .iter()
        .flat_map(|s| s.step_names())
        .any(|n| cfg.steps.get(n).map(|b| !b.skip_judges).unwrap_or(false));
    if !driver && !has_judged {
        anyhow::bail!(
            "every step in the sequence is skip_judges — nothing can ever merge and done_if can \
             never fire (§5.7). At least one judged step is required."
        );
    }

    // DoD-set = done_if ∪ invariants; run-set = DoD ∪ abort_if ∪ every if-condition (§5.3).
    // On the driver path there is no DoD at all — agg is never told what "done" means, so it must
    // not claim success. `Sequence::done_if` still carries its YAML default; it is simply not read.
    let done_if = (!driver).then(|| cfg.sequence.done_if.clone());
    let mut dod: Vec<String> = match &done_if {
        Some(d) => stop::judge_names(d)?,
        None => Vec::new(),
    };
    for inv in &cfg.sequence.invariants {
        push_unique(&mut dod, inv);
    }
    let mut run_set = dod.clone();
    if let Some(a) = &cfg.sequence.abort_if {
        for n in stop::judge_names(a)? {
            push_unique(&mut run_set, &n);
        }
    }
    // `notify_if` joins the RUN-SET on exactly the same terms as `abort_if` (STUCK_NOTIFY §12.1):
    // its detectors must EXECUTE each step, but they are machinery, not goals — never in the DoD-set,
    // so `all_goals` can't be blocked by `stuck` and the regression gate (scoped to `in_dod`) can't
    // roll a session back because a detector flipped. Without this the judge resolves to no file and
    // `stop::validate` below rejects the expression as an unknown identifier at startup.
    if let Some(n) = &cfg.sequence.notify_if {
        for name in stop::judge_names(n)? {
            push_unique(&mut run_set, &name);
        }
    }
    for st in &statements {
        if let Some(c) = st.condition() {
            for n in stop::judge_names(c)? {
                push_unique(&mut run_set, &n);
            }
        }
    }

    // resolve every run-set name to a judge FILE (§5.1) — a name with no file is a startup error.
    let mut judges_vec: Vec<Judge> = Vec::with_capacity(run_set.len());
    for name in &run_set {
        let kind = judges::resolve(name, config_base)?;
        judges_vec.push(Judge {
            name: name.clone(),
            kind,
            invariant: cfg.sequence.invariants.iter().any(|i| i == name),
            in_dod: dod.iter().any(|d| d == name),
            // no per-judge `timeout:` key in YAML — the run-level `judge.timeout` covers every one.
            timeout: None,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        });
    }

    // `notify_if` with nothing to deliver is a silent no-op — the loop would detect and tell nobody.
    // Refuse it loudly here (`notify:` ALONE is fine: that is the "stop + notify" policy, §12.7).
    if cfg.sequence.notify_if.is_some()
        && cfg.sequence.notify.as_ref().map(|n| n.cmd.is_empty()).unwrap_or(true)
    {
        anyhow::bail!(
            // the suggested command is BOUNDED: delivery is foreground and untimed, and this string is
            // read at exactly the moment the user is about to write their first notify.cmd.
            "sequence.notify_if is set but sequence.notify.cmd is empty — nothing would fire. Add a \
             delivery command (e.g. notify: {{ cmd: [\"curl -s --max-time 10 -d {{{{reason}}}} \
             ntfy.sh/my-topic\"] }}) or remove notify_if."
        );
    }

    let engine = Engine::new(
        judges_vec,
        done_if,
        cfg.sequence.abort_if.clone(),
        cfg.sequence.notify_if.clone(),
    )?;
    Ok(Assembly { engine, statements })
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
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

    /// The message of an `assemble` that must FAIL. Hand-rolled because `Assembly` is deliberately
    /// not `Debug` (it holds judge state), so `Result::unwrap_err` is unavailable here.
    fn assemble_err(cfg: &AggConfig, base: &Path) -> String {
        match assemble(cfg, base) {
            Ok(_) => panic!("assemble was expected to REFUSE this config, but it succeeded"),
            Err(e) => e.to_string(),
        }
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
}
