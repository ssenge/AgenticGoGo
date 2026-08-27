//! AgenticGoGo (`agg`) — the CLI entry point.
//!
//! A thin clap front-end over the `agg` library: it parses subcommands (init/doctor/plan/
//! status/run/dashboard/stop/spawn/send), resolves the project paths, and dispatches into the
//! harness. The orchestration itself lives in the library crate.

// The harness lives in the library crate (`agg`); `main.rs` is the thin CLI over it. Only the
// modules the CLI actually touches are imported here.
use agg::core::{config, engine, judge};
use agg::os::{detach, spawns};
use agg::ui::{dashboard, status};
use agg::{bus, doctor, init, loop_, project, skills, state};

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
    /// path to harness config (default: <dir>/agg/agg.yaml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a starter agg.yaml + AGG.md + state/STATE.md + a judge in this dir.
    Init {
        /// which agent the scaffold should target (default: the agent whose shell you are in,
        /// else claude). The template differs: codex omits `model:`, and only claude may set `cost:`.
        #[arg(long)]
        agent: Option<String>,
        /// overwrite existing config files
        #[arg(long)]
        force: bool,
    },
    /// Diagnose your setup (the agent is on PATH and can do what your config asks, config
    /// parses, conditions valid, skills installed).
    Doctor,
    /// Evaluate every judge once and print the starting scoreboard (a dry run — RE-RUNS judges).
    Plan,
    /// Print the running loop's latest scoreboard from its published snapshot (cheap — does NOT
    /// re-run judges; reads agg/private/state.json, same as the /agg:status skill).
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
        /// run in the background: detach, write agg/private/run.pid, log to agg/private/run.log.
        #[arg(long, short = 'd')]
        detach: bool,
    },
    /// Run ONE judge once (resolved by NAME from disk) and print its raw verdict JSON + a human
    /// line — for authoring or debugging a single judge without running the whole `plan`.
    Judge {
        /// the judge name to resolve (agg/judges/<name>.{sh,md} → ~/.agg/judges/<name>.*)
        name: String,
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
    /// Send a steering command to a RUNNING workflow's bus (applied at the next session boundary).
    ///
    /// Every `send` requires a workflow to be running — a steering message with nothing to steer is
    /// not queued, it is a landmine that fires at some unrelated future startup.
    #[command(subcommand)]
    Send(SendCmd),
    /// Answer an open human ask (`agg status` lists the ids).
    ///
    /// NOT under `send`, deliberately: an answer is not a steering message. It is a durable fact
    /// recorded in the ask ledger, it outlives the workflow that asked, and it therefore works
    /// whether or not one is running. For a `choose`/`bool` ask the value must be ON THE LIST —
    /// anything else is refused and the options re-printed, so a driver can never be handed a value
    /// it did not offer. A yes/no ask takes `yes`/`no` (or `1`/`2`).
    Answer {
        /// the ask id, as printed by `agg status` or by the workflow
        id: String,
        /// the answer: a value, an option name, or the option's 1-based number
        value: String,
        /// who answered (recorded in the ledger for the audit trail)
        #[arg(long, default_value = "operator")]
        by: String,
    },
    /// Ask a human for something — for the WORKER to invoke. Records the ask and exits immediately.
    #[command(subcommand)]
    Hil(HilCmd),
    /// Install the `/agg:*` setup skills where your agent will actually find them.
    #[command(subcommand)]
    Skills(SkillsCmd),
}

/// Managing the `/agg:*` skills (`/agg:new`, `/agg:status`, `/agg:supervise`).
#[derive(Subcommand)]
enum SkillsCmd {
    /// Copy the `/agg:*` skills into the directory the chosen agent discovers.
    ///
    /// claude reads `.claude/skills/`; codex and copilot read `.agents/skills/`. Claude and Copilot
    /// invoke them as slash commands (`/agg-new`); codex uses a DOLLAR prefix (`$agg-new`).
    Install {
        /// which agent to install for. Default: the `agent:` key in agg.yaml if it exists, else
        /// the agent whose shell you are running in. Installing for the wrong agent puts the files
        /// where it will never look, so agg asks rather than guess.
        #[arg(long)]
        agent: Option<String>,
        /// install for your whole user account (under $HOME) instead of just this project
        #[arg(long)]
        user: bool,
    },
}

/// Steering commands the operator (or an outer supervisor session) can send to a running loop.
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

