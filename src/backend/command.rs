//! The generic command harness: any agent CLI, described in YAML, driven as a worker.
//!
//! ```yaml
//! agents:
//!   my-cli:
//!     command: ["my-cli", "--yes", "--message-file", "{brief_file}"]
//!     cwd: "{workdir}"
//!     output: text                  # text | json_lines (each line one JSON object; `usage` and `session_id` are read)
//!     capabilities: { resume: false, usage: false, cost: false }
//!     rate_limit_pattern: "(?i)rate limit|429"      # optional
//!     preflight: ["my-cli", "--version"]            # optional
//!     env: { MY_CLI_NO_TELEMETRY: "1" }
//! ```
//!
//! It is honest: it sets the capability flags it is given, shows each output line on the dashboard, and the exit code
//! decides success. It cannot give usage accounting (unless the lines carry it), resume (unless told), or rate-limit
//! detection without a pattern, and it is no ruler: it makes no read-only one-shot call, so rubric verifiers and the
//! summarizer stay on claude, codex or copilot.

use super::{stream, AgentBackend, Capabilities, OneShot, SessionReport, SessionSpec};
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

/// How the CLI's output lines are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// every line is shown as it is
    #[default]
    Text,
    /// every line is a JSON object: its text is shown, tokens, cost and session are read (see `fields`)
    JsonLines,
}

/// What the CLI can give back; each defaults to no.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandCaps {
    /// can continue a session (`{session}` in the command is then the session to continue)
    #[serde(default)]
    pub resume: bool,
    /// its `json_lines` output reports output tokens (`usage.output_tokens`)
    #[serde(default)]
    pub usage: bool,
    /// its `json_lines` output reports a dollar cost (`total_cost_usd`)
    #[serde(default)]
    pub cost: bool,
}

/// Where `json_lines` output holds what the harness reads. Each entry is a JSON pointer, or `type=/pointer` to look only
/// at the lines whose `type` is that; the first entry that finds a value wins. An empty list means the default.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    /// the text to show (default: `/text`, `/message`)
    #[serde(default)]
    pub text: Vec<String>,
    /// the output tokens a line reports (default: `/usage/output_tokens`, `/output_tokens`)
    #[serde(default)]
    pub tokens: Vec<String>,
    /// the dollar cost the session reports (default: `/total_cost_usd`)
    #[serde(default)]
    pub cost: Vec<String>,
    /// the session's id (default: `/session_id`, `/sessionId`)
    #[serde(default)]
    pub session: Vec<String>,
}

/// The values `fields` (or its `default`) points at in the line `v`, in order.
fn picks<'a>(v: &'a Value, fields: &[String], default: &[&str]) -> Vec<&'a Value> {
    let own: Vec<&str> = fields.iter().map(String::as_str).collect();
    let paths = if own.is_empty() { default } else { &own };
    paths
        .iter()
        .filter_map(|p| {
            let (kind, pointer) = match p.split_once('=') {
                Some((k, rest)) if !k.starts_with('/') => (Some(k), rest),
                _ => (None, *p),
            };
            if kind.is_some_and(|k| v.get("type").and_then(Value::as_str) != Some(k)) {
                return None;
            }
            v.pointer(pointer)
        })
        .collect()
}

/// One agent CLI, as a config describes it.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// the argv, with `{brief_file}` (a file holding the step's brief), `{brief}` (its text), `{workdir}` (the project),
    /// `{model}` and `{session}` filled in
    pub command: Vec<String>,
    /// where it runs (default: the project); the same placeholders
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub output: Output,
    /// where its `json_lines` hold the text, tokens, cost and session (the defaults suit a Claude-like stream)
    #[serde(default)]
    pub fields: Fields,
    #[serde(default)]
    pub capabilities: CommandCaps,
    /// a line that matches this reads as a rate limit (when the session also exits non-zero)
    #[serde(default)]
    pub rate_limit_pattern: Option<String>,
    /// the command `rlt doctor` and the preflight run (default: `<program> --version`)
    #[serde(default)]
    pub preflight: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// built from the CLI's documented output, not yet from a recorded session
    #[serde(default)]
    pub experimental: bool,
}

/// A configured agent CLI, ready to run as a worker.
#[derive(Clone)]
pub struct CommandBackend {
    name: &'static str,
    bin: &'static str,
    spec: Arc<CommandSpec>,
    limit: Option<Arc<Regex>>,
}

