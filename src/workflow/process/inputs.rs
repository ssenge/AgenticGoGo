//! A step's small input files, put into its brief (A-37): the worker starts on its task instead of
//! spending its first turns reading files the process already knows it needs.

use std::path::Path;

use super::format::InputsSpec;
use crate::workflow::git::git_out;

/// The declared inputs as groups of files that go into the brief whole or not at all: a file as it is named,
/// a pattern (`*-decisions.md`) or a folder as git finds it, so the worker never sees half a folder.
fn groups(project: &Path, always: &[String], inputs: &[String]) -> Vec<Vec<String>> {
    let mut seen: Vec<String> = vec![];
    let mut found: Vec<Vec<String>> = vec![];
    for spec in always.iter().chain(inputs) {
        let listed = spec.contains('*') || spec.ends_with('/') || project.join(spec).is_dir();
        let named: Vec<String> = if listed {
            git_out(project, &["ls-files", "--", spec]).unwrap_or_default().lines().map(String::from).collect()
        } else {
            vec![spec.clone()]
        };
        let new: Vec<String> = named.into_iter().filter(|f| !seen.contains(f)).collect();
        seen.extend(new.iter().cloned());
        found.push(new);
    }
    found
}

/// A group's files and their text, when every one is readable text of at most `max` bytes.
fn read(project: &Path, group: &[String], max: usize) -> Option<Vec<(String, String)>> {
    group.iter().map(|f| std::fs::read_to_string(project.join(f)).ok().filter(|t| t.len() <= max)
        .map(|t| (f.clone(), t))).collect()
}

/// The brief's section with the step's input files as they are at its start, as many whole inputs as fit;
/// empty when none does.
pub fn section(project: &Path, spec: &InputsSpec, inputs: &[String]) -> String {
    let (mut shown, mut body, mut size) = (vec![], String::new(), 0);
    for files in groups(project, &spec.always, inputs).iter().filter_map(|g| read(project, g, spec.max_file)) {
        let total: usize = files.iter().map(|(_, t)| t.len()).sum();
        if size + total > spec.max_all {
            continue;
        }
        size += total;
        for (file, text) in files {
            body.push_str(&format!("\n### `{file}`\n\n````\n{}\n````\n", text.trim_end()));
            shown.push(format!("`{file}`"));
        }
    }
    if shown.is_empty() {
        return String::new();
    }
    format!("## Your input files\n\nThese input files are below as they were when this step started, so you \
             don't need to read them: {}. Read any other input yourself.\n{body}", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named files, small folders and the notes go into the brief; missing and too large files don't, nor a
    /// folder holding one.
    #[test]
    fn the_brief_holds_the_small_named_inputs_and_the_notes() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let spec = InputsSpec { always: vec!["notes/env.md".into(), "notes/progress.md".into()], max_file: 16_000, max_all: 48_000 };
        for folder in ["notes", "reports", "docs"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        std::fs::write(dir.join("notes/progress.md"), "## a-step\n- a note\n").unwrap();
        std::fs::write(dir.join("task.md"), "Move it to the latest LTS.\n").unwrap();
        std::fs::write(dir.join("reports/as-is.md"), "The as-is report.\n").unwrap();
        std::fs::write(dir.join("docs/small.md"), "half a folder\n").unwrap();
        std::fs::write(dir.join("docs/big.md"), "x".repeat(spec.max_file + 1)).unwrap();
        std::fs::write(dir.join("big.yaml"), "x".repeat(spec.max_file + 1)).unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"]] {
            std::process::Command::new("git").args(args).current_dir(dir).status().unwrap();
        }
        let inputs = ["task.md", "reports/", "docs/", "big.yaml", "missing.md"].map(String::from);
        let text = section(dir, &spec, &inputs);
        assert!(text.contains("`notes/progress.md`, `task.md`, `reports/as-is.md`. Read any"), "{text}");
        assert!(!text.contains("env.md"), "a missing file is left out");
        assert!(text.contains("- a note") && text.contains("latest LTS") && text.contains("The as-is report."));
        assert!(!text.contains("half a folder") && !text.contains("xxx"), "no half folder, no large file");
        assert_eq!(section(&dir.join("nowhere"), &spec, &[]), "");
    }
}
