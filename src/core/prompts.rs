//! Prompts from files: `prompt_file` / `role_prompt_file`, the file-backed twins of `prompt` /
//! `role_prompt`.
//!
//! A prompt kept in its own file is reviewed and diffed as a document instead of as a YAML string,
//! and can be shared with other tooling. Three rules make that safe to rely on:
//!
//! - **One form per key.** Naming the inline key and its file twin in one block is a startup error
//!   (`problems`), as is a file that cannot be read — never a silent preference for one of them.
//! - **Read at session start.** [`load`] runs as each session's brief is composed, so an edited
//!   file applies from the next session; a run never has to restart to pick it up.
//! - **Recorded.** Every file read is hashed into `agg/private/prompts.jsonl` ([`record`]), one row
//!   per session, so a run shows which version of a prompt each session was given.

use crate::core::config::{AggConfig, ResolvedStep};
use crate::problem::Problem;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One prompt file as a session read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptFile {
    /// the config key that named it: `prompt_file` or `role_prompt_file`.
    pub key: String,
    /// the resolved path that was read.
    pub path: String,
    /// lowercase hex SHA-256 of its bytes.
    pub sha256: String,
}

/// One `prompts.jsonl` row: the prompt files one session's brief was composed from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRecord {
    pub session: u32,
    pub step: String,
    pub files: Vec<PromptFile>,
    pub ts: u64,
}

/// Where a configured prompt file lives: absolute as written, else relative to `agg/`.
pub fn resolve(config_base: &Path, file: &str) -> PathBuf {
    let p = Path::new(file);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_base.join(p)
    }
}

/// The startup refusals for ONE inline/file pair at `at` (e.g. `steps.w`): both forms set, or a
/// file that cannot be read. `key` is the inline key's name (`prompt` / `role_prompt`).
pub fn pair_problems(at: &str, key: &str, inline: Option<&str>, file: Option<&str>, config_base: &Path) -> Vec<Problem> {
    let Some(file) = file else { return Vec::new() };
    let path = format!("{at}.{key}_file");
    if inline.is_some() {
        return vec![Problem::new(
            path,
            format!("`{key}` and `{key}_file` are alternatives — set one of them, not both."),
        )];
    }
    match std::fs::read_to_string(resolve(config_base, file)) {
        Ok(_) => Vec::new(),
        Err(e) => vec![Problem::new(path, format!("cannot read `{file}` ({}): {e}", resolve(config_base, file).display()))],
    }
}

/// Every prompt-file refusal in `cfg`: the `defaults:` role pair and each step's two pairs.
pub fn problems(cfg: &AggConfig, config_base: &Path) -> Vec<Problem> {
    let d = &cfg.defaults;
    let mut out =
        pair_problems("defaults", "role_prompt", d.role_prompt.as_deref(), d.role_prompt_file.as_deref(), config_base);
    for (name, b) in &cfg.steps {
        let at = format!("steps.{name}");
        out.extend(pair_problems(&at, "role_prompt", b.role_prompt.as_deref(), b.role_prompt_file.as_deref(), config_base));
        out.extend(pair_problems(&at, "prompt", b.prompt.as_deref(), b.prompt_file.as_deref(), config_base));
    }
    out
}

/// `step` with its file-backed prompts READ NOW into `role_prompt` / `prompt`, plus what was read.
/// A file that cannot be read is an error: a session briefed without its task does the wrong work.
pub fn load(config_base: &Path, step: &ResolvedStep) -> Result<(ResolvedStep, Vec<PromptFile>)> {
    let mut out = step.clone();
    let mut files = Vec::new();
    if let Some(f) = &step.role_prompt_file {
        let (text, pf) = read("role_prompt_file", config_base, f)?;
        out.role_prompt = Some(text);
        files.push(pf);
    }
    if let Some(f) = &step.prompt_file {
        let (text, pf) = read("prompt_file", config_base, f)?;
        out.prompt = Some(text);
        files.push(pf);
    }
    Ok((out, files))
}

/// Read one prompt file and hash the very bytes that were read.
fn read(key: &str, config_base: &Path, file: &str) -> Result<(String, PromptFile)> {
    let path = resolve(config_base, file);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {key} `{file}` ({})", path.display()))?;
    let sha256 = crate::protect::sha256::hex(&bytes);
    let text = String::from_utf8(bytes).with_context(|| format!("{key} `{file}` is not UTF-8"))?;
    // the workflow's variables: a file is a template it wrote; inline text (a project's content) is not
    let text = crate::core::vars::apply(&text).map_err(|e| anyhow::anyhow!("{key} `{file}`: {e}"))?;
    Ok((text, PromptFile { key: key.into(), path: path.display().to_string(), sha256 }))
}

