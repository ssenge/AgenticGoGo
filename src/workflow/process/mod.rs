//! Process files (A-37): `agg process <file> <project>` runs a process of standard steps from YAML. A process is
//! phases of steps, each step one of a closed set of kinds (a script, an agent's work judged by checks and a
//! critic, a human's approval, a loop over a list, an exam), with its prompt, checks, critic and limits as data.
//! The interpreter builds each entry into the block agg already has ([`super::Flow`]); the YAML has no logic of its
//! own. A step's identity is its id; its number and a checkpoint's name are computed from position.
//!
//! `agg process <file> --check` validates the file, `--list` prints its numbered steps as JSON.

pub mod check;
pub mod format;
pub mod ladder;
pub mod number;
pub mod settings;
pub mod template;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Result};

use super::Run;

const USAGE: &str = "usage: agg process <file> <project> [--resume] [--from <step>] [--note <text> | --note-file <file> \
    [--decided]] [--reset] [--until <step>] [--reentries <n>]\n       agg process <file> --check\n       agg process <file> --list";

/// `agg process …`: the arguments after the subcommand. `--check` and `--list` read only the file; otherwise the
/// process runs on the project, and starts again where the task force sent it (`--from`, `--note`).
pub fn command(args: &[String]) -> Result<ExitCode> {
    let positional = positionals(args);
    let Some(file) = positional.first().map(PathBuf::from) else { bail!("{USAGE}") };
    if args.iter().any(|a| a == "--check") {
        return report(&check::check(&file));
    }
    if args.iter().any(|a| a == "--list") {
        let text = std::fs::read_to_string(&file).map_err(|e| anyhow!("{}: {e}", file.display()))?;
        let process = format::Process::parse(&text).map_err(|e| anyhow!("{}: {e}", file.display()))?;
        let list: Vec<serde_json::Value> = number::number(&process).iter().map(number::Numbered::json).collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(ExitCode::SUCCESS);
    }
    let Some(_project) = positional.get(1) else { bail!("{USAGE}") };
    Err(anyhow!("agg process: running a process isn't built yet"))
}

/// The arguments that aren't flags or flags' values: the file, then the project.
fn positionals(args: &[String]) -> Vec<String> {
    args.iter().enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || !Run::VALUED.contains(&args[i - 1].as_str())))
        .map(|(_, a)| a.clone()).collect()
}

/// Prints the problems of `--check` one per line and a verdict JSON last; exit 1 when there are any.
fn report(problems: &[String]) -> Result<ExitCode> {
    for p in problems {
        eprintln!("{p}");
    }
    let verdict = serde_json::json!({"met": problems.is_empty(),
        "rationale": if problems.is_empty() { "the process file is sound".to_string() } else { problems.join("; ") },
        "problems": problems});
    println!("{verdict}");
    Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file and the project are the arguments that no flag takes.
    #[test]
    fn the_file_and_the_project_are_the_positional_arguments() {
        let args: Vec<String> = ["p.yaml", "proj", "--from", "3.4", "--note", "x", "--resume", "--until", "7.1"]
            .map(String::from).to_vec();
        assert_eq!(positionals(&args), ["p.yaml", "proj"]);
    }
}
