//! `agg init` — scaffold a working AgenticGoGo project in a directory.
//!
//! Kills the blank-page problem: instead of authoring agg.yaml + goals.yaml + a
//! judge + the make-or-break AGG_RESUME.md from scratch, the user runs ONE command
//! and gets a runnable starter that `agg plan` accepts immediately.
//!
//! `--folder` scaffolds into an `agg/` config subdir (config-adjacent files — goals/agg/
//! resume/judges — kept out of the project root); `agg run` auto-detects either layout. The
//! only layout-dependent value is the judge `cmd` path, which is relative to the PROJECT ROOT
//! (scripts always run there), so the foldered judge is `./agg/judges/tests.sh`.

use anyhow::{bail, Result};
use std::path::Path;

/// Scaffold the four starter files. With `folder`, they go under `<dir>/agg/`; otherwise into
/// `<dir>` directly. Refuses to clobber existing config unless `force` is set, so re-running
/// init never silently destroys real work.
pub fn run(dir: &Path, force: bool, folder: bool) -> Result<()> {
    // config_base = where the config files land; judge_cmd = how goals.yaml refers to the judge
    // (relative to the PROJECT ROOT, since judge scripts run there regardless of layout).
    let (base, judge_cmd) = if folder {
        (dir.join(crate::paths::CONFIG_DIR), "./agg/judges/tests.sh")
    } else {
        (dir.to_path_buf(), "./judges/tests.sh")
    };
    let goals_yaml = GOALS_YAML.replace("./judges/tests.sh", judge_cmd);
    // the scaffolded model names come from the backend, so a model bump is a one-line change
    // there instead of a hunt through a YAML template (which `format!` can't interpolate
    // without escaping every `{` in its flow maps).
    let agg_yaml = AGG_YAML
        .replace("{{MODEL}}", crate::backend::active().default_model())
        .replace("{{SUMMARY_MODEL}}", crate::backend::active().default_summary_model());

    let files: [(&str, &str, bool); 4] = [
        ("goals.yaml", goals_yaml.as_str(), false),
        ("agg.yaml", agg_yaml.as_str(), false),
        ("AGG_RESUME.md", RESUME_MD, false),
        ("judges/tests.sh", JUDGE_SH, true), // true = chmod +x
    ];

    // Pre-check: don't overwrite anything without --force.
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
        let shown = if folder { format!("{}/{}", crate::paths::CONFIG_DIR, name) } else { name.to_string() };
        eprintln!("  created {shown}");
    }

    // Ensure `.agg/` (runtime state incl. transient memory scratch) is gitignored even when git
    // isolation is OFF — memory works without isolation, so we can't lean on the isolation path.
    // Guarded: `ensure_agg_gitignored` writes `.gitignore` + runs `git rm --cached` and is NOT a
    // safe no-op outside a repo, so only call it inside one. (`AGG_MEMORY.md` lives at the project
    // root and is intentionally NOT ignored — it's meant to be committed.)
    if crate::git::is_repo(dir) {
        crate::git::ensure_agg_gitignored(dir);
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
#        and three ceiling guards — over_budget (tokens), over_cost ($), over_iterations
#        (sessions) — plus wall_hours. Set the ceilings in agg.yaml (budget/cost) and via
#        --max-sessions; each `over_*` trips when its ceiling is exceeded.
stop_when: "tests_pass"

# Optional emergency brake — stop immediately if an invariant breaks, OR any ceiling blows.
# (Money, tokens, sessions, and time are all OR-ed: hit ANY one and the loop halts.)
halt_when: "any_regressed(invariants) OR over_cost OR over_budget OR over_iterations OR wall_hours >= 4"
"#;

const AGG_YAML: &str = r#"# agg.yaml — harness configuration.
project: my-project
model: "{{MODEL}}"     # the inner worker model
resume_prompt: "AGG_RESUME.md"   # the standing instructions fed to EVERY worker session

heartbeat_secs: 30                # a status line at least this often
watchdog: { idle_secs: 900, cpu_grace: 180 }   # kill a worker silent AND cpu-flat this long
ratelimit_backoff_secs: 1800      # back off this long on a real usage limit

budget: { total: null }           # output-token ceiling (null = unlimited) → over_budget
cost:   { total: null }           # dollar ceiling, e.g. 5.0 (null = unlimited) → over_cost
summary: { enabled: true, model: {{SUMMARY_MODEL}}, min_interval_secs: 300 }  # progress summaries
memory:  { enabled: true, max_kb: 64, inject_kb: 8 }   # durable AGG_MEMORY.md; inject newest 8 KB/prompt
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
#
# ── Optional: per-session git isolation (needs a clean git repo). Each session runs on
#    its own branch off the base; the result is merged back UNLESS the worker vetoed it
#    (wrote red_file). With rollback_on_regression (default on), agg stages the merge,
#    re-runs the judges against the merged tree, and ROLLS BACK if a previously-met goal
#    (e.g. an invariant) regressed — so a bad session can never land on base. ──
# session_isolation:
#   enabled: true
#   rollback_on_regression: true   # stage→re-test→commit|rollback (base never regresses)
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
- LONG TASKS (a sim/build/benchmark that runs longer than one turn): do NOT launch it with a
  bare `nohup … &` and then idle-wait — your session ends when your turn does, which kills or
  orphans the task, and the next session relaunches a duplicate. Instead run it via
  `agg spawn --name <id> --reason "<why>" -- <cmd>`. agg keeps it alive past your session,
  PROTECTS it from the straggler reaper, and tells the next session it's running (and why).
  Then EXIT. A later session is told about it and polls its log (`.agg/spawns/<id>.log`) —
  consuming the result when it finishes — instead of starting over. One spawn per task; check
  the BACKGROUND TASKS block at the top of your prompt before launching anything.

# Memory (OPTIONAL — agg captures memory either way)
- agg keeps durable cross-session learnings in `AGG_MEMORY.md` at the project root and injects a
  recent slice of it (plus a LAST SESSION block) at the BOTTOM of this prompt, as lower-priority
  context. READ it — it's what prior sessions learned. You do NOT maintain it; agg folds a note
  after every session automatically (even if you crash or get killed mid-task).
- If you have a crisp, durable learning worth carrying forward (a gotcha, a decision, the exact
  next step), you MAY write it to `.agg/memory/session-<N>.md`, where `<N>` is THIS session number
  from the "session #N" banner agg printed when it launched you (e.g. `.agg/memory/session-7.md`).
  agg prefers your note on a clean session. This is OPTIONAL — skipping it loses nothing; agg
  still records a mechanical note from the goal scoreboard and your visible progress. Keep it
  short and plain; agg sanitizes and size-caps it.

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

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agg-init-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scaffold_parses_with_real_loaders() {
        let dir = tmpdir("root");
        run(&dir, false, false).unwrap();
        // the generated config must load with the ACTUAL loaders (no schema drift)
        crate::core::config::AggConfig::load(&dir.join("agg.yaml")).expect("scaffolded agg.yaml must parse");
        let g = crate::core::config::GoalsConfig::load(&dir.join("goals.yaml")).expect("scaffolded goals.yaml must parse");
        crate::core::engine::Engine::new(g).expect("scaffolded goals must build an engine (stop_when valid)");
        // refuses to clobber without force
        assert!(run(&dir, false, false).is_err());
        assert!(run(&dir, true, false).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_scaffold_lands_under_agg_and_resolves() {
        let dir = tmpdir("folder");
        run(&dir, false, true).unwrap();
        // files land under agg/, NOT the root
        assert!(dir.join("agg/agg.yaml").exists(), "foldered agg.yaml under agg/");
        assert!(dir.join("agg/goals.yaml").exists());
        assert!(dir.join("agg/judges/tests.sh").exists());
        assert!(!dir.join("agg.yaml").exists(), "root must stay clean in folder mode");
        // the resolver finds the foldered config, and it loads + builds an engine
        assert_eq!(crate::paths::config_base(&dir), dir.join("agg"));
        let cfg = crate::paths::config_file(&dir, "goals.yaml");
        assert_eq!(cfg, dir.join("agg/goals.yaml"));
        let g = crate::core::config::GoalsConfig::load(&cfg).expect("foldered goals.yaml must parse");
        // the judge cmd points at the project-root-relative path
        assert!(
            format!("{:?}", g.goals[0].judge).contains("./agg/judges/tests.sh"),
            "foldered judge cmd should be root-relative: {:?}", g.goals[0].judge
        );
        crate::core::engine::Engine::new(g).expect("foldered goals must build an engine");
        std::fs::remove_dir_all(&dir).ok();
    }
}
