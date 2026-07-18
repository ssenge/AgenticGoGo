//! `agg init` — scaffold a working AgenticGoGo project.
//!
//! `agg.yaml` (defaults/judge/steps/sequence), a committed `AGG.md` (stable scope), the gitignored
//! forward-state file `state/STATE.md`, and a starter judge. `goals.yaml` is gone: a judge IS a
//! goal, resolved by name from `agg/judges/` (§7.1). Config + AGG.md are COMMITTED under `agg/`; all
//! runtime state (incl. STATE.md) lives under the gitignored `agg/state/`.

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

    let files: [(&str, &str, bool); 4] = [
        ("agg.yaml", agg_yaml.as_str(), false),
        ("AGG.md", AGG_MD, false),
        ("state/STATE.md", STATE_MD, false),
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
         2. Edit agg/AGG.md — the standing project instructions each worker reads (committed).\n  \
         3. Edit agg/state/STATE.md — the forward \"what to do next\" advice (agg regenerates the\n     \
            per-session brief at agg/state/INSTRUCTIONS.md from these; gitignored).\n  \
         4. agg plan            # dry-run: see the starting scoreboard (run from the project root)\n  \
         5. agg run             # launch the loop until done_if is met\n",
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
{{MODEL_LINE}}{{EFFORT_LINE}}  state: "state/STATE.md"          # forward "what to do next" file the AGENT maintains (under agg/, gitignored)

# THE RULER — runs the LLM judges + the summarizer. Immutable; naming any of these in a step is a
# HARD ERROR (a grader that moves makes verdicts incomparable across cycles).
judge:
  agent: "{{AGENT}}"
{{JUDGE_MODEL}}  timeout: 300                     # seconds, EVERY judge (script + LLM)

# The step palette. The NAME is your own label; the body is overrides only. A step may also carry
# `role_prompt:` (generic role framing, e.g. a red-team "reconsider" step) and `prompt:` (its
# specific ask) — both are composed into the per-session brief above the context. Example:
#   reconsider: { skip_judges: true, role_prompt: "Step back — assume the current approach is wrong.",
#                 prompt: "Name 2-3 different approaches, pick one, record the rejected ones + why." }
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

const AGG_MD: &str = r#"<!-- AGG.md — the standing instructions for this project (the CLAUDE.md-analog for the agg loop).
     COMMITTED; you (the human) own it; rare edits. agg points every worker session here for
     orientation. The moving "what to do next" lives in agg/state/STATE.md; durable knowledge and
     multi-session PLANS live in agg/state/wiki/. A vague file here = a loop that spins. -->

# Project
One line: what this project is and does.

# Goal
Make all the project's tests pass. (The real gate is your judges / `done_if`.)

# Architecture — where things live
Fill this in so a fresh worker orients fast: key modules/entry points, and the exact build/test
commands to run.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
- Real, correct work only — no stubs. Keep changes focused.
- Durable knowledge lives in `agg/state/wiki/` as an OKF (Open Knowledge Format) wiki: one concept
  per markdown file (HYPHENATED, space-free filenames so links resolve everywhere), a required `type:`
  frontmatter, CROSS-LINKED with standard `[label](page.md)` markdown links so it forms a graph. agg's
  per-session brief ships the exact format + a template. Keep any multi-session PLAN there (STATE.md is
  rewritten each session, so a plan parked there is lost; the wiki persists and survives rollbacks) and
  record dead-ends + decisions. View it in Obsidian by opening the `agg/` folder as a vault.
"#;

const STATE_MD: &str = r#"<!-- STATE.md — your predecessor's forward advice: crisp "what to do next". agg regenerates the
     per-session brief (agg/state/INSTRUCTIONS.md) from this + AGG.md + memory, and points the
     worker at it. You (the agent) rewrite THIS file each session before you exit. Keep it SHORT —
     it is read in full. Gitignored, so it survives a session rollback. -->

# Where things stand
First session — nothing done yet.

# Next step
1. Orient: read agg/AGG.md, then run the project's tests/checks to see what's failing.
2. Implement or fix ONE thing that moves a goal forward. Real, correct work — no stubs.
3. Verify your change (re-run the relevant test/check).
4. Rewrite THIS file with the new state + the exact next task before you exit.
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
