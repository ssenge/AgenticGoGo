//! The standing texts of every process brief (A-38 3.8): agg's defaults, embedded in the binary, that apply when the
//! process file sets nothing. A text the file names **replaces** the default; `{ append: <file> }` adds to it.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde::Deserialize;

/// The worker's system prompt: its brief is `agg/private/INSTRUCTIONS.md`.
pub const SYSTEM: &str = include_str!("defaults/system.md");
/// What a critic reviews and what it never raises (`{{ outputs }}` is the step's outputs).
pub const CRITIC_SCOPE: &str = include_str!("defaults/critic-scope.md");
/// The task force's prompt: its goal, how to find the cause, what it never changes, the answer's form.
pub const TASK_FORCE: &str = include_str!("defaults/task-force.md");
/// The notes section of a brief (`{notes}` is the notes file).
pub const NOTES: &str = include_str!("defaults/notes.md");
/// The shared-machine section of a brief.
pub const SHARED_MACHINE: &str = include_str!("defaults/shared-machine.md");
/// The longest section of a step in the notes file, in lines.
pub const NOTES_LINES: usize = 5;

/// A standing text in a process file: the file that replaces the default, or `append`, the file that follows it.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Text {
    File(String),
    Append { append: String },
}

impl Text {
    /// The file the text names (relative to the process's folder).
    pub fn file(&self) -> &str {
        match self {
            Text::File(f) | Text::Append { append: f } => f,
        }
    }

    /// Whether the file follows the default (and doesn't replace it).
    pub fn appends(&self) -> bool {
        matches!(self, Text::Append { .. })
    }
}

/// The effective text: the default when the file sets none, the file's when it names one, the default followed by the
/// file's when it appends.
pub fn text(spec: Option<&Text>, default: &str, home: &Path) -> Result<String> {
    let Some(spec) = spec else { return Ok(default.to_string()) };
    let file = std::fs::read_to_string(home.join(spec.file())).map_err(|e| anyhow!("{}: {e}", spec.file()))?;
    Ok(if spec.appends() { format!("{}\n\n{file}", default.trim_end()) } else { file })
}

/// The notes section of a brief with the notes file filled in.
pub fn notes_section(file: &str) -> String {
    NOTES.replace("{notes}", file)
}

/// What is wrong with step `id`'s section of the notes file (`## <id>`, at most [`NOTES_LINES`] lines); none when it is
/// fine or the step wrote none.
pub fn notes_problem(notes: &str, id: &str) -> Option<String> {
    let mut lines = notes.lines().skip_while(|l| l.trim() != format!("## {id}")).skip(1);
    let section: Vec<&str> = lines.by_ref().take_while(|l| !l.starts_with("## ")).filter(|l| !l.trim().is_empty()).collect();
    (section.len() > NOTES_LINES).then(|| format!("notes section of {id} has {} lines, at most {NOTES_LINES}", section.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A default stands until the file names a text; a named text replaces it; `append` follows it.
    #[test]
    fn a_text_is_the_default_the_files_or_the_default_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("own.md"), "mine\n").unwrap();
        assert_eq!(text(None, "default", dir.path()).unwrap(), "default");
        assert_eq!(text(Some(&Text::File("own.md".into())), "default", dir.path()).unwrap(), "mine\n");
        assert_eq!(text(Some(&Text::Append { append: "own.md".into() }), "default\n", dir.path()).unwrap(), "default\n\nmine\n");
        assert!(text(Some(&Text::File("none.md".into())), "default", dir.path()).unwrap_err().to_string().contains("none.md"));
    }

    /// Both forms parse: a path, and `append: <path>`.
    #[test]
    fn a_text_is_a_path_or_an_append() {
        let one: Text = serde_yaml::from_str("prompts/a.md").unwrap();
        let two: Text = serde_yaml::from_str("{ append: prompts/b.md }").unwrap();
        assert!(!one.appends() && two.appends() && one.file() == "prompts/a.md" && two.file() == "prompts/b.md");
    }

    /// A step's notes section may have five lines; more is a problem naming the step; another step's section doesn't count.
    #[test]
    fn a_notes_section_has_at_most_five_lines() {
        let long = "## build\n1\n2\n\n3\n4\n5\n6\n## test\nx\n";
        assert_eq!(notes_problem(long, "build").as_deref(), Some("notes section of build has 6 lines, at most 5"));
        assert_eq!(notes_problem(long, "test"), None);
        assert_eq!(notes_problem(long, "deploy"), None, "a step that wrote none is fine");
        assert!(notes_section("app/progress.md").contains("`app/progress.md` holds") && NOTES.contains("{{ this.id }}"));
    }
}
