//! The operator's own `env` block (`env` in the user-scope Claude settings file), passed on to the Claude workers and
//! one-shot calls without loading the plugins, hooks and MCP servers of the same file.
//!
//! A worker that runs without the user's settings (`--setting-sources project,local`) never reads that file, and a
//! host whose model gateway or telemetry switches are set only there then fails every session. `user_env` names what
//! to take from the block: nothing, every key, or a list of keys. A variable rlt's own environment already has wins,
//! and the keys rlt manages itself (`RLT_*`) are never taken. Only key NAMES are ever shown, never values.

use std::path::PathBuf;

use serde::Deserialize;

/// What to take from the settings' `env` block.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum UserEnv {
    /// `false`: nothing; `true`: every key of the block
    Flag(bool),
    /// only these keys
    Keys(Vec<String>),
}

impl Default for UserEnv {
    fn default() -> UserEnv {
        UserEnv::Flag(false)
    }
}

impl From<bool> for UserEnv {
    fn from(on: bool) -> UserEnv {
        UserEnv::Flag(on)
    }
}

impl<S: Into<String>, const N: usize> From<[S; N]> for UserEnv {
    fn from(keys: [S; N]) -> UserEnv {
        UserEnv::Keys(keys.into_iter().map(Into::into).collect())
    }
}

thread_local! {
    /// What the current run passes: per thread (a lane's own), and replaced whenever a run takes its setting.
    static PASSED: std::cell::RefCell<Vec<(String, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The user-scope Claude settings file: `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
pub fn settings_file() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join(".claude")))?;
    Some(dir.join("settings.json"))
}

/// The string entries of the `env` object in settings text; none when it is no JSON or has no such block.
pub fn parse(text: &str) -> Vec<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let Some(env) = value.get("env").and_then(|e| e.as_object()) else { return Vec::new() };
    env.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()
}

/// The `env` block of the settings file; none when the file is missing or unreadable.
pub fn file_env() -> Vec<(String, String)> {
    settings_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

/// What `which` takes of `block`, in the block's order: not a key rlt's environment has (`set`), not one rlt manages.
pub fn select(block: Vec<(String, String)>, which: &UserEnv, set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let wanted = |k: &str| match which {
        UserEnv::Flag(on) => *on,
        UserEnv::Keys(keys) => keys.iter().any(|w| w == k),
    };
    block.into_iter().filter(|(k, _)| wanted(k) && !k.starts_with("RLT_") && !set(k)).collect()
}

/// Takes `which` from the settings file, makes it what the Claude backend passes from now on, and returns the key names.
pub fn take(which: &UserEnv) -> Vec<String> {
    let pairs = select(file_env(), which, |k| std::env::var_os(k).is_some());
    let names = pairs.iter().map(|(k, _)| k.clone()).collect();
    PASSED.with(|p| *p.borrow_mut() = pairs);
    names
}

/// Makes `pairs` what this thread's calls pass to Claude: a run's instance installs its own on the thread it runs on.
pub fn install(pairs: Vec<(String, String)>) {
    PASSED.with(|p| *p.borrow_mut() = pairs);
}

/// What the run passes to every Claude worker and one-shot process.
pub fn passed() -> Vec<(String, String)> {
    PASSED.with(|p| p.borrow().clone())
}

/// `name` from rlt's environment, else from what the run passes: what a Claude process would see.
pub fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().or_else(|| passed().into_iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// The names in the settings file's `env` block that look like a gateway or a key (`ANTHROPIC_*`).
pub fn gateway_keys() -> Vec<String> {
    file_env().into_iter().map(|(k, _)| k).filter(|k| k.starts_with("ANTHROPIC_")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"{"model":"x","env":{"ANTHROPIC_BASE_URL":"https://gw","ANTHROPIC_AUTH_TOKEN":"t","N":3,"FLAG":true,"RLT_X":"1"}}"#;

    fn names(p: &[(String, String)]) -> Vec<&str> {
        p.iter().map(|(k, _)| k.as_str()).collect()
    }

    /// Only string values of the `env` block count; no block, no JSON or no object is nothing, never an error.
    #[test]
    fn the_block_has_its_string_values_only() {
        let all = parse(FILE);
        assert_eq!(names(&all), ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "RLT_X"], "{all:?}");
        assert!(parse(r#"{"model":"x"}"#).is_empty());
        assert!(parse(r#"{"env":"no"}"#).is_empty());
        assert!(parse("not json").is_empty());
        assert!(parse("").is_empty());
    }

    /// `false` takes nothing, `true` every key, a list those it names; a key the file lacks is silently absent.
    #[test]
    fn a_selection_takes_what_it_names() {
        let none = |_: &str| false;
        assert!(select(parse(FILE), &UserEnv::Flag(false), none).is_empty());
        assert_eq!(names(&select(parse(FILE), &UserEnv::Flag(true), none)), ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN"]);
        let list = UserEnv::Keys(vec!["ANTHROPIC_BASE_URL".into(), "MISSING".into()]);
        assert_eq!(names(&select(parse(FILE), &list, none)), ["ANTHROPIC_BASE_URL"]);
    }

    /// A variable rlt's own environment has wins over the file; the keys rlt manages are never taken, even when named.
    #[test]
    fn the_environment_and_rlt_s_own_keys_win() {
        let set = |k: &str| k == "ANTHROPIC_BASE_URL";
        assert_eq!(names(&select(parse(FILE), &UserEnv::Flag(true), set)), ["ANTHROPIC_AUTH_TOKEN"]);
        let named = UserEnv::Keys(vec!["RLT_X".into()]);
        assert!(select(parse(FILE), &named, |_| false).is_empty());
    }

    /// The settings file is `$CLAUDE_CONFIG_DIR/settings.json` when that is set, else `~/.claude/settings.json`.
    /// One test, since both read process-wide variables.
    #[test]
    fn the_file_follows_claude_config_dir_and_what_a_run_takes_is_passed() {
        let dir = tempfile::tempdir().unwrap();
        let old_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::fs::write(dir.path().join("settings.json"), r#"{"env":{"RLT_UE_TEST_A":"a","UE_TEST_B":"b","UE_TEST_SET":"c"}}"#).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path());
        std::env::set_var("UE_TEST_SET", "mine");
        assert_eq!(settings_file(), Some(dir.path().join("settings.json")));
        assert_eq!(take(&UserEnv::Flag(true)), ["UE_TEST_B"], "the environment's UE_TEST_SET wins, RLT_* never");
        assert_eq!(passed(), [("UE_TEST_B".to_string(), "b".to_string())]);
        assert_eq!(var("UE_TEST_B").as_deref(), Some("b"));
        assert_eq!(var("UE_TEST_SET").as_deref(), Some("mine"));
        assert!(take(&UserEnv::Flag(false)).is_empty() && passed().is_empty());
        std::fs::remove_file(dir.path().join("settings.json")).unwrap();
        assert!(take(&UserEnv::Flag(true)).is_empty(), "a missing file is nothing, not an error");
        std::env::remove_var("UE_TEST_SET");
        match old_dir {
            Some(d) => std::env::set_var("CLAUDE_CONFIG_DIR", d),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
    }

    /// The spelling in a process file: a flag or a list.
    #[test]
    fn it_reads_a_flag_or_a_list() {
        assert_eq!(serde_yaml::from_str::<UserEnv>("true").unwrap(), UserEnv::Flag(true));
        assert_eq!(serde_yaml::from_str::<UserEnv>("[A, B]").unwrap(), UserEnv::Keys(vec!["A".into(), "B".into()]));
        assert!(serde_yaml::from_str::<UserEnv>("maybe").is_err());
    }
}
