//! `agg doctor` — preflight diagnosis. Answers "am I set up right?" in one command,
//! so "why isn't it working?" becomes a checklist instead of a mystery.

use crate::config::{AggConfig, GoalsConfig};
use crate::engine::Engine;
use anyhow::Result;
use std::path::Path;

pub fn run(dir: &Path, config: &Path, goals: &Path) -> Result<()> {
    eprintln!("agg doctor — checking your setup in {}\n", dir.display());
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
        // Engine::new validates stop_when + halt_when
        match Engine::new(gc) {
            Ok(_) => check(true, &format!("stop/halt conditions valid ({n} goal(s))"), "", &mut fail),
            Err(e) => check(false, "stop/halt conditions valid", &format!("{e}"), &mut fail),
        }
    }

    // 4) resume prompt exists (named in agg.yaml)
    if let Some(c) = &agg_cfg {
        let rp = dir.join(&c.resume_prompt);
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
