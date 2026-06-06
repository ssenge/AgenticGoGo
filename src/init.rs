//! `agg init` — scaffold a working AgenticGoGo project in a directory.
//!
//! Kills the blank-page problem: instead of authoring agg.yaml + goals.yaml + a
//! judge + the make-or-break AGG_RESUME.md from scratch, the user runs ONE command
//! and gets a runnable starter that `agg plan` accepts immediately.

use anyhow::{bail, Result};
use std::path::Path;

/// Scaffold the four starter files into `dir`. Refuses to clobber existing config
/// unless `force` is set, so re-running init never silently destroys real work.
pub fn run(dir: &Path, force: bool) -> Result<()> {
    let files: [(&str, &str, bool); 4] = [
        ("goals.yaml", GOALS_YAML, false),
        ("agg.yaml", AGG_YAML, false),
        ("AGG_RESUME.md", RESUME_MD, false),
        ("judges/tests.sh", JUDGE_SH, true), // true = chmod +x
    ];

    // Pre-check: don't overwrite anything without --force.
    if !force {
        let existing: Vec<&str> = files
            .iter()
            .map(|(name, _, _)| *name)
            .filter(|name| dir.join(name).exists())
            .collect();
        if !existing.is_empty() {
            bail!(
                "refusing to overwrite existing file(s): {}\n  re-run with `agg init --force` to replace them.",
                existing.join(", ")
            );
        }
    }

    for (name, contents, executable) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
        if executable {
            make_executable(&path);
        }
        eprintln!("  created {}", name);
    }

    eprintln!(
        "\n✔ Scaffolded an AgenticGoGo starter in {}.\n\n\
         Next steps:\n  \
         1. Edit goals.yaml / judges/ to match YOUR project (the starter checks a `.passing` file).\n  \
         2. Edit AGG_RESUME.md — this prompt drives every worker session; make it specific.\n  \
         3. agg plan            # dry-run: see the starting scoreboard\n  \
         4. agg run             # launch the loop until your goals are met\n  \
         5. agg dashboard       # (optional, another terminal) live TUI\n\n\
         Tip: in Claude Code, `/agg:new` can generate these FROM your existing plans instead.",
        dir.display()
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

const GOALS_YAML: &str = r#"# goals.yaml — what "done" means for this project.
# Each goal has a judge that prints a verdict JSON to stdout:
#   {"met": <bool>, "value": <num>, "max": <num>, "target": <num>, "rationale": "<one line>"}

goals:
  # A cardinal goal: met when `value` reaches `target` (e.g. N of M tests pass).
  - id: tests_pass
    type: cardinal           # binary | percentage | cardinal
    target: 3
    description: "All tests pass"
    judge:
      kind: script           # a command whose stdout is the verdict JSON
      cmd: "./judges/tests.sh"
      timeout: 60

  # An INVARIANT goal: must STAY true. If it regresses, halt_when can stop the loop.
  # (Replace this stub with a real "never break the build / no wrong results" check.)
  - id: no_regressions
    type: binary
    invariant: true
    description: "Never break a previously-passing build"
    judge:
      kind: script
      cmd: 'echo ''{"met":true,"value":1,"max":1,"target":1,"rationale":"build green (stub — replace me)"}'''
      timeout: 30

# Stop the loop when this expression is true (a safe mini-language, NOT eval).
# Terms: goal ids, all_goals, count_met, met_fraction, any_regressed(invariants),
#        over_budget, tokens_spent, wall_hours.
stop_when: "tests_pass"

# Optional emergency brake — stop immediately if an invariant breaks or budget/time blows.
halt_when: "any_regressed(invariants) OR wall_hours >= 4"
"#;

const AGG_YAML: &str = r#"# agg.yaml — harness configuration.
project: my-project
model: "claude-opus-4-8[1m]"     # the inner worker model
resume_prompt: "AGG_RESUME.md"   # the standing instructions fed to EVERY worker session

heartbeat_secs: 30                # a status line at least this often
watchdog: { idle_secs: 900, cpu_grace: 180 }   # kill a worker silent AND cpu-flat this long
ratelimit_backoff_secs: 1800      # back off this long on a real usage limit

budget: { total: null }           # output-token ceiling (null = unlimited)
summary: { enabled: true, model: haiku, min_interval_secs: 300 }  # progress summaries
resume_sessions: false            # fresh context per session (recommended)

# ── Optional: generic lifecycle hooks (agg just runs these shell commands; it is
#    tool-agnostic — wire in whatever YOU use). Uncomment + edit as needed. ──
# hooks:
#   on_start:         []   # once at startup — e.g. build a code graph: ["graphify . --no-viz"]
#   on_session_start: []   # before each worker session — e.g. incremental refresh
#   on_session_end:   []   # after each session's judging — e.g. persist a memory note
#   on_stop:          []   # once when the loop stops — e.g. teardown / final export
#   background:       []   # long-lived, reaped on stop — e.g. ["graphify . --watch"]
#
# ── Optional: files prepended to every worker prompt (reusable tooling/guidance you
#    author — agg adds NO tool-specific text). e.g. tell the worker to use your tools. ──
# prompt_includes:
#   - "AGG_TOOLING.md"
"#;

const RESUME_MD: &str = r#"<!-- AGG_RESUME.md — the prompt fed to EVERY fresh worker session.
     This is the single most important file: a vague prompt = a loop that spins.
     Make it specific to YOUR project. Keep the autonomous-loop structure below. -->

# Goal
<!-- One or two sentences: what should be true when this is done? -->
Make all the project's tests pass.

# This session — do ONE self-contained chunk of work
1. Orient: read any handoff/state file, then run the project's tests/checks to see what's failing.
2. Implement or fix ONE thing that moves a goal forward. Do real, correct work — no stubs.
3. Verify your change (re-run the relevant test/check).
4. If there's a HANDOFF file, update it with the new state + the exact next task; commit.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
- `claude -p` does not auto-compact; when context fills you just stop. So BEFORE that:
  finish the current chunk, write the handoff, commit, then exit. The loop relaunches you fresh.
- Commit as you go. Keep changes focused and correct.

<!-- If your project uses a spec/plan tool (get-shit-done, a ROADMAP, etc.), paste the
     relevant execution steps HERE — skills are NOT invocable in headless `agg run`. -->
"#;

const JUDGE_SH: &str = r#"#!/usr/bin/env bash
# Starter judge — prints a verdict JSON to stdout.
# This stub reads a count from a `.passing` file (default 0) and reports N/3.
# REPLACE the body with your real check, e.g.:
#   out="$(npm test 2>&1)"; passed=$(echo "$out" | grep -oE '[0-9]+ passing' | grep -oE '[0-9]+')
#   ...then emit the JSON below with your real numbers.
N="$(cat .passing 2>/dev/null || echo 0)"
TARGET=3
met=$([ "$N" -ge "$TARGET" ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass (starter stub — replace me)"}\n' \
  "$met" "$N" "$TARGET" "$TARGET" "$N" "$TARGET"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_parses_with_real_loaders() {
        let dir = std::env::temp_dir().join(format!("agg-init-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, false).unwrap();
        // the generated config must load with the ACTUAL loaders (no schema drift)
        crate::config::AggConfig::load(&dir.join("agg.yaml")).expect("scaffolded agg.yaml must parse");
        let g = crate::config::GoalsConfig::load(&dir.join("goals.yaml")).expect("scaffolded goals.yaml must parse");
        crate::engine::Engine::new(g).expect("scaffolded goals must build an engine (stop_when valid)");
        // refuses to clobber without force
        assert!(run(&dir, false).is_err());
        assert!(run(&dir, true).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
