//! AgenticGoGo (`agg`) — a generic agent-loop harness for Claude Code workers.
//!
//! Phase 1 walking skeleton. Subcommands:
//!   agg plan    — evaluate every judge once, print the starting scoreboard (dry run)
//!   agg status  — same as plan but intended for quick re-checks
//!   agg run     — the loop: launch worker → judge → check stop → repeat

mod bus;
mod config;
mod dashboard;
mod engine;
mod judge;
mod loop_;
mod model;
mod state;
mod stop;
mod stream;
mod summary;
mod worker;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
    /// Evaluate every judge once and print the starting scoreboard (no worker launched).
    Plan,
    /// Print the current scoreboard (alias of plan for quick re-checks).
    Status,
    /// Run the loop until the stop condition is met (or halt fires).
    Run {
        /// stop after this many sessions regardless (0 = unlimited)
        #[arg(long, default_value_t = 0)]
        max_sessions: u32,
    },
    /// Live TUI dashboard — tails the running loop's state. Quit with q.
    Dashboard,
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
    dir: PathBuf,
    config: PathBuf,
    goals: PathBuf,
}

impl Cli {
    fn paths(&self) -> Paths {
        let dir = self.dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let config = self.config.clone().unwrap_or_else(|| dir.join("agg.yaml"));
        let goals = self.goals.clone().unwrap_or_else(|| dir.join("goals.yaml"));
        Paths { dir, config, goals }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let p = cli.paths();

    match &cli.cmd {
        Cmd::Plan | Cmd::Status => {
            let goals_cfg = config::GoalsConfig::load(&p.goals)?;
            let mut eng = engine::Engine::new(goals_cfg)?;
            eprintln!("agg: evaluating {} goal(s) once (dry run)…", eng.goals.len());
            // dry run: no budget/wall-time accounting (default RunState)
            let res = eng.evaluate_cycle(&p.dir, &engine::RunState::default());
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
        Cmd::Run { max_sessions } => {
            let agg_cfg = config::AggConfig::load(&p.config)
                .with_context(|| "load agg.yaml (run `agg plan` if you only have goals.yaml)")?;
            let goals_cfg = config::GoalsConfig::load(&p.goals)?;
            let eng = engine::Engine::new(goals_cfg)?;
            loop_::run(agg_cfg, eng, &p.dir, *max_sessions)
        }
        Cmd::Dashboard => dashboard::run(&p.dir),
        Cmd::Send(send) => {
            let b = bus::Bus::open(&p.dir).with_context(|| "opening bus (is this a project dir?)")?;
            let cmd = match send {
                SendCmd::Inject { text } => bus::Command::InjectInstruction { text: text.clone() },
                SendCmd::Budget { total } => bus::Command::SetBudget { total: *total },
                SendCmd::Pause => bus::Command::Pause,
                SendCmd::Resume => bus::Command::Resume,
                SendCmd::Stop { reason } => bus::Command::Stop { reason: reason.clone() },
                SendCmd::Note { text } => bus::Command::Note { text: text.clone() },
            };
            // monotonic-ish stamp for send-order filenames (CLI context: SystemTime ok)
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("{:013}", d.as_millis()))
                .unwrap_or_else(|_| "0000000000000".into());
            let path = b.send(&cmd, &stamp)?;
            eprintln!("queued → {} (the loop applies it at the next session boundary)", path.display());
            Ok(())
        }
    }
}