/// A name or a program that lives as long as the process (a handful per run): the trait wants `&'static str`.
fn leaked(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

impl CommandBackend {
    /// The agent `name` as `spec` describes it; an error for an empty command or a pattern that isn't a regex.
    pub fn new(name: &str, spec: CommandSpec) -> Result<CommandBackend> {
        let Some(program) = spec.command.first() else { anyhow::bail!("agents.{name}: `command` is empty") };
        let limit = match &spec.rate_limit_pattern {
            Some(p) => Some(Arc::new(Regex::new(p).map_err(|e| anyhow::anyhow!("agents.{name}.rate_limit_pattern: {e}"))?)),
            None => None,
        };
        Ok(CommandBackend { name: leaked(name), bin: leaked(program), spec: Arc::new(spec), limit })
    }

    /// The command line for one session: the spec's argv with the placeholders filled in.
    fn argv(&self, spec: &SessionSpec) -> Vec<String> {
        let brief_file = brief_file(spec);
        let fill = |text: &str| {
            text.replace("{brief_file}", &brief_file)
                .replace("{brief}", spec.prompt)
                .replace("{workdir}", &spec.cwd.display().to_string())
                .replace("{model}", spec.model)
                .replace("{session}", spec.resume_id.unwrap_or(""))
        };
        self.spec.command.iter().map(|a| fill(a)).collect()
    }
}

/// The step's brief as a file the CLI can read, under the project's private folder (read by the worker, written by
/// rlt): `rlt/private/brief.md`; the path, empty where it can't be written.
fn brief_file(spec: &SessionSpec) -> String {
    let path = crate::paths::private_dir(spec.cwd).join("harness-brief.md");
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    match std::fs::write(&path, spec.prompt) {
        Ok(()) => path.display().to_string(),
        Err(_) => String::new(),
    }
}

impl AgentBackend for CommandBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn bin(&self) -> &'static str {
        self.bin
    }

    fn capabilities(&self) -> Capabilities {
        let c = self.spec.capabilities;
        Capabilities {
            reports_output_tokens: c.usage && self.spec.output == Output::JsonLines,
            reports_cost_usd: c.cost && self.spec.output == Output::JsonLines,
            supports_resume: c.resume,
            detects_rate_limits: self.limit.is_some(),
            experimental: self.spec.experimental,
            ..Capabilities::default()
        }
    }

    fn default_model(&self) -> &'static str {
        ""
    }

    fn default_summary_model(&self) -> &'static str {
        ""
    }

    fn session_command(&self, spec: &SessionSpec) -> Command {
        let argv = self.argv(spec);
        let mut command = Command::new(&argv[0]);
        // stdout is what the reader shows; stdin closed (a CLI that waits for input would hang); stderr goes to the run's log
        command.args(&argv[1..]).args(spec.extra_args).envs(&self.spec.env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let cwd = self.spec.cwd.as_ref().map(|c| c.replace("{workdir}", &spec.cwd.display().to_string()));
        command.current_dir(cwd.as_deref().map(Path::new).unwrap_or(spec.cwd));
        command
    }

    fn parse_event(&self, line: &str) -> Option<stream::Event> {
        let shown = match self.spec.output {
            Output::Text => line.to_string(),
            Output::JsonLines => {
                let v: Value = serde_json::from_str(line).ok()?;
                picks(&v, &self.spec.fields.text, &["/text", "/message"]).into_iter().find_map(Value::as_str)?.to_string()
            }
        };
        let text = shown.trim().to_string();
        (!text.is_empty()).then(|| stream::Event {
            display: format!("  💬 {text}"),
            kind: stream::EventKind::Think,
            text: text.clone(),
            is_result: false,
            thought: Some(text),
        })
    }

    fn parse_usage(&self, line: &str) -> Option<crate::usage::Tokens> {
        if !self.capabilities().reports_output_tokens {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        let n = picks(&v, &self.spec.fields.tokens, &["/usage/output_tokens", "/output_tokens"]).into_iter().find_map(Value::as_u64)?;
        Some(crate::usage::Tokens { output: Some(n), ..Default::default() })
    }

    fn parse_session_id(&self, line: &str) -> Option<String> {
        if !self.spec.capabilities.resume || self.spec.output != Output::JsonLines {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        picks(&v, &self.spec.fields.session, &["/session_id", "/sessionId"]).into_iter().find_map(Value::as_str).map(str::to_string)
    }

    fn parse_result(&self, line: &str) -> Option<SessionReport> {
        let cost = (self.capabilities().reports_cost_usd)
            .then(|| {
                let v: Value = serde_json::from_str(line).ok()?;
                picks(&v, &self.spec.fields.cost, &["/total_cost_usd"]).into_iter().find_map(Value::as_f64)
            })
            .flatten();
        let limited = self.limit.as_ref().is_some_and(|re| re.is_match(line));
        (limited || cost.is_some()).then(|| SessionReport {
            cost_usd: cost,
            rate_limited: limited,
            rate_limit_reset: None,
            error: limited.then(|| line.to_string()),
        })
    }

    fn one_shot(
        &self,
        _prompt: &str,
        _model: &str,
        _effort: &str,
        _timeout_secs: u64,
        _cwd: Option<&Path>,
        _isolation: crate::isolation::Isolation,
        _schema: Option<&str>,
    ) -> Result<OneShot, String> {
        Err(format!("agent `{}` is a command agent: it makes no read-only one-shot call, so it can't be a ruler", self.name))
    }

    fn is_installed(&self) -> bool {
        self.probe().is_ok()
    }

    fn preflight(&self) -> Result<()> {
        self.probe().map_err(|e| anyhow::anyhow!("agent `{}`: {e}", self.name))
    }
}

impl CommandBackend {
    /// Runs the preflight command (default `<program> --version`): its failure says why the agent isn't usable.
    fn probe(&self) -> Result<(), String> {
        let argv = self.spec.preflight.clone().unwrap_or_else(|| vec![self.bin.to_string(), "--version".into()]);
        let Some((program, args)) = argv.split_first() else { return Err("`preflight` is empty".into()) };
        let status = Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("`{}` exited {}", argv.join(" "), s.code().unwrap_or(-1))),
            Err(e) => Err(format!("`{program}` could not run: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(extra: &str) -> CommandSpec {
        serde_yaml::from_str(&format!("command: [\"my-cli\", \"--file\", \"{{brief_file}}\", \"--in\", \"{{workdir}}\"]\n{extra}")).unwrap()
    }

    fn session(cwd: &Path) -> SessionSpec<'_> {
        SessionSpec { prompt: "do it", model: "m1", effort: "", resume_id: Some("s-9"), fork: false, user_settings: true,
                      system_prompt: None, extra_args: &[], cwd, isolation: crate::isolation::Isolation::None }
    }

    /// The command is the template with the brief in a file, the workdir and the placeholders filled in.
    #[test]
    fn the_command_is_the_template_filled_in_with_the_brief_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let b = CommandBackend::new("my-cli", spec("env: { A: \"1\" }\n")).unwrap();
        let c = b.session_command(&session(dir.path()));
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "my-cli");
        assert_eq!(args[0], "--file");
        assert!(args[1].ends_with("harness-brief.md") && std::fs::read_to_string(&args[1]).unwrap() == "do it");
        assert_eq!(args[3], dir.path().display().to_string());
        assert!(c.get_envs().any(|(k, v)| k == "A" && v == Some("1".as_ref())));
        assert_eq!((b.name(), b.bin()), ("my-cli", "my-cli"));
    }

    /// Text lines are shown as they are; json_lines show their text and carry usage and the session when asked to.
    #[test]
    fn the_output_is_read_as_text_or_as_json_lines_with_what_the_spec_claims() {
        let text = CommandBackend::new("t", spec("")).unwrap();
        assert_eq!(text.parse_event("working…").unwrap().text, "working…");
        assert!(text.parse_event("   ").is_none());
        assert!(text.parse_usage("{\"usage\":{\"output_tokens\":5}}").is_none(), "text output reports no usage");
        let json = CommandBackend::new("j", spec("output: json_lines\ncapabilities: { resume: true, usage: true, cost: true }\n")).unwrap();
        assert_eq!(json.parse_event("{\"text\":\"hello\"}").unwrap().text, "hello");
        assert!(json.parse_event("not json").is_none() && json.parse_event("{\"other\":1}").is_none());
        assert_eq!(json.parse_usage("{\"usage\":{\"output_tokens\":7}}").unwrap().output, Some(7));
        assert_eq!(json.parse_session_id("{\"session_id\":\"abc\"}").as_deref(), Some("abc"));
        assert_eq!(json.parse_result("{\"total_cost_usd\":0.25}").unwrap().cost_usd, Some(0.25));
        let caps = json.capabilities();
        assert!(caps.reports_output_tokens && caps.reports_cost_usd && caps.supports_resume && !caps.supports_one_shot);
        assert!(!text.capabilities().reports_output_tokens && !text.capabilities().detects_rate_limits);
    }

    /// A rate-limit pattern marks the line; a spec's mistakes are refused with where.
    #[test]
    fn a_pattern_detects_a_rate_limit_and_a_bad_spec_is_refused() {
        let b = CommandBackend::new("r", spec("rate_limit_pattern: \"(?i)rate limit|429\"\n")).unwrap();
        let hit = b.parse_result("Error: Rate Limit reached").unwrap();
        assert!(hit.rate_limited && hit.error.as_deref() == Some("Error: Rate Limit reached"));
        assert!(b.parse_result("all fine").is_none() && b.capabilities().detects_rate_limits);
        assert!(CommandBackend::new("e", CommandSpec { command: vec![], ..spec("") }).err().unwrap().to_string().contains("`command` is empty"));
        assert!(CommandBackend::new("x", spec("rate_limit_pattern: \"(\"\n")).is_err());
        assert!(serde_yaml::from_str::<CommandSpec>("command: [a]\nbogus: 1\n").is_err());
        assert!(b.one_shot("p", "m", "", 5, None, crate::isolation::Isolation::None, None).err().unwrap().contains("can't be a ruler"));
    }

    /// The probe runs the configured command; a program that isn't there says so.
    #[test]
    fn the_preflight_runs_the_configured_command() {
        let ok = CommandBackend::new("p", spec("preflight: [\"true\"]\n")).unwrap();
        assert!(ok.is_installed() && ok.preflight().is_ok());
        let bad = CommandBackend::new("q", spec("preflight: [\"false\"]\n")).unwrap();
        assert!(!bad.is_installed() && bad.preflight().unwrap_err().to_string().contains("exited 1"));
        let missing = CommandBackend::new("r", spec("preflight: [\"/no/such/program\"]\n")).unwrap();
        assert!(missing.preflight().unwrap_err().to_string().contains("could not run"));
    }
}
