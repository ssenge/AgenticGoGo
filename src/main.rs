//! AgenticGoGo (`agg`) — the CLI entry point.
//!
//! A thin clap front-end over the `agg` library: it parses subcommands (init/doctor/plan/
//! status/run/dashboard/stop/spawn/send), resolves the project paths, and dispatches into the
//! harness. The orchestration itself lives in the library crate.

// The harness lives in the library crate (`agg`); `main.rs` is the thin CLI over it. Only the
// modules the CLI actually touches are imported here.
use agg::{bus, config, dashboard, detach, doctor, engine, init, judge, loop_, project, spawns, state, status};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "agg", version, about = "AgenticGoGo — a generic agent-loop harness")]
struct Cli {
    /// project directory (defaults to current dir)
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    /// path to harness config (default: <dir>/agg.yaml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// path to goals file (default: <dir>/goals.yaml)
    #[arg(long, global = true)]
    goals: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a starter agg.yaml + goals.yaml + AGG_RESUME.md + a judge in this dir.
    Init {
        /// overwrite existing config files
        #[arg(long)]
        force: bool,
        /// scaffold into an `agg/` config folder instead of the project root (keeps the root
        /// tidy when you have judges/ + rubrics/). `agg run` auto-detects either layout.
        #[arg(long)]
        folder: bool,
    },
    /// Diagnose your setup (claude on PATH, config parses, conditions valid, …).
    Doctor,
    /// Evaluate every judge once and print the starting scoreboard (a dry run — RE-RUNS judges).
    Plan,
    /// Print the running loop's latest scoreboard from its published snapshot (cheap — does NOT
    /// re-run judges; reads .agg/state.json, same as the /agg:status skill).
    Status {
        /// emit the raw snapshot as JSON (the full DashboardState) for scripting/piping.
        #[arg(long)]
        json: bool,
    },
    /// Run the loop until the stop condition is met (or halt fires).
    Run {
        /// stop after this many sessions regardless (0 = unlimited)
        #[arg(long, default_value_t = 0)]
        max_sessions: u32,
        /// run in the background: detach, write .agg/run.pid, log to .agg/run.log.
        #[arg(long, short = 'd')]
        detach: bool,
    },
    /// Run ONE goal's judge once and print its raw verdict JSON + a human line — for authoring
    /// or debugging a single judge without running the whole `plan`.
    Judge {
        /// the goal id whose judge to run
        id: String,
    },
    /// Show this project's run history (every `agg run`, newest first) + lifetime totals.
    History {
        /// emit the raw history ledger as JSON (the full Project record) for scripting/piping.
        #[arg(long)]
        json: bool,
    },
    /// Serve a thin JSON HTTP API over this project's live state, for the standalone web UI.
    /// Read-only endpoints (/api/state, /api/history, /api/health) + control (POST /api/send).
    Serve {
        /// port to bind on 127.0.0.1 (the web tool proxies to it)
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// CORS origin allowed to call the API (the web tool's URL). Defaults to the SvelteKit
        /// dev server (http://localhost:5173).
        #[arg(long, default_value = "")]
        cors_origin: String,
        /// bearer token required on every request (empty = no auth, the local default). Set this
        /// when exposing the API beyond localhost.
        #[arg(long, default_value = "")]
        token: String,
    },
    /// Live TUI dashboard — tails the running loop's state. Quit with q.
    Dashboard {
        /// print a one-shot text snapshot to stdout and exit (for headless/CI/SSH — no TUI).
        #[arg(long)]
        once: bool,
    },
    /// Stop a running loop gracefully after its current session (alias of `send stop`).
    /// The ONE blessed top-level alias — every other steering verb lives under `agg send`.
    Stop {
        /// reason (recorded in the finish banner)
        #[arg(default_value = "operator requested stop")]
        reason: String,
    },
    /// Launch a long-running task that OUTLIVES the worker session, tracked so the straggler
    /// reaper spares it and the next session knows it is running (and why). Use this instead
    /// of a hand-rolled `nohup` for any sim/build that takes longer than a single turn.
    Spawn {
        /// short unique handle for the task, e.g. "scaling-20q".
        #[arg(long)]
        name: String,
        /// WHY it is running — surfaced to the next worker so it polls instead of relaunching.
        #[arg(long)]
        reason: String,
        /// the command to run (everything after `--`), e.g. `-- python -m qcbn.scaling 20`.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Send a steering command to a running loop's bus (applied at the next session boundary).
    #[command(subcommand)]
    Send(SendCmd),
}

/// Steering commands the operator (or outer Claude) can send to a running loop.
#[derive(Subcommand)]
enum SendCmd {
    /// Prepend a high-priority instruction to the next worker session.
    Inject {
        /// the instruction text
        text: String,
    },
    /// Change the token budget (omit value for unlimited).
    Budget {
        /// total output-token ceiling
        total: Option<u64>,
    },
    /// Pause the loop before the next session.
    Pause,
    /// Resume a paused loop.
    Resume,
    /// Stop the loop gracefully after the current session boundary.
    Stop {
        /// reason (recorded in the finish banner)
        #[arg(default_value = "operator requested stop")]
        reason: String,
    },
    /// Send a free-form note (logged; no behavior change).
    Note {
        text: String,
    },
}

struct Paths {
    /// project root (cwd for judges + worker; runtime state lives in `<dir>/.agg/`).
    dir: PathBuf,
    /// where user inputs live: `<dir>/agg/` if that folder exists, else `<dir>`. The resume
    /// prompt and LLM-judge rubric files resolve against this base.
    config_base: PathBuf,
    config: PathBuf,
    goals: PathBuf,
}

impl Cli {
    fn paths(&self) -> Paths {
        let dir = self.dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let config_base = agg::paths::config_base(&dir);
        // An explicit --config/--goals wins; otherwise honour the optional `agg/` folder.
        let config = self.config.clone().unwrap_or_else(|| agg::paths::config_file(&dir, "agg.yaml"));
        let goals = self.goals.clone().unwrap_or_else(|| agg::paths::config_file(&dir, "goals.yaml"));
        Paths { dir, config_base, config, goals }
    }
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(code) => code,
        Err(e) => {
            // hard error (a `?` path): print the chain and exit 1 — distinct from the loop's
            // outcome codes (0 goals-met/stopped, 3 halt, 4 max-sessions).
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// The real CLI body. `agg run` maps its [`loop_::RunOutcome`] to an exit code so automation can
/// branch on the result; every other subcommand exits 0 on success (errors bubble up as 1).
fn run_cli() -> Result<ExitCode> {
    let cli = Cli::parse();
    let p = cli.paths();

    match &cli.cmd {
        Cmd::Init { force, folder } => init::run(&p.dir, *force, *folder),
        Cmd::Doctor => doctor::run(&p.dir, &p.config_base, &p.config, &p.goals),
        Cmd::Plan => {
            no_config_hint(&p.goals)?;
            let goals_cfg = config::GoalsConfig::load(&p.goals)?;
            let mut eng = engine::Engine::new(goals_cfg)?;
            eprintln!("agg: evaluating {} goal(s) once (dry run)…", eng.goals.len());
            // dry run: no budget/wall-time accounting (default RunState)
            let res = eng.evaluate_cycle(&p.dir, &p.config_base, &engine::RunState::default());
            print!("{}", eng.scoreboard());
            if res.halt {
                println!("\n⚠ HALT condition is already true: {}", res.halt_reason.unwrap_or_default());
            } else if res.stop {
                println!("\n✔ STOP condition already satisfied — nothing to run.");
            } else {
                let (met, total) = eng.tally();
                println!("\n→ {met}/{total} met; loop would continue. Run `agg run` to start.");
            }
            Ok(())
        }
        // Cheap read of the published snapshot — never re-runs judges (that's `plan`).
        Cmd::Status { json } => {
            if *json {
                println!("{}", status::render_json(&p.dir)?);
            } else {
                print!("{}", status::render(&p.dir));
            }
            Ok(())
        }
        Cmd::History { json } => {
            let proj = project::Project::load(&p.dir);
            if *json {
                println!("{}", serde_json::to_string_pretty(&proj)?);
            } else {
                print!("{}", proj.render_history());
            }
            Ok(())
        }
        Cmd::Judge { id } => {
            no_config_hint(&p.goals)?;
            let goals_cfg = config::GoalsConfig::load(&p.goals)?;
            let goal = goals_cfg.goals.iter().find(|g| &g.id == id).ok_or_else(|| {
                let ids: Vec<&str> = goals_cfg.goals.iter().map(|g| g.id.as_str()).collect();
                anyhow::anyhow!("no goal `{id}` in goals.yaml. available: {}", ids.join(", "))
            })?;
            // run the one judge exactly as the loop would (scripts from the project root,
            // rubric from the config dir).
            let verdict = judge::run(&goal.judge, &p.dir, &p.config_base);
            // raw verdict JSON (the judge contract), then a one-line human summary.
            println!("{}", serde_json::to_string(&verdict).unwrap_or_else(|_| "{}".into()));
            let mark = if verdict.met { "✔ met" } else { "✖ not met" };
            eprintln!(
                "  {mark} — {} (value {} / target {}{})",
                if verdict.rationale.is_empty() { "(no rationale)" } else { &verdict.rationale },
                verdict.value,
                verdict.target,
                verdict.error.as_ref().map(|e| format!("; ERROR: {e}")).unwrap_or_default(),
            );
            Ok(())
        }
        Cmd::Run { max_sessions, detach } => {
            no_config_hint(&p.config)?;
            // agent CLI on PATH BEFORE launching the loop, so a missing binary fails with a
            // clear message up front rather than a buried mid-run "FAILED to spawn".
            agg::backend::preflight()?;
            if *detach {
                // Validate the config NOW (in the foreground) so a typo fails loudly here
                // rather than silently in a detached child the user can't see. Then spawn
                // the loop detached and return — the child re-runs `agg run` for real. A
                // detached run returns before the outcome exists, so consumers poll
                // `agg status --json` / `agg history --json` for the end reason instead.
                let _ = config::AggConfig::load(&p.config)?;
                let _ = config::GoalsConfig::load(&p.goals)
                    .and_then(engine::Engine::new)?;
                return detach::spawn_detached(&p.dir).map(|_| ExitCode::SUCCESS);
            }
            let agg_cfg = config::AggConfig::load(&p.config)?;
            let goals_cfg = config::GoalsConfig::load(&p.goals)?;
            let eng = engine::Engine::new(goals_cfg)?;
            let outcome = loop_::run(agg_cfg, eng, &p.dir, &p.config_base, *max_sessions)?;
            return Ok(ExitCode::from(outcome.exit_code()));
        }
        Cmd::Serve { port, cors_origin, token } => {
            agg::serve::run(agg::serve::ServeConfig {
                dir: p.dir.clone(),
                port: *port,
                cors_origin: cors_origin.clone(),
                token: token.clone(),
            })?;
            Ok(())
        }
        Cmd::Dashboard { once } => {
            if *once {
                // headless one-shot: the same snapshot the TUI renders, to stdout, then exit.
                print!("{}", status::render(&p.dir));
                Ok(())
            } else {
                dashboard::run(&p.dir)
            }
        }
        Cmd::Stop { reason } => send_to_bus(&p.dir, bus::Command::Stop { reason: reason.clone() }),
        Cmd::Spawn { name, reason, cmd } => spawn_task(&p.dir, name, reason, cmd),
        Cmd::Send(send) => {
            let cmd = match send {
                SendCmd::Inject { text } => bus::Command::InjectInstruction { text: text.clone() },
                SendCmd::Budget { total } => bus::Command::SetBudget { total: *total },
                SendCmd::Pause => bus::Command::Pause,
                SendCmd::Resume => bus::Command::Resume,
                SendCmd::Stop { reason } => bus::Command::Stop { reason: reason.clone() },
                SendCmd::Note { text } => bus::Command::Note { text: text.clone() },
            };
            send_to_bus(&p.dir, cmd)
        }
    }
    // Every non-`run` subcommand yields `()` on success → exit 0. `agg run` returns its outcome
    // code earlier via an explicit `return`, so it never reaches here.
    .map(|()| ExitCode::SUCCESS)
}

/// Queue one steering command onto a running loop's bus. Shared by `agg send …` and
/// the `agg stop` convenience alias.
fn send_to_bus(dir: &std::path::Path, cmd: bus::Command) -> Result<()> {
    let live = agg::detach::live_pid(dir).is_some();
    let path = agg::bus::queue_command(dir, &cmd)?;
    if live {
        eprintln!("queued → {} (the loop applies it at the next session boundary)", path.display());
    } else {
        // Liveness guard: no loop is running here. Still queue (pre-arming before `agg run` is a
        // legitimate use), but say so — a `stop` queued now would fire at the NEXT run's startup.
        eprintln!(
            "queued → {} — but NO loop is running in this dir right now.\n  \
             it will apply when one starts (a queued `stop` fires immediately at the next `agg run`;\n  \
             delete .agg/bus/in/*.json to cancel).",
            path.display()
        );
    }
    Ok(())
}

/// Launch a long-running task detached, in its OWN process group, with its own log file,
/// and register it so (a) the straggler reaper spares its pgid and (b) the next worker
/// session is told it is running and why. This is the blessed alternative to a hand-rolled
/// `nohup` — agg launches it, so agg knows its pid/pgid directly (no env marker needed,
/// which is good because macOS blocks reading a process's env).
fn spawn_task(dir: &std::path::Path, name: &str, reason: &str, cmd: &[String]) -> Result<()> {
    use std::process::{Command, Stdio};
    if cmd.is_empty() {
        anyhow::bail!("nothing to spawn — pass the command after `--`, e.g. `agg spawn --name x --reason y -- sleep 60`");
    }
    let log_dir = spawns::Registry::log_dir(dir);
    std::fs::create_dir_all(&log_dir).with_context(|| "creating .agg/spawns log dir")?;
    let log_path = log_dir.join(format!("{name}.log"));
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening spawn log {}", log_path.display()))?;
    let log_err = log.try_clone().with_context(|| "duplicating spawn log handle")?;

    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // OWN session (and therefore its own process group, as leader): decoupled from the
    // worker so the worker's post-session group-kill never touches it, AND it survives the
    // launching shell/worker exiting. `setsid()` alone does both — do NOT also call
    // `process_group(0)`, which would make the child a group leader first and then make
    // `setsid()` fail with EPERM (a group leader can't start a new session).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // new session → new process group (child is leader) → no controlling tty.
        unsafe {
            c.pre_exec(agg::proc::setsid);
        }
    }
    let child = c.spawn().with_context(|| format!("spawning `{}`", cmd.join(" ")))?;
    let pid = child.id();
    // The child is its own group leader, so pgid == pid.
    let pgid = pid;
    // Detach: we do not wait. The child runs on; the registry + next session own its fate.
    std::mem::forget(child);

    // Read the session count from state.json so the breadcrumb says "since session N".
    let started_session = state::DashboardState::read(dir).map(|s| s.session).unwrap_or(0);

    spawns::Registry::register(dir, spawns::SpawnEntry {
        name: name.to_string(),
        pgid,
        pid,
        reason: reason.to_string(),
        cmd: cmd.join(" "),
        log: log_path.to_string_lossy().into_owned(),
        started_session,
        status: "running".into(),
    })
    .with_context(|| "registering spawn in .agg/spawns.json")?;

    eprintln!(
        "▶ spawned `{name}` (pid {pid}, pgid {pgid}) — survives session boundaries.\n  \
         reason: {reason}\n  \
         log:    {}\n  \
         poll:   tail -f {}\n  \
         it is now PROTECTED from the straggler reaper and announced to the next session.",
        log_path.display(),
        log_path.display(),
    );
    Ok(())
}

/// If the required config file is missing, exit with an actionable hint instead of
/// a cryptic "No such file" — point the user at `agg init` / `/agg:new`.
fn no_config_hint(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "no {} here — this directory isn't set up yet.\n  \
             • run `agg init` to scaffold a starter project, or\n  \
             • in Claude Code, run `/agg:new` to generate config from your existing plans.",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("config")
        );
    }
    Ok(())
}