/// Append one row to `agg/private/prompts.jsonl`. Nothing to record (no file-backed prompt) writes
/// nothing.
pub fn record(dir: &Path, session: u32, step: &str, files: &[PromptFile]) -> Result<()> {
    use std::io::Write;
    if files.is_empty() {
        return Ok(());
    }
    let path = crate::paths::prompts_jsonl(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let rec = PromptRecord { session, step: step.into(), files: files.to_vec(), ts: crate::util::now_epoch() };
    let line = serde_json::to_string(&rec).context("serializing a prompt record")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{line}").with_context(|| format!("appending to {}", path.display()))
}

/// Every row of `agg/private/prompts.jsonl`, oldest first; a missing file is no rows.
pub fn read_ledger(dir: &Path) -> Vec<PromptRecord> {
    std::fs::read_to_string(crate::paths::prompts_jsonl(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> AggConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn base() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("steps")).unwrap();
        std::fs::write(t.path().join("steps/w.md"), "do the work\n").unwrap();
        std::fs::write(t.path().join("role.md"), "you are a tester\n").unwrap();
        t
    }

    #[test]
    fn a_relative_file_resolves_under_agg_and_an_absolute_one_as_written() {
        assert_eq!(resolve(Path::new("/p/agg"), "steps/w.md"), Path::new("/p/agg/steps/w.md"));
        assert_eq!(resolve(Path::new("/p/agg"), "/shared/w.md"), Path::new("/shared/w.md"));
    }

    #[test]
    fn both_forms_in_one_block_are_refused_at_the_file_key() {
        let t = base();
        let c = cfg(
            "project: p\ndefaults: { role_prompt: x, role_prompt_file: role.md }\n\
             steps:\n  w: { prompt: x, prompt_file: steps/w.md, role_prompt: y, role_prompt_file: role.md }\n",
        );
        let paths: Vec<String> = problems(&c, t.path()).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["defaults.role_prompt_file", "steps.w.role_prompt_file", "steps.w.prompt_file"]);
    }

    #[test]
    fn a_missing_file_is_refused_and_a_present_one_is_not() {
        let t = base();
        let c = cfg("project: p\nsteps:\n  ok: { prompt_file: steps/w.md }\n  bad: { prompt_file: steps/nope.md }\n");
        let ps = problems(&c, t.path());
        assert_eq!(ps.len(), 1, "{ps:?}");
        assert_eq!(ps[0].path, "steps.bad.prompt_file");
        assert!(ps[0].message.contains("cannot read `steps/nope.md`"), "{}", ps[0].message);
    }

    /// A step's file overrides the defaults' INLINE role text — the pair inherits as a pair.
    #[test]
    fn a_step_role_file_replaces_the_defaults_inline_role() {
        let c = cfg("project: p\ndefaults: { role_prompt: inline }\nsteps:\n  a: { role_prompt_file: role.md }\n  b: {}\n");
        let a = c.resolve_step("a").unwrap();
        assert_eq!((a.role_prompt.as_deref(), a.role_prompt_file.as_deref()), (None, Some("role.md")));
        let b = c.resolve_step("b").unwrap();
        assert_eq!((b.role_prompt.as_deref(), b.role_prompt_file.as_deref()), (Some("inline"), None));
    }

    /// The loaded step is the inline step: same `role_prompt` and `prompt` text, so the brief built
    /// from it is the same — and each file read is listed with its hash.
    #[test]
    fn a_loaded_step_equals_the_inline_form() {
        let t = base();
        let inline = cfg("project: p\nsteps:\n  w: { role_prompt: \"you are a tester\\n\", prompt: \"do the work\\n\" }\n");
        let filed = cfg("project: p\nsteps:\n  w: { role_prompt_file: role.md, prompt_file: steps/w.md }\n");
        let (loaded, files) = load(t.path(), &filed.resolve_step("w").unwrap()).unwrap();
        let want = inline.resolve_step("w").unwrap();
        assert_eq!((loaded.role_prompt, loaded.prompt), (want.role_prompt, want.prompt));
        assert_eq!(files.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["role_prompt_file", "prompt_file"]);
        assert_eq!(files[1].sha256, crate::protect::sha256::hex(b"do the work\n"));
    }

    #[test]
    fn an_inline_only_step_loads_unchanged_and_reads_nothing() {
        let t = base();
        let step = cfg("project: p\nsteps:\n  w: { prompt: x }\n").resolve_step("w").unwrap();
        let (loaded, files) = load(t.path(), &step).unwrap();
        assert_eq!(loaded.prompt.as_deref(), Some("x"));
        assert!(files.is_empty());
    }

    /// Read at session start: an edit between two loads is picked up, and the ledger shows a new hash.
    #[test]
    fn an_edited_file_applies_next_load_and_records_a_new_hash() {
        let t = base();
        let step = cfg("project: p\nsteps:\n  w: { prompt_file: steps/w.md }\n").resolve_step("w").unwrap();
        let (_, first) = load(t.path(), &step).unwrap();
        record(t.path(), 1, "w", &first).unwrap();
        std::fs::write(t.path().join("steps/w.md"), "do it differently\n").unwrap();
        let (second_step, second) = load(t.path(), &step).unwrap();
        record(t.path(), 2, "w", &second).unwrap();
        assert_eq!(second_step.prompt.as_deref(), Some("do it differently\n"));
        let rows = read_ledger(t.path());
        assert_eq!(rows.iter().map(|r| r.session).collect::<Vec<_>>(), [1, 2]);
        assert_ne!(rows[0].files[0].sha256, rows[1].files[0].sha256);
    }

    #[test]
    fn a_prompt_file_that_is_not_utf8_is_an_error() {
        let t = base();
        std::fs::write(t.path().join("bin.md"), [0xff, 0xfe, 0x00]).unwrap();
        let step = cfg("project: p\nsteps:\n  w: { prompt_file: bin.md }\n").resolve_step("w").unwrap();
        let err = load(t.path(), &step).unwrap_err();
        assert!(format!("{err:#}").contains("not UTF-8"), "{err:#}");
    }

    #[test]
    fn a_file_gone_at_session_start_is_an_error_and_nothing_records_nothing() {
        let t = base();
        let step = cfg("project: p\nsteps:\n  w: { prompt_file: steps/gone.md }\n").resolve_step("w").unwrap();
        assert!(load(t.path(), &step).is_err());
        record(t.path(), 1, "w", &[]).unwrap();
        assert!(!crate::paths::prompts_jsonl(t.path()).exists());
    }
}
