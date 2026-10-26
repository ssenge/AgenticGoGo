//! What the operator's environment changes in every agent call, whichever step or judge makes it: a model the
//! provider lacks and its stand-in (`AGG_MODELS`), Codex's config overrides (`AGG_CODEX_CONFIG`), and the MCP
//! servers and settings of a Claude worker (`AGG_CLAUDE_MCP_CONFIG`, `AGG_CLAUDE_SETTINGS`). Switching
//! between environments (a subscription, a cloud provider, another account) then changes no config file.

/// The model to call for `model`: its stand-in in `AGG_MODELS` (`<model>=<stand-in>`, comma-separated), else
/// `model` itself. E.g. `claude-fable-5-1=claude-opus-5-5` where the provider has no Fable.
pub fn model(model: &str) -> String {
    stand_in(std::env::var("AGG_MODELS").ok().as_deref(), model)
}

/// Codex's `-c` overrides from `AGG_CODEX_CONFIG`, one `key=value` per line, e.g.
/// `model_provider="amazon-bedrock-runtime"`: passed on every Codex session and one-shot, since a one-shot
/// reads no config file (`--ignore-user-config`) and so can't take its provider from one.
pub fn codex_config() -> Vec<String> {
    lines(std::env::var("AGG_CODEX_CONFIG").ok().as_deref())
}

/// Claude Code's `--mcp-config` (a file, or its JSON) from `AGG_CLAUDE_MCP_CONFIG`, for a worker that runs without
/// the user's settings and so without the MCP servers registered there: e.g. a web search where the provider has
/// none.
pub fn claude_mcp_config() -> Option<String> {
    set("AGG_CLAUDE_MCP_CONFIG")
}

/// Claude Code's `--settings` (a file, or its JSON) from `AGG_CLAUDE_SETTINGS`, for a worker that runs without the
/// user's settings: e.g. a hook that shortens its shell commands' output.
pub fn claude_settings() -> Option<String> {
    set("AGG_CLAUDE_SETTINGS")
}

/// Variable `name`'s value, unless it is unset or blank.
fn set(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// `model`'s stand-in in `map`, else `model`.
fn stand_in(map: Option<&str>, model: &str) -> String {
    map.into_iter()
        .flat_map(|m| m.split(','))
        .filter_map(|pair| pair.split_once('='))
        .find(|(from, _)| from.trim() == model)
        .map_or_else(|| model.to_string(), |(_, to)| to.trim().to_string())
}

/// The non-empty lines of `raw`, trimmed.
fn lines(raw: Option<&str>) -> Vec<String> {
    raw.into_iter().flat_map(str::lines).map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model with a stand-in is replaced; any other, and every model without a map, stays.
    #[test]
    fn a_model_with_a_stand_in_is_replaced() {
        let map = Some("claude-fable-5-1 = claude-opus-5-5, gpt-a=gpt-b");
        assert_eq!(stand_in(map, "claude-fable-5-1"), "claude-opus-5-5");
        assert_eq!(stand_in(map, "gpt-a"), "gpt-b");
        assert_eq!(stand_in(map, "claude-haiku-5-5"), "claude-haiku-5-5");
        assert_eq!(stand_in(map, ""), "");
        assert_eq!(stand_in(None, "claude-fable-5-1"), "claude-fable-5-1");
        assert_eq!(stand_in(Some("no pair here"), "x"), "x");
    }

    /// Each non-empty line is one override; none set, none passed.
    #[test]
    fn each_line_is_one_codex_override() {
        assert_eq!(lines(Some("model_provider=\"p\"\n\n  model=\"m\"  \n")), ["model_provider=\"p\"", "model=\"m\""]);
        assert!(lines(None).is_empty());
        assert!(lines(Some("")).is_empty());
    }
}