/// The WORKER's ask front-end (`internal/HUMAN_LOOP.md` §4.8).
///
/// ⛔ These RECORD AND EXIT. They never wait, and there is deliberately no `--wait`: a worker session
/// is a paid subprocess holding a git branch, so a worker that waits on a human is the exact failure
/// agg exists to replace. Only a driver author — in Rust, at a call site they wrote — can make the
/// loop block. Ask, end your session, and the answer arrives in the NEXT session's brief.
#[derive(Subcommand)]
enum HilCmd {
    /// Ask a yes/no question.
    Bool { question: String },
    /// Ask the human to pick one of `--option`.
    Choose {
        question: String,
        #[arg(long = "option", required = true)]
        options: Vec<String>,
    },
    /// Ask for a value.
    ///
    /// ⛔ NEVER for a secret. The answer is written to the ask ledger and to the next session's
    /// brief, both files on disk. Ask the human to put the credential where credentials go and
    /// confirm with `agg hil bool` instead: an answer may NAME a secret, never CONTAIN one.
    Input { question: String },
}

struct Paths {
    /// project root (cwd for judges + worker; runtime state lives in `<dir>/agg/{state,private}/`).
    dir: PathBuf,
    /// where user inputs live: `<dir>/agg/`. The state file, project judges, and LLM-judge rubric
    /// files resolve against this base.
    config_base: PathBuf,
    config: PathBuf,
}

