//! `agg doctor` — preflight diagnosis. Answers "am I set up right?" in one command.

use crate::core::config::AggConfig;
use anyhow::Result;
use std::path::Path;

pub fn run(dir: &Path, config_base: &Path, config: &Path) -> Result<()> {
    eprintln!("agg doctor — checking your setup in {}\n", dir.display());
    eprintln!("  config dir: {}/", crate::paths::CONFIG_DIR);
    let mut fail = 0;

    // 1) agg.yaml present + parses (the new single config; goals.yaml is gone).
    let cfg = match (config.exists(), AggConfig::load(config)) {
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

    // 2) EVERY agent the sequence names is on PATH (§7.3) — worker default, ruler, per-step agents.
    let agent_names: Vec<String> = match &cfg {
        Some(c) => c.agent_names(),
        None => vec![AggConfig::agent_name(config)],
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

    // 3) build the run-set engine + parse the sequence exactly as `agg run` does — this resolves
    //    every judge name to a file, validates the sequence, and checks the done/abort expressions.
    if let Some(c) = &cfg {
        match crate::loop_::assemble(c, config_base) {
            Ok(asm) => {
                check(true, &format!("sequence + judges resolve ({} judge(s))", asm.engine.judges.len()), "", &mut fail);
                // 3b) the chosen agent(s) can do what the config asks.
                match crate::capability::check(c, &asm.engine.judges) {
                    Ok(()) => check(true, "the agent(s) can do everything this config asks", "", &mut fail),
                    Err(e) => check(false, "the agent(s) support what this config asks", &format!("{e}"), &mut fail),
                }
            }
            Err(e) => check(false, "sequence + judges resolve", &format!("{e}"), &mut fail),
        }

        // 3c) blast-radius isolation: if any step asks for `isolation: sandbox` on an agent that
        //     does NOT self-sandbox, an OS wrapper (bwrap / sandbox-exec) must be present — else the
        //     worker would run unconfined. Checked only when the config actually requests it.
        let wants_wrapper = c
            .steps
            .keys()
            .filter_map(|n| c.resolve_step(n).ok())
            .any(|s| {
                s.isolation == crate::isolation::Isolation::Sandbox
                    && s.backend().map(|b| !b.self_sandboxes()).unwrap_or(false)
            });
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

        // 4) the forward state file exists (named by `defaults.state`, resolved against agg/).
        let sp = config_base.join(&c.defaults.state);
        check(
            sp.exists(),
            &format!("state file `{}` exists", c.defaults.state),
            "create it (or run `agg init`); the agent maintains it as forward state",
            &mut fail,
        );
    }

    // 5) are the /agg:* skills where the worker agent looks? Reported, never failed. Report on the
    //    WORKER agent (`defaults.agent`) specifically — NOT `agent_names().first()`, which sorts, so
    //    a codex-worker/claude-judge config would misreport "claude" (the human invokes /agg:* in
    //    the agent they drive the project with, i.e. the worker).
    let name = match &cfg {
        Some(c) => c.defaults.agent.clone(),
        None => AggConfig::agent_name(config),
    };
    let (proj, user) = (crate::skills::installed(&name, dir, false), crate::skills::installed(&name, dir, true));
    if proj || user {
        eprintln!("  ✔ the /agg:* skills are installed for `{name}`");
    } else {
        eprintln!("  · the /agg:* skills are not installed for `{name}` — optional; `agg skills install` adds them");
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
