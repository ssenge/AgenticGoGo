//! Whether a run's worker agent can be reached at all, asked once before the run's first step: one tiny call with the
//! worker's own flags and environment (not a judge's: a judge loads the operator's settings, so it would pass where
//! the worker, started without them, fails), and the cause named when it fails.

use std::path::Path;

use super::{AgentBackend, SessionSpec};

/// The cheapest sensible call.
const PROMPT: &str = "Reply with the single word ok.";
/// Seconds the call may take; a gateway that never answers is a cause too.
const TIMEOUT_SECS: u64 = 180;

/// The text of the agent's error (its first lines) and, where it is an authentication error and the operator's
/// settings `env` has `gateway_keys` that the run does not pass, the setting that passes them.
pub fn explain(said: &str, gateway_keys: &[String]) -> String {
    let lines: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect();
    let mut text = if lines.is_empty() { "the agent failed without saying why".to_string() } else { lines.join("\n  ") };
    let lower = said.to_lowercase();
    let auth = ["invalid api key", "401", "authentication"].iter().any(|m| lower.contains(m));
    if auth && !gateway_keys.is_empty() {
        text.push_str(&format!(
            "\n  the operator's settings env has {}; set rlt: {{ user_env: true }} (or a list) in the process file",
            gateway_keys.join(", ")
        ));
    }
    text
}

/// What the agent said when its call failed: the error of its result event, else its stderr, else its output.
fn said(stdout: &str, stderr: &str) -> String {
    let event_error = stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .find(|v| v["is_error"] == true).and_then(|v| v["result"].as_str().map(str::to_string));
    event_error.unwrap_or_else(|| if stderr.trim().is_empty() { stdout.to_string() } else { stderr.to_string() })
}

/// Makes the call `agent` would make for a worker (`user_settings` as the run has them) in `dir`: `Ok` when it
/// answered, else what it said. A rate limit is an answer: the worker waits for those out itself.
pub fn probe(agent: &dyn AgentBackend, model: &str, user_settings: bool, dir: &Path) -> Result<(), String> {
    let model = super::overrides::model(model);
    let command = agent.session_command(&SessionSpec {
        prompt: PROMPT,
        model: &model,
        effort: "",
        resume_id: None,
        fork: false,
        user_settings,
        system_prompt: None,
        extra_args: &[],
        cwd: dir,
        isolation: crate::isolation::Isolation::None,
    });
    let out = crate::os::proc::run_with_timeout(command, TIMEOUT_SECS)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.lines().any(|l| agent.parse_result(l).is_some_and(|r| r.rate_limited)) {
        return Ok(());
    }
    let said = said(&stdout, &String::from_utf8_lossy(&out.stderr));
    let failed = !out.success || stdout.lines().any(|l| serde_json::from_str::<serde_json::Value>(l.trim()).is_ok_and(|v| v["is_error"] == true));
    if failed { Err(said) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An authentication error names the settings keys that would fix it; any other error, and an authentication
    /// error with no such keys, stays the agent's text alone.
    #[test]
    fn an_authentication_error_names_the_settings_that_fix_it() {
        let keys = ["ANTHROPIC_BASE_URL".to_string(), "ANTHROPIC_AUTH_TOKEN".to_string()];
        let text = explain("Invalid API key · Please run /login", &keys);
        assert!(text.starts_with("Invalid API key"), "{text}");
        assert!(text.contains("the operator's settings env has ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN; set rlt: { user_env: true } (or a list) in the process file"), "{text}");
        assert!(explain("API Error: 401 Unauthorized", &keys).contains("user_env"));
        assert!(explain("Authentication failed", &keys).contains("user_env"));
        assert!(!explain("connection refused", &keys).contains("user_env"));
        assert!(!explain("Invalid API key", &[]).contains("user_env"));
    }

    /// Only the first lines of the agent's text, and a word when it had none.
    #[test]
    fn the_cause_is_the_agent_s_first_lines() {
        assert_eq!(explain("a\n\nb\nc\nd\ne", &[]), "a\n  b\n  c");
        assert_eq!(explain("  \n", &[]), "the agent failed without saying why");
    }

    /// What the agent said: the error of its result event, else stderr, else stdout.
    #[test]
    fn it_reads_the_error_of_the_result_event() {
        let out = "{\"type\":\"system\"}\n{\"type\":\"result\",\"is_error\":true,\"result\":\"Invalid API key\"}\n";
        assert_eq!(said(out, "noise"), "Invalid API key");
        assert_eq!(said("plain", "stderr text"), "stderr text");
        assert_eq!(said("plain", " "), "plain");
    }
}
