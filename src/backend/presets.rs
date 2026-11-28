//! Presets for four more agent CLIs, on top of the command harness: Gemini CLI, OpenCode, Qwen Code and Pi.
//!
//! They are **experimental**: the flags were read off each CLI's own `--help`, but a full session was not recorded
//! (that costs quota and needs a login), so every field path below that a real session would show is taken from the
//! CLI's documentation and marked `synthetic` in `tests/fixtures/agent-streams/`. Each run says so. A config's own
//! `agents:` entry of the same name replaces a preset, so a CLI that changed its output is a one-line fix there.
//!
//! The brief goes in a file (`{brief_file}`) and the prompt says to read it: no argument-length limit, and the same
//! shape for all four.

use super::command::CommandSpec;

/// The names the presets answer to.
pub const NAMES: &[&str] = &["gemini", "opencode", "qwen", "pi"];

const GEMINI: &str = r#"
command: [gemini, --approval-mode, yolo, --output-format, stream-json, --prompt,
  "Your task is in the file {brief_file}. Read it first and carry it out completely."]
output: json_lines
fields:
  text: ["message=/content"]
  tokens: ["result=/stats/output_tokens", "result=/stats/output"]
rate_limit_pattern: "(?i)rate.?limit|quota exceeded|RESOURCE_EXHAUSTED|\\b429\\b"
capabilities: { usage: true }
experimental: true
"#;

const OPENCODE: &str = r#"
command: [opencode, run, --format, json, --dir, "{workdir}", --auto,
  "Your task is in the file {brief_file}. Read it first and carry it out completely."]
output: json_lines
fields:
  text: ["text=/part/text"]
  tokens: ["step_finish=/part/tokens/output"]
rate_limit_pattern: "(?i)rate.?limit|quota exceeded|\\b429\\b"
capabilities: { usage: true }
experimental: true
"#;

const QWEN: &str = r#"
command: [qwen, --approval-mode, yolo, --output-format, stream-json, --prompt,
  "Your task is in the file {brief_file}. Read it first and carry it out completely."]
output: json_lines
fields:
  text: ["assistant=/message/content/0/text"]
  tokens: ["result=/usage/output_tokens"]
rate_limit_pattern: "(?i)rate.?limit|quota exceeded|\\b429\\b"
capabilities: { usage: true }
experimental: true
"#;

const PI: &str = r#"
command: [pi, --print, --mode, json,
  "Your task is in the file {brief_file}. Read it first and carry it out completely."]
output: json_lines
fields:
  text: ["message_end=/message/content/0/text"]
  tokens: ["message_end=/message/usage/output"]
rate_limit_pattern: "(?i)rate.?limit|quota exceeded|\\b429\\b"
capabilities: { usage: true }
experimental: true
"#;

/// The preset spec for `name`, or `None` when no preset has that name.
pub fn spec(name: &str) -> Option<CommandSpec> {
    let yaml = match name {
        "gemini" => GEMINI,
        "opencode" => OPENCODE,
        "qwen" => QWEN,
        "pi" => PI,
        _ => return None,
    };
    Some(serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("preset `{name}` does not parse: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;

    fn lines(file: &str) -> Vec<String> {
        let path = format!("{}/tests/fixtures/agent-streams/{file}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
    }

    fn backend(name: &str) -> crate::backend::command::CommandBackend {
        crate::backend::command::CommandBackend::new(name, spec(name).unwrap()).unwrap()
    }

    /// Output tokens summed over a recorded stream, and the texts shown.
    fn read(name: &str, file: &str) -> (u64, Vec<String>) {
        let b = backend(name);
        let tokens = lines(file).iter().filter_map(|l| b.parse_usage(l)).filter_map(|t| t.output).sum();
        let shown = lines(file).iter().filter_map(|l| b.parse_event(l)).map(|e| e.text).collect();
        (tokens, shown)
    }

    /// Every preset parses, is experimental, and counts as one agent the registry resolves.
    #[test]
    fn every_preset_is_an_experimental_agent_the_registry_resolves() {
        for name in NAMES {
            let b = crate::backend::for_name(name).unwrap();
            assert!(b.capabilities().experimental && b.name() == *name, "{name}");
            assert!(!b.capabilities().supports_one_shot, "{name} is no ruler");
        }
        assert!(spec("nope").is_none());
    }

    /// OpenCode: the stream was recorded from the real CLI (no login needed for its free model).
    #[test]
    fn opencode_is_read_from_a_recorded_session() {
        let (tokens, shown) = read("opencode", "opencode-1.19.6.jsonl");
        assert_eq!(shown, ["Hi! What can I help you with?"]);
        assert_eq!(tokens, 11);
    }

    /// Gemini, Qwen and Pi: synthetic streams written from each CLI's documentation, until a session is recorded.
    #[test]
    fn the_others_are_read_from_synthetic_streams() {
        let (tokens, shown) = read("gemini", "gemini-0.71.1-synthetic.jsonl");
        assert_eq!((tokens, shown.last().map(String::as_str)), (2, Some("OK")));
        let (tokens, shown) = read("qwen", "qwen-0.28.7-synthetic.jsonl");
        assert_eq!((tokens, shown), (2, vec!["OK".to_string()]));
        let (tokens, shown) = read("pi", "pi-0.73.1-synthetic.jsonl");
        assert_eq!((tokens, shown.last().map(String::as_str)), (2, Some("OK")));
    }

    /// The real no-login recordings show nothing and count nothing: an error line is no output.
    #[test]
    fn a_session_without_login_reads_as_nothing() {
        assert_eq!(read("qwen", "qwen-0.28.7-noauth.jsonl"), (0, vec![]));
        assert_eq!(read("pi", "pi-0.73.1-noauth.jsonl"), (0, vec![]));
    }

    /// The command tells the agent to read the brief file, and operator `worker_args` follow it.
    #[test]
    fn the_command_points_to_the_brief_file_and_takes_the_operators_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let extra = vec!["--model".to_string(), "x".to_string()];
        let spec = crate::backend::SessionSpec {
            prompt: "do it", model: "", effort: "", resume_id: None, fork: false, user_settings: true,
            system_prompt: None, extra_args: &extra, cwd: dir.path(), isolation: crate::isolation::Isolation::None,
        };
        let c = backend("gemini").session_command(&spec);
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.iter().any(|a| a.contains("harness-brief.md")), "{args:?}");
        assert_eq!(&args[args.len() - 2..], ["--model", "x"]);
    }
}
