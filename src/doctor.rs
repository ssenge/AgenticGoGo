//! `rlt doctor` — preflight diagnosis. Answers "am I set up right?" in one command.

use crate::core::config::RelentlessConfig;
use anyhow::Result;
use std::path::Path;

mod json;
pub use json::{report, BackendFacts, ConfigFacts, DoctorReport, Tier, Tiers};

/// `rlt doctor --json`: print the [`DoctorReport`] and return its `ok`.
pub fn run_json(config_base: &Path, config: &Path) -> Result<bool> {
    let r = report(config_base, config);
    println!("{}", serde_json::to_string_pretty(&r)?);
    Ok(r.ok)
}

pub fn run(dir: &Path, config_base: &Path, config: &Path) -> Result<()> {
    eprintln!("rlt doctor — checking your setup in {}\n", dir.display());
    eprintln!("  config dir: {}/", crate::paths::CONFIG_DIR);
    let mut fail = 0;

    // 1) rlt.yaml present + parses (the new single config; goals.yaml is gone).
    let cfg = match (config.exists(), RelentlessConfig::load(config)) {
        (true, Ok(c)) => {
            check(true, "rlt.yaml parses", "", &mut fail);
            Some(c)
        }
        (true, Err(e)) => {
            check(false, "rlt.yaml parses", &format!("{e}"), &mut fail);
            None
        }
        (false, _) => {
            check(false, "rlt.yaml exists", "run `rlt init` to scaffold one", &mut fail);
            None
        }
    };

    // 2) EVERY agent the sequence names is on PATH (§7.3) — worker default, ruler, per-step agents.
    let agent_names: Vec<String> = match &cfg {
        Some(c) => c.agent_names(),
        None => vec![RelentlessConfig::agent_name(config)],
    };
    for name in &agent_names {
        match crate::backend::for_name(name) {
            Ok(b) => check(
                b.is_installed(),
                &format!("agent `{}`: the `{}` CLI is on PATH", b.name(), b.bin()),
                "install it and make sure `--version` works",
                &mut fail,
            ),
            Err(e) => check(false, &format!("agent `{name}` is known"), &format!("{e}"), &mut fail),
        }
    }

    // 2b) a Codex verifier reads no config file: a provider only the user's Codex config names is missed (a warning)
    if agent_names.iter().any(|n| n == "codex") {
        if let Some(missed) = crate::backend::overrides::codex_provider_missed() {
            eprintln!("  ⚠ {missed}");
        }
    }

    // 3) build the run-set engine + parse the sequence exactly as `rlt run` does — this resolves
    //    every verifier name to a file, validates the sequence, and checks the done/abort expressions.
    if let Some(c) = &cfg {
        match crate::loop_::assemble(c, config_base) {
            Ok(asm) => {
                check(true, &format!("sequence + verifiers resolve ({} verifier(s))", asm.engine.verifiers.len()), "", &mut fail);
                // 3b) the chosen agent(s) can do what the config asks.
                match crate::capability::check(c, &asm.engine.verifiers, dir) {
                    Ok(()) => check(true, "the agent(s) can do everything this config asks", "", &mut fail),
                    Err(e) => check(false, "the agent(s) support what this config asks", &format!("{e}"), &mut fail),
                }
            }
            Err(e) => check(false, "sequence + verifiers resolve", &format!("{e}"), &mut fail),
        }

        // 3c) blast-radius isolation: if any step asks for `isolation: sandbox`, an OS wrapper
        //     (bwrap / sandbox-exec) must be present — for EVERY agent, since §2.4 rlt wraps them
        //     all rather than trusting an agent's own sandbox. Checked only when the config asks.
        let wants_wrapper = c
            .steps
            .keys()
            .filter_map(|n| c.resolve_step(n).ok())
            .any(|s| s.isolation == crate::isolation::Isolation::Sandbox);
        if wants_wrapper {
            check(
                crate::isolation::available(),
                "OS sandbox wrapper present for `isolation: sandbox` (bubblewrap / sandbox-exec)",
                "install bubblewrap (`apt install bubblewrap`) on Linux, or ensure `sandbox-exec` is on PATH (macOS)",
                &mut fail,
            );
        }

        // 3d) …and the rung above: `isolation: container` needs an engine whose daemon ANSWERS.
        //     Same gating rule — only checked when the config actually asks for the tier.
        let wants_engine = c
            .steps
            .keys()
            .filter_map(|n| c.resolve_step(n).ok())
            .any(|s| s.isolation == crate::isolation::Isolation::Container);
        if wants_engine {
            check(
                crate::isolation::container_available(),
                "container engine running for `isolation: container` (docker / podman)",
                "start the engine (`colima start`, Docker Desktop, `podman machine start`) or install one",
                &mut fail,
            );
        }

        // 4) the forward state file exists (named by `defaults.state`, resolved against rlt/).
        let sp = config_base.join(&c.defaults.state);
        check(
            sp.exists(),
            &format!("state file `{}` exists", c.defaults.state),
            "create it (or run `rlt init`); the agent maintains it as forward state",
            &mut fail,
        );
    }

    // 5) are the /rlt:* skills where the worker agent looks? Reported, never failed. Report on the
    //    WORKER agent (`defaults.agent`) specifically — NOT `agent_names().first()`, which sorts, so
    //    a codex-worker/claude-verifier config would misreport "claude" (the human invokes /rlt:* in
    //    the agent they drive the project with, i.e. the worker).
    let name = match &cfg {
        Some(c) => c.defaults.agent.clone(),
        None => RelentlessConfig::agent_name(config),
    };
    let (proj, user) = (crate::skills::installed(&name, dir, false), crate::skills::installed(&name, dir, true));
    if proj || user {
        eprintln!("  ✔ the /rlt:* skills are installed for `{name}`");
    } else {
        eprintln!("  · the /rlt:* skills are not installed for `{name}` — optional; `rlt skills install` adds them");
    }

    eprintln!();
    if fail == 0 {
        eprintln!("✔ all checks passed — you're ready: `rlt plan` then `rlt run`.");
        Ok(())
    } else {
        anyhow::bail!("{fail} check(s) failed — fix the items marked ✗ above, then re-run `rlt doctor`.");
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