impl Cli {
    fn paths(&self) -> Paths {
        let dir = self.dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let config_base = agg::paths::config_base(&dir);
        // An explicit --config wins; otherwise resolve inside the mandatory `agg/` folder.
        let config = self.config.clone().unwrap_or_else(|| config_base.join("agg.yaml"));
        Paths { dir, config_base, config }
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
        Cmd::Init { agent, force } => init::run(&p.dir, *force, agent.as_deref()),
        Cmd::Doctor => doctor::run(&p.dir, &p.config_base, &p.config),
        Cmd::Plan => {
            no_config_hint(&p.config)?;
            let cfg = config::AggConfig::load(&p.config)?;
            // build the run-set engine exactly as the loop does (resolves judges by name).
            let asm = loop_::assemble(&cfg, &p.config_base)?;
            // `plan` RE-RUNS the judges, so an LLM judge would make a real model call — refuse a
            // config the chosen agent(s) cannot honour first.
            agg::capability::check(&cfg, &asm.engine.judges)?;
            let ruler = cfg.ruler_backend()?;
            let jm = cfg.judge_model(ruler);
            let mut eng = asm.engine;
            eprintln!("agg: evaluating {} judge(s) once (dry run)…", eng.judges.len());
            // ponytail: UNCONFINED, unlike the baseline pass (features/setup.rs) which now uses the
            // run-level tier. Two things make that safe here and not there: `plan` never writes
            // `verdicts.jsonl`, so a tampered judge has no ledger to forge; and it is
            // OPERATOR-invoked, so running it is the same act of trust as running any script in a
            // repo you are working on. The residual is that a judge tampered with by an earlier
            // sandboxed run is arbitrary code on the operator's host. Upgrade path if that matters:
            // pass `cfg.run_isolation()` here and in `agg judge` below, exactly as Baseline does.
            let res = eng.run_step(&p.dir, &engine::RunState::default(), ruler, &jm, cfg.judge.timeout, "plan", None, false, agg::isolation::Isolation::None);
            print!("{}", eng.scoreboard());
            // Non-terminal, so it is reported ALONGSIDE the verdict, not instead of it: knowing a
            // detector already fires is worth having before committing to an overnight run.
            if let Some(reason) = &res.notify {
                println!("\n⚑ notify_if is already true: {reason}");
            }
            if res.halt {
                println!("\n⚠ abort_if is already true: {}", res.halt_reason.unwrap_or_default());
            } else if res.stop {
                println!("\n✔ done_if already satisfied — nothing to run.");
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
        Cmd::Judge { name } => {
            // resolve the judge by NAME from disk (§5.1) — no goals.yaml. Ensure the standard
            // library exists so a library judge name resolves.
            let _ = agg::core::judges::ensure_library();
            let kind = agg::core::judges::resolve(name, &p.config_base)?;
            // run it exactly as the loop would (scripts from the project root, rubric on the RULER).
            let ruler = ruler_for(&p.config)?;
            let (jm, timeout) = match config::AggConfig::load(&p.config) {
                Ok(c) => (c.judge_model(ruler), c.judge.timeout),
                Err(_) => (ruler.default_summary_model().to_string(), 300),
            };
            // Manual `agg judge` — no worker ran this session, so no confinement is needed.
            // No run-set here (this is one judge, by hand), so a native judge's ctx would have
            // nothing to consult — and `agg judge` resolves by file extension, which cannot
            // produce one. `None` iso_base ⇒ `diff()` is the working tree against HEAD.
            let (verdict, _spend) = judge::run(
                &kind,
                name,
                &p.dir,
                ruler,
                &jm,
                timeout,
                None,
                "judge",
                agg::isolation::Isolation::None,
                &agg::core::judge::NoJudges,
                None,
            );
            // raw verdict JSON (the judge contract), then a one-line human summary.
            println!("{}", serde_json::to_string(&verdict).unwrap_or_else(|_| "{}".into()));
            let mark = if verdict.met { "✔ met" } else { "✖ not met" };
            eprintln!(
                "  {mark} — {} (value {} / target {}{})",
                if verdict.rationale.is_empty() { "(no rationale)" } else { &verdict.rationale },
                verdict.value.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                verdict.target,
                verdict.error.as_ref().map(|e| format!("; ERROR: {e}")).unwrap_or_default(),
            );
            Ok(())
        }
        Cmd::Run { max_sessions, detach } => {
            no_config_hint(&p.config)?;
            let agg_cfg = config::AggConfig::load(&p.config)?;
            // build the run-set engine + parse the sequence (validates step names / judge files /
            // the not-all-skip rule in the FOREGROUND — a typo fails loudly here, not inside a
            // detached child the user can't see).
            let asm = loop_::assemble(&agg_cfg, &p.config_base)?;

            // …then REFUSE anything the config asks for that the chosen agent(s) cannot do.
            agg::capability::check(&agg_cfg, &asm.engine.judges)?;
            // EVERY agent the sequence names must be on PATH (§7.3), before launching the loop.
            for name in agg_cfg.agent_names() {
                agg::backend::for_name(&name)?.preflight()?;
            }

            if *detach {
                return detach::spawn_detached(&p.dir).map(|_| ExitCode::SUCCESS);
            }
            let outcome = loop_::run(agg_cfg, asm, &p.dir, &p.config_base, *max_sessions)?;
            return Ok(ExitCode::from(outcome.exit_code()));
        }
        Cmd::Serve { port, cors_origin, token } => {
            agg::ui::serve::run(agg::ui::serve::ServeConfig {
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
        Cmd::Answer { id, value, by } => answer_ask(&p.dir, id, value, by),
        Cmd::Hil(hil) => worker_ask(&p.dir, hil),
        Cmd::Skills(SkillsCmd::Install { agent, user }) => {
            // Resolving the agent is the whole correctness of this command: install for the wrong
            // one and the files land in a directory that agent never reads, so the user sees no
            // skill and NO ERROR. This runs at SETUP time, so agg.yaml usually does not exist yet —
            // and `AggConfig::agent_name` answers `claude` for a missing file. Silently trusting it
            // sent every Codex user's skills to `.claude/skills/`. So: config first (explicit
            // intent), then the agent we are actually RUNNING inside, and only then give up.
            let agent = match agent.clone() {
                Some(a) => a,
                None if p.config.exists() => config::AggConfig::agent_name(&p.config),
                None => skills::host_agent()
                    .map(str::to_string)
                    .context(
                        "cannot tell which agent to install the skills for.\n  \
                         There is no agg.yaml yet (so no `agent:` key), and this shell is not \
                         inside a known agent.\n  Name it explicitly:\n    \
                         agg skills install --agent claude|codex|copilot",
                    )?,
            };
            let root = skills::install(&agent, &p.dir, *user)?;
            eprintln!("installed the /agg:* skills for `{agent}` → {}", root.display());
            for (name, _) in skills::SKILLS {
                eprintln!("  ✔ {name}");
            }
            // The PREFIX is not the same on each agent, and telling someone to type a command their
            // agent does not have is the difference between "it works" and "that does nothing".
            // Claude: `/agg-new` here (the `agg:` in `/agg:new` comes from the PLUGIN namespace, so
            // a copy-in install does not get it). Copilot: every skill is a slash command
            // (`userInvocable` in its own SDK = "can be invoked by the user as a slash command").
            // Codex: `$agg-new` — it uses `$`, NOT `/`; a `/agg-new` there is "Unrecognized command"
            // (openai/codex#11817, closed as not planned).
            match agent.as_str() {
                "codex" => eprintln!(
                    "\nInvoke them with `$agg-new`, `$agg-status`, `$agg-supervise` \
                     — codex uses `$`, not `/`."
                ),
                "claude" => eprintln!(
                    "\nInvoke them with `/agg-new`, `/agg-status`, `/agg-supervise`.\n\
                     (From the plugin marketplace instead, they are `/agg:new` etc.)"
                ),
                _ => eprintln!("\nInvoke them with `/agg-new`, `/agg-status`, `/agg-supervise`."),
            }
            eprintln!(
                "Or just ask — every agent also picks a skill by matching your request against its\n\
                 description: \"set up AgenticGoGo for this project\" → agg-new."
            );
            Ok(())
        }
    }
    // Every non-`run` subcommand yields `()` on success → exit 0. `agg run` returns its outcome
    // code earlier via an explicit `return`, so it never reaches here.
    .map(|()| ExitCode::SUCCESS)
}

/// The RULER — the backend that runs LLM judges — for a command that may have a missing/broken
/// agg.yaml (`judge` resolves the ruler even without a full parse). Reads `judge.agent`.
fn ruler_for(config: &std::path::Path) -> Result<&'static dyn agg::backend::AgentBackend> {
    agg::backend::for_name(&config::AggConfig::ruler_name(config))
}

/// Queue one steering command onto a running loop's bus. Shared by `agg send …` and
/// the `agg stop` convenience alias.
/// `agg answer <id> <value>` — record a human's answer to an open ask.
///
/// Writes the ask ledger DIRECTLY; nothing goes on the bus. An answer is a durable fact, not a
/// steering message: it outlives the workflow that raised the question (a worker asks, its session
/// ends, the workflow may reach its goal and exit while the question is still open), and a blocked
/// driver polls the ledger rather than the queue. Routing it through the bus is what previously made
/// it the one command needing an exemption from the liveness rule.
///
/// Not a hole in the moat: this is the OPERATOR's command, run outside the worker's jail. A confined
/// worker invoking it is refused by the kernel exactly as before — `agg/private/` is carved out of
/// its writable set.
fn answer_ask(dir: &std::path::Path, id: &str, value: &str, by: &str) -> Result<()> {
    use agg::core::asks;

    // ONE validator, shared with `POST /api/answer`, so a value off a closed option list is refused
    // identically whichever channel it arrives on.
    let canonical = asks::validate_answer(dir, id, value).map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(ask) = asks::get(dir, id) {
        eprintln!("answering `{id}`: {}", asks::one_line(&ask.question, 100));
    }
    asks::answer(dir, id, &canonical, by, agg::util::now_epoch())?;
    println!("recorded: {canonical}");
    Ok(())
}

/// `agg hil …` — the worker's ask front-end. Writes a REQUEST and returns immediately.
///
/// The request lands in `agg/state/asks/` (worker-writable, so it survives `isolation: sandbox`);
/// agg promotes it into the private ledger at the next session boundary and pages the human. The
/// request is untrusted either way — only the answer's channel decides trust.
fn worker_ask(dir: &std::path::Path, hil: &HilCmd) -> Result<()> {
    use agg::core::asks::AskCase;

    let (case, question, options) = match hil {
        HilCmd::Bool { question } => (AskCase::Bool, question, Some(vec!["yes".to_string(), "no".to_string()])),
        HilCmd::Choose { question, options } => {
            if options.len() < 2 {
                anyhow::bail!("`hil choose` needs at least two --option values (got {})", options.len());
            }
            (AskCase::Choose, question, Some(options.clone()))
        }
        HilCmd::Input { question } => (AskCase::Input, question, None),
    };

    let req = agg::core::asks::WorkerRequest {
        case,
        question: question.clone(),
        options,
        ts: agg::util::now_epoch(),
    };
    let path = agg::core::asks::write_worker_request(dir, &req)?;

    // What the worker is told to do next is the whole contract: it must NOT wait.
    println!(
        "ask recorded ({}).\n\
         Do NOT wait for it — end your session now. The answer will be in your next session's \
         instructions.",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("?")
    );
    Ok(())
}

fn send_to_bus(dir: &std::path::Path, cmd: bus::Command) -> Result<()> {
    // The "is a workflow running" rule lives in `bus::queue_command`, so this channel and the web
    // API cannot disagree about it — they used to, and did: the CLI queued with a warning while the
    // API refused, for the very same command.
    let path = agg::bus::queue_command(dir, &cmd)?;
    eprintln!("queued → {} (applied at the next session boundary)", path.display());
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
    std::fs::create_dir_all(&log_dir).with_context(|| "creating agg/state/spawns log dir")?;
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
            c.pre_exec(agg::os::proc::setsid);
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
    .with_context(|| "registering spawn in agg/state/spawns.json")?;

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

