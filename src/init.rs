//! `agg init` — scaffold a working AgenticGoGo project.
//!
//! One config file now (`agg.yaml` — defaults/judge/steps/sequence) plus the forward state file
//! (`AGG_STATE.md`) and a starter judge. `goals.yaml` is gone: a judge IS a goal, resolved by name
//! from `agg/judges/` (§7.1). Everything scaffolds under the mandatory `agg/` folder.

use anyhow::{bail, Result};
use std::path::Path;

pub fn run(dir: &Path, force: bool, agent: Option<&str>) -> Result<()> {
    let chosen = agent.map(str::to_string).or_else(|| crate::skills::host_agent().map(str::to_string));
    let b = crate::backend::for_name(chosen.as_deref().unwrap_or("claude"))?;
    let base = dir.join(crate::paths::CONFIG_DIR);
    let agent = b.name();

    // Two keys are not universal (§4.1) — emitting them for the wrong agent is a startup REFUSAL:
    //   model:  Codex must OMIT it (naming a model is a hard 400).
    //   cost:   Claude ONLY reports dollars.
    let model_line = match b.default_model() {
        "" => "  # model:                        # codex picks its own — naming one is a hard 400\n".to_string(),
        m => format!("  model: \"{m}\"                 # the inner worker model\n"),
    };
    let effort_line = match b.default_effort() {
        "" => "  effort: \"\"                       # this agent cannot combine effort with model: auto\n".to_string(),
        e => format!("  effort: \"{e}\"                  # thinking effort: low|medium|high|xhigh|max\n"),
    };
    let judge_model = match b.default_summary_model() {
        "" => "  # model:                        # codex omits it, same hard-400 reason\n".to_string(),
        m => format!("  model: \"{m}\"                 # the cheap RULER model for LLM judges\n"),
    };
    let (cost_line, over_cost) = if b.capabilities().reports_cost_usd {
        ("    cost: null                     # dollar ceiling (null = unlimited) → over_cost\n".to_string(), " OR over_cost")
    } else {
        // cost omitted (not null) so the block stays minimal; a comment says why it's absent.
        (format!(
            "    # cost: omitted — `{agent}` cannot report dollars, so over_cost can never fire (agg warns/refuses). Use `tokens`.\n"
        ), "")
    };

    let agg_yaml = AGG_YAML
        .replace("{{AGENT}}", agent)
        .replace("{{MODEL_LINE}}", &model_line)
        .replace("{{EFFORT_LINE}}", &effort_line)
        .replace("{{JUDGE_MODEL}}", &judge_model)
        .replace("{{COST_LINE}}", &cost_line)
        .replace("{{OVER_COST}}", over_cost);

    let files: [(&str, &str, bool); 3] = [
        ("agg.yaml", agg_yaml.as_str(), false),
        ("AGG_STATE.md", STATE_MD, false),
        ("judges/tests_pass.sh", JUDGE_SH, true),
    ];

    if !force {
        let existing: Vec<&str> = files
            .iter()
            .map(|(name, _, _)| *name)
            .filter(|name| base.join(name).exists())
            .collect();
        if !existing.is_empty() {
            bail!(
                "refusing to overwrite existing file(s): {}\n  re-run with `agg init --force` to replace them.",
                existing.join(", ")
            );
        }
    }

    for (name, contents, executable) in files {
        let path = base.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
        if executable {
            make_executable(&path);
        }
        eprintln!("  created {}/{}", crate::paths::CONFIG_DIR, name);
    }

    // install the standard judge library to ~/.agg/judges/ so a library-named judge resolves.
    if let Err(e) = crate::core::judges::ensure_library() {
        eprintln!("  ⚠ could not install the ~/.agg/judges library: {e}");
    }

    if crate::git::is_repo(dir) {
        crate::git::ensure_agg_gitignored(dir);
    }

    eprintln!(
        "\n✔ Scaffolded an AgenticGoGo starter in {}.\n\n\
         Next steps:\n  \
         1. Edit agg/agg.yaml `done_if` + agg/judges/ to match YOUR project.\n  \
         2. Edit agg/AGG_STATE.md — the standing instructions each worker session reads.\n  \
         3. agg plan            # dry-run: see the starting scoreboard (run from the project root)\n  \
         4. agg run             # launch the loop until done_if is met\n",
        base.display()
    );
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

// ---- starter file contents ----

const AGG_YAML: &str = r#"# agg.yaml — harness + steps + sequence. One file: a judge IS a goal, resolved by NAME from
# agg/judges/<name>.{sh,md} (then ~/.agg/judges/).
project: "my-project"

# Inherited by EVERY step; a step body may override any of these.
defaults:
  agent: "{{AGENT}}"
{{MODEL_LINE}}{{EFFORT_LINE}}  state: "AGG_STATE.md"            # the forward state file the AGENT maintains (best-effort)

# THE RULER — runs the LLM judges + the summarizer. Immutable; naming any of these in a step is a
# HARD ERROR (a grader that moves makes verdicts incomparable across cycles).
judge:
  agent: "{{AGENT}}"
{{JUDGE_MODEL}}  timeout: 300                     # seconds, EVERY judge (script + LLM)

# The step palette. The NAME is your own label; the body is overrides only.
steps:
  worker: {}                       # pure defaults

# The repeating sequence + the run-level ceilings and Definition of Done.
sequence:
  steps:
    - "worker"                     # run `worker`, forever, until done_if fires
  # Run-level ceilings, unified under one block — each null = unlimited.
  limits:
    tokens: null                   # output-token ceiling — worker AND judge spend → over_budget
{{COST_LINE}}    sessions: null                 # session cap → over_iterations (or pass `agg run --max-sessions <n>`)
  gate_regressions: true           # roll a session back if a previously-met judge regresses
  invariants: []                   # judge names that must STAY met
  done_if: "tests_pass"            # your Definition of Done — judge names, all_goals, count_met, …
  abort_if: "over_budget{{OVER_COST}} OR over_iterations OR wall_hours >= 4"

heartbeat_secs: 30
watchdog: { idle_secs: 900, cpu_grace: 180 }
ratelimit_backoff_secs: 1800
summary: { enabled: true, min_interval_secs: 300 }
memory:  { enabled: true, max_kb: 64, inject_kb: 8 }
session_isolation: {}              # MANDATORY; defaults (branch_prefix: agg, red_file: .agg_red)
"#;

const STATE_MD: &str = r#"<!-- AGG_STATE.md — the standing instructions fed to EVERY fresh worker session, AND the forward
     state the agent maintains ("what to do next"). agg reads it at the bottom of each prompt; you
     start it, the agent updates it as it works. A vague file = a loop that spins. -->

# Goal
Make all the project's tests pass.

# This session — do ONE self-contained chunk of work
1. Orient: read this file, then run the project's tests/checks to see what's failing.
2. Implement or fix ONE thing that moves a goal forward. Real, correct work — no stubs.
3. Verify your change (re-run the relevant test/check).
4. Update THIS file with the new state + the exact next task; commit.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
- Commit as you go. Keep changes focused and correct. When context fills, finish the chunk,
  update this file, commit, then exit — the loop relaunches you fresh.
"#;

const JUDGE_SH: &str = r#"#!/usr/bin/env bash
# Starter judge (agg/judges/tests_pass.sh) — resolved by the NAME `tests_pass` in done_if.
# Prints a verdict JSON to stdout. REPLACE the body with your real check.
# Env agg sets: AGG_SESSION, AGG_STEP, AGG_JUDGE, AGG_PROJECT_DIR.
N="$(cat .passing 2>/dev/null || echo 0)"
TARGET=3
met=$([ "$N" -ge "$TARGET" ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass (starter stub — replace me)"}\n' \
  "$met" "$N" "$TARGET" "$TARGET" "$N" "$TARGET"
"#;
