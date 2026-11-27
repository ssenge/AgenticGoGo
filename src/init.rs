//! `rlt init` — scaffold a working Relentless project.
//!
//! `rlt.yaml` (defaults/judge/steps/sequence), a committed `RLT.md` (stable scope), the gitignored
//! forward-state file `state/STATE.md`, and a starter judge. `goals.yaml` is gone: a judge IS a
//! goal, resolved by name from `rlt/judges/` (§7.1). Config + RLT.md are COMMITTED under `rlt/`;
//! runtime state is gitignored and split by who writes it — the worker's own files (STATE.md,
//! `wiki/`) in `rlt/state/`, rlt's bookkeeping (INSTRUCTIONS.md, the verdict ledger, the bus) in
//! `rlt/private/`. `init` only ever scaffolds the former; `rlt/private/` is created by the loop.
//! See [`crate::paths`] for the full table.

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
            "    # cost: omitted — `{agent}` cannot report dollars, so over_cost can never fire (rlt warns/refuses). Use `tokens`.\n"
        ), "")
    };

    let rlt_yaml = RLT_YAML
        .replace("{{AGENT}}", agent)
        .replace("{{MODEL_LINE}}", &model_line)
        .replace("{{EFFORT_LINE}}", &effort_line)
        .replace("{{JUDGE_MODEL}}", &judge_model)
        .replace("{{COST_LINE}}", &cost_line)
        .replace("{{OVER_COST}}", over_cost);

    let files: [(&str, &str, bool); 4] = [
        ("rlt.yaml", rlt_yaml.as_str(), false),
        ("RLT.md", RLT_MD, false),
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
                "refusing to overwrite existing file(s): {}\n  re-run with `rlt init --force` to replace them.",
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

    // install the standard judge library to ~/.rlt/judges/ so a library-named judge resolves.
    if let Err(e) = crate::core::judges::ensure_library() {
        eprintln!("  ⚠ could not install the ~/.rlt/judges library: {e}");
    }

    if crate::git::is_repo(dir) {
        crate::git::ensure_rlt_gitignored(dir);
    }

    eprintln!(
        "\n✔ Scaffolded an Relentless starter in {}.\n\n\
         Next steps:\n  \
         1. Edit rlt/rlt.yaml `done_if` + rlt/judges/ to match YOUR project.\n  \
         2. Edit rlt/RLT.md — the standing project instructions each worker reads (committed).\n  \
         3. Edit rlt/state/STATE.md — the forward \"what to do next\" advice (rlt regenerates the\n     \
            per-session brief at rlt/private/INSTRUCTIONS.md from these; both gitignored).\n  \
         4. rlt plan            # dry-run: see the starting scoreboard (run from the project root)\n  \
         5. rlt run             # launch the loop until done_if is met\n",
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
//
// The scaffolds live as REAL files under `plugin/scaffold/` and are `include_str!`'d — the same
// pattern this crate already uses for the `/rlt:*` skills (`crate::skills`) and the judge library
// (`crate::core::judges`). Editing a scaffold means editing an actual .md/.yaml/.sh file (syntax
// highlighting, no Rust-string escaping) rather than an inline `r#"…"#` blob. They still compile INTO
// the binary — a change is a behavior change and must go through the gate; the file just reads better.
// The `{{…}}` placeholders in `rlt.yaml` are filled by the `.replace()` calls in `run` (no template
// engine — a handful of substitutions do not earn a dependency).

const RLT_YAML: &str = include_str!("../plugin/scaffold/rlt.yaml");
const RLT_MD: &str = include_str!("../plugin/scaffold/RLT.md");
const STATE_MD: &str = include_str!("../plugin/scaffold/STATE.md");
const JUDGE_SH: &str = include_str!("../plugin/scaffold/tests_pass.sh");
