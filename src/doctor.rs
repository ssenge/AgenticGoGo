//! `agg doctor` — preflight diagnosis. Answers "am I set up right?" in one command,
//! so "why isn't it working?" becomes a checklist instead of a mystery.

use crate::config::{AggConfig, GoalsConfig};
use crate::engine::Engine;
use crate::model::JudgeSpec;
use anyhow::Result;
use std::path::Path;

pub fn run(dir: &Path, config_base: &Path, config: &Path, goals: &Path) -> Result<()> {
    eprintln!("agg doctor — checking your setup in {}\n", dir.display());
    // report which layout is in effect, so a surprising path resolution is visible up front.
    if config_base == dir {
        eprintln!("  config dir: project root");
    } else {
        eprintln!("  config dir: {}/ (the optional config folder)", crate::paths::CONFIG_DIR);
    }
    let mut fail = 0;

    // 1) claude CLI present
    let claude_ok = std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    check(claude_ok, "Claude Code CLI (`claude`) on PATH", "install from https://claude.com/claude-code", &mut fail);

    // 2) agg.yaml present + parses
    let agg_cfg = match (config.exists(), AggConfig::load(config)) {
        (true, Ok(c)) => {
            check(true, "agg.yaml parses", "", &mut fail);
            Some(c)
        }
        (true, Err(e)) => {
            check(false, "agg.yaml parses", &format!("{e}"), &mut fail);
            None
        }
        (false, _) => {
            check(false, "agg.yaml exists", "run `agg init` to scaffold one", &mut fail);
            None
        }
    };

    // 3) goals.yaml present + parses + stop/halt expressions valid
    let goals_cfg = match (goals.exists(), GoalsConfig::load(goals)) {
        (true, Ok(c)) => {
            check(true, "goals.yaml parses", "", &mut fail);
            Some(c)
        }
        (true, Err(e)) => {
            check(false, "goals.yaml parses", &format!("{e}"), &mut fail);
            None
        }
        (false, _) => {
            check(false, "goals.yaml exists", "run `agg init` to scaffold one", &mut fail);
            None
        }
    };

    if let Some(gc) = goals_cfg {
        let n = gc.goals.len();
        // 3a) referenced LLM-judge rubric files exist (resolved against config_base, like the
        //     loop does) — a common failure after a parse error. And 3b) script-judge cmds that
        //     LOOK like a script path (`./judges/x.sh`, not inline shell) point at a file that
        //     exists and is executable — the single most common real failure after parse.
        for g in &gc.goals {
            match &g.judge {
                JudgeSpec::Llm { rubric, .. } => {
                    let rp = config_base.join(rubric);
                    check(
                        rp.exists(),
                        &format!("goal `{}`: rubric `{rubric}` exists", g.id),
                        "create the rubric file (its path is relative to your config dir)",
                        &mut fail,
                    );
                }
                JudgeSpec::Script { cmd, .. } => {
                    // judge scripts run from the PROJECT ROOT (dir), so resolve there.
                    if let Some(script) = script_cmd_path(cmd) {
                        let path = dir.join(&script);
                        if !path.exists() {
                            check(false, &format!("goal `{}`: judge script `{script}` exists", g.id),
                                  "the cmd looks like a script path but no such file (relative to the project root)", &mut fail);
                        } else if !is_executable(&path) {
                            check(false, &format!("goal `{}`: judge script `{script}` is executable", g.id),
                                  &format!("chmod +x {script}"), &mut fail);
                        } else {
                            check(true, &format!("goal `{}`: judge script `{script}` ok", g.id), "", &mut fail);
                        }
                    }
                    // inline shell / bare commands (echo …, pytest, …) are not path-checkable — skip.
                }
            }
        }
        // Engine::new validates stop_when + halt_when
        match Engine::new(gc) {
            Ok(_) => check(true, &format!("stop/halt conditions valid ({n} goal(s))"), "", &mut fail),
            Err(e) => check(false, "stop/halt conditions valid", &format!("{e}"), &mut fail),
        }
    }

    // 4) resume prompt exists (named in agg.yaml, resolved against the config dir)
    if let Some(c) = &agg_cfg {
        let rp = config_base.join(&c.resume_prompt);
        check(rp.exists(), &format!("resume prompt `{}` exists", c.resume_prompt),
              "create it (or run `agg init`); it's the prompt fed to every worker", &mut fail);
    }

    eprintln!();
    if fail == 0 {
        eprintln!("✔ all checks passed — you're ready: `agg plan` then `agg run`.");
        Ok(())
    } else {
        anyhow::bail!("{fail} check(s) failed — fix the items marked ✗ above, then re-run `agg doctor`.");
    }
}

fn check(ok: bool, label: &str, hint: &str, fail: &mut u32) {
    if ok {
        eprintln!("  ✔ {label}");
    } else {
        *fail += 1;
        if hint.is_empty() {
            eprintln!("  ✗ {label}");
        } else {
            eprintln!("  ✗ {label}\n      → {hint}");
        }
    }
}

/// If a script-judge `cmd` is a checkable SCRIPT PATH, return it; otherwise None (inline shell
/// like `echo '...'` / a piped command, or a bare PATH command like `pytest`, can't be
/// file-checked). We treat the cmd as a path iff it's a SINGLE token (no shell metacharacters)
/// that either starts with `./`, `../`, `/`, or ends in a known script extension.
fn script_cmd_path(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    // any shell metacharacter ⇒ it's a command line, not a lone path → don't guess.
    if t.is_empty() || t.split_whitespace().count() != 1
        || t.contains(['|', '&', ';', '>', '<', '$', '`', '(', ')', '\'', '"', '*'])
    {
        return None;
    }
    let looks_like_path = t.starts_with("./")
        || t.starts_with("../")
        || t.starts_with('/')
        || [".sh", ".py", ".rb", ".js", ".ts", ".pl", ".bash"].iter().any(|e| t.ends_with(e));
    looks_like_path.then(|| t.to_string())
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    // Windows has no x-bit; existence is the best we can check.
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::script_cmd_path;

    #[test]
    fn detects_script_paths() {
        assert_eq!(script_cmd_path("./judges/x.sh").as_deref(), Some("./judges/x.sh"));
        assert_eq!(script_cmd_path("/abs/check.py").as_deref(), Some("/abs/check.py"));
        assert_eq!(script_cmd_path("../tools/run.bash").as_deref(), Some("../tools/run.bash"));
        assert_eq!(script_cmd_path("judges/x.sh").as_deref(), Some("judges/x.sh")); // ext match
    }

    #[test]
    fn skips_inline_shell_and_bare_commands() {
        assert_eq!(script_cmd_path(r#"echo '{"met":true}'"#), None); // inline shell
        assert_eq!(script_cmd_path("pytest -q"), None); // multi-token bare command
        assert_eq!(script_cmd_path("pytest"), None); // bare command, no path markers
        assert_eq!(script_cmd_path("a && b"), None); // shell metachar
        assert_eq!(script_cmd_path("cat x | jq ."), None); // pipe
        assert_eq!(script_cmd_path(""), None);
    }
}
