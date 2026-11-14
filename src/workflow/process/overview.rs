//! `agg process <file> --overview [--columns <key>,…] [--only-with <key>]`: the process as one Markdown table per
//! phase, written from its steps' `meta` fields: a row per numbered step, its label first, then a cell per column.
//! The columns are meta keys (default: every key that occurs, in order of first appearance); `--only-with` keeps
//! the steps that have that key. A phase without a row is left out.

use anyhow::{anyhow, bail, Result};
use serde_yaml::Value;

use super::format::Process;
use super::number::{self, Numbered};

/// The text of a meta value as a table cell: a scalar as written, a list joined by commas; `|` and line breaks
/// escaped so the cell stays one cell.
fn cell(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Sequence(items)) => items.iter().map(|v| cell(Some(v))).collect::<Vec<_>>().join(", "),
        Some(other) => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    };
    text.replace('|', "\\|").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A column's heading: its key with the first letter in capitals and `_` as a space.
fn heading(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// Every meta key of the steps, in order of first appearance.
fn keys(steps: &[Numbered]) -> Vec<String> {
    let mut found: Vec<String> = vec![];
    for step in steps {
        if let Some(Value::Mapping(map)) = &step.meta {
            for key in map.keys().filter_map(Value::as_str) {
                if !found.iter().any(|k| k == key) {
                    found.push(key.to_string());
                }
            }
        }
    }
    found
}

/// The tables as text: `columns` (or every key), the steps that have `only_with` when it is set.
pub fn render(steps: &[Numbered], columns: &[String], only_with: Option<&str>) -> String {
    let columns = if columns.is_empty() { keys(steps) } else { columns.to_vec() };
    let mut out = String::new();
    let mut phase = 0;
    for step in steps {
        let Some(label) = step.label() else { continue };
        let Some(Value::Mapping(meta)) = &step.meta else { continue };
        if only_with.is_some_and(|k| !meta.contains_key(k)) {
            continue;
        }
        if step.phase_number != phase {
            phase = step.phase_number;
            let head: Vec<String> = std::iter::once("Step".to_string()).chain(columns.iter().map(|c| heading(c))).collect();
            out += &format!("{}## Phase {}: {}\n\n| {} |\n|{}\n", if out.is_empty() { "" } else { "\n" }, phase,
                step.phase_name, head.join(" | "), "---|".repeat(head.len()));
        }
        let cells: Vec<String> = columns.iter().map(|c| cell(meta.get(c.as_str()))).collect();
        out += &format!("| {} | {} |\n", label.replace('|', "\\|"), cells.join(" | "));
    }
    out
}

/// The flag's value after `name`, split at commas.
fn list_after(args: &[String], name: &str) -> Result<Vec<String>> {
    let Some(at) = args.iter().position(|a| a == name) else { return Ok(vec![]) };
    let value = args.get(at + 1).filter(|v| !v.starts_with("--")).ok_or_else(|| anyhow!("{name} needs a value"))?;
    Ok(value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
}

/// `agg process <file> --overview …`: prints the tables.
pub fn command(file: &std::path::Path, args: &[String]) -> Result<()> {
    let text = std::fs::read_to_string(file).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let process = Process::parse(&text).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let only = list_after(args, "--only-with")?;
    if only.len() > 1 {
        bail!("--only-with takes one key");
    }
    print!("{}", render(&number::number(&process), &list_after(args, "--columns")?, only.first().map(String::as_str)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps() -> Vec<Numbered> {
        let process = Process::parse(
            "version: 1\nphases:\n  - { id: a, name: Alpha, steps: [\n    { id: s1, name: One, kind: outside, meta: { desc: \"x | y\", done: ok, extra: [1, 2] } },\n    { id: s2, name: Two, kind: outside },\n    { id: s3, name: Three, kind: outside, meta: { done: later } }] }\n  - { id: b, name: Beta, steps: [{ id: s4, name: Four, kind: outside }] }\n",
        )
        .expect("parses");
        number::number(&process)
    }

    #[test]
    fn a_row_per_step_with_meta_and_a_table_per_phase_that_has_one() {
        let text = render(&steps(), &[], None);
        assert!(text.starts_with("## Phase 1: Alpha\n\n| Step | Desc | Done | Extra |\n|---|---|---|---|\n"), "{text}");
        assert!(text.contains("| 1.1 One | x \\| y | ok | 1, 2 |"), "a cell escapes | and lists join: {text}");
        assert!(text.contains("| 1.3 Three |  | later |  |"), "{text}");
        assert!(!text.contains("Two") && !text.contains("Beta"), "a step or phase without meta is left out: {text}");
    }

    #[test]
    fn columns_and_only_with_choose_what_is_shown() {
        let text = render(&steps(), &["done".to_string()], Some("desc"));
        assert_eq!(text, "## Phase 1: Alpha\n\n| Step | Done |\n|---|---|\n| 1.1 One | ok |\n");
    }

    #[test]
    fn a_flag_needs_its_value() {
        assert!(list_after(&["--columns".to_string()], "--columns").is_err());
        assert_eq!(list_after(&["--columns".to_string(), "a, b".to_string()], "--columns").unwrap(), ["a", "b"]);
    }
}
