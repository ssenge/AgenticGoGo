//! The checks agg runs around every step when the process file asks (A-38): the inputs and outputs
//! a step declares (`run.declared`), and the scripts it wrote (`run.scripts_runnable`). Each is a judge whose verdict
//! goes to the run record like any hook's; a worker runs it as its own check too.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use super::format::{Phase, Step};
use super::hooks::When;
use super::proc::Shared;
use super::template;
use crate::core::model::{Judge, Verdict};

/// A record entry with one of these kinds, met, is a step with a result.
const RESULTS: [&str; 3] = ["done", "tool", "unchanged"];

/// The built-in checks of step `key` (its id `id`) before or after it, as the process file asks for them.
pub fn judges(shared: &Shared, key: &str, id: &str, when: When) -> Vec<Judge> {
    let mut out = vec![];
    if shared.spec.run.declared {
        match when {
            When::Before => {
                let written = written(shared);
                let inputs: Vec<String> = declared(shared, key, id, false).into_iter().filter(|p| written.contains(p)).collect();
                if !inputs.is_empty() {
                    out.push(Judge::native("declared-inputs", move |ctx| {
                        missing(ctx.dir(), &inputs, "inputs an earlier step wrote are missing", "")
                    }));
                }
            }
            When::After => {
                let outputs = declared(shared, key, id, true);
                if !outputs.is_empty() {
                    out.push(Judge::native("declared-outputs", move |ctx| {
                        missing(ctx.dir(), &outputs, "declared outputs missing", "; write them")
                    }));
                }
            }
        }
    }
    if let (When::After, Some(notes)) = (when, &shared.spec.run.notes) {
        let (notes, id) = (notes.clone(), id.to_string());
        out.push(Judge::native("notes", move |ctx| {
            let text = std::fs::read_to_string(ctx.dir().join(&notes)).unwrap_or_default();
            match super::standing::notes_problem(&text, &id) {
                Some(problem) => Verdict::binary(false).with_rationale(problem),
                None => Verdict::binary(true).with_rationale("the step's notes section is short enough"),
            }
        }));
    }
    if when == When::After && !shared.spec.run.scripts_runnable.is_empty() {
        let folders = shared.spec.run.scripts_runnable.iter().filter_map(|f| render(shared, f)).collect::<Vec<_>>();
        let key = key.to_string();
        out.push(Judge::native("scripts-runnable", move |ctx| runnable(ctx.dir(), &key, &folders)));
    }
    out
}

/// A path or folder of the process file with the process's names filled in; none for one that names what only a
/// step knows (`{{ item }}`).
fn render(shared: &Shared, text: &str) -> Option<String> {
    template::render(text, &shared.vars).ok().filter(|t| !t.contains("{{"))
}

/// What step `key` declares under `inputs` or `outputs`: plain paths, without globs and trailing slashes.
fn declared(shared: &Shared, key: &str, id: &str, outputs: bool) -> Vec<String> {
    let settings = shared.settings.for_step(key, id);
    let list = if outputs { settings.outputs } else { settings.inputs };
    list.unwrap_or_default()
        .iter()
        .filter_map(|p| render(shared, p))
        .filter(|p| !p.contains(['*', '?', '[']))
        .map(|p| p.trim_end_matches('/').to_string())
        .collect()
}

/// Every step id of the process, an `each`'s children included.
fn ids(phases: &[Phase]) -> BTreeSet<String> {
    fn walk(steps: &[Step], out: &mut BTreeSet<String>) {
        for s in steps {
            out.insert(s.id.clone());
            walk(&s.steps, out);
        }
    }
    let mut out = BTreeSet::new();
    for p in phases {
        walk(&p.steps, &mut out);
    }
    out
}

/// The id of the step a key names: the key itself, or its longest prefix that is an id (`transform` of `transform-U1`).
fn base_id(key: &str, known: &BTreeSet<String>) -> String {
    let parts: Vec<&str> = key.split('-').collect();
    (1..=parts.len()).rev().map(|n| parts[..n].join("-")).find(|p| known.contains(p)).unwrap_or_else(|| key.to_string())
}

/// The outputs that the steps with a met result in the run's record declare.
fn written(shared: &Shared) -> BTreeSet<String> {
    let Some(history) = &shared.spec.run.history else { return BTreeSet::new() };
    let text = std::fs::read_to_string(shared.project.join(history)).unwrap_or_default();
    let known = ids(&shared.spec.phases);
    let ran: BTreeSet<String> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["met"] == true && RESULTS.contains(&e["kind"].as_str().unwrap_or_default()))
        .filter_map(|e| e["id"].as_str().map(String::from))
        .collect();
    ran.iter().flat_map(|key| declared(shared, key, &base_id(key, &known), true)).collect()
}

/// Met when every path exists in the project; else the ones that don't.
fn missing(project: &Path, paths: &[String], what: &str, advice: &str) -> Verdict {
    let gone: Vec<&str> = paths.iter().map(String::as_str).filter(|p| !project.join(p).exists()).collect();
    if gone.is_empty() {
        let all = if what.starts_with("inputs") { "every input an earlier step wrote exists" } else { "every declared output exists" };
        Verdict::binary(true).with_rationale(all)
    } else {
        Verdict::binary(false).with_rationale(format!("{what}: {}{advice}", gone.join(", ")))
    }
}

/// The files under `folders` that step `key` changed or added (all of them when it has no mark yet).
fn changed(project: &Path, key: &str, folders: &[String]) -> Vec<String> {
    let git = |args: &[&str]| -> Vec<String> {
        match Command::new("git").args(args).current_dir(project).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect(),
            _ => vec![],
        }
    };
    let since = crate::workflow::before(key);
    let marked = !git(&["rev-parse", "-q", "--verify", &since]).is_empty();
    let mut found: BTreeSet<String> = BTreeSet::new();
    for folder in folders {
        let names = if marked {
            [git(&["diff", "--name-only", &since, "--", folder]), git(&["ls-files", "-o", "--exclude-standard", folder])].concat()
        } else {
            walk(&project.join(folder), project)
        };
        found.extend(names);
    }
    found.into_iter().filter(|n| project.join(n).is_file()).collect()
}

/// Every file below `dir`, relative to `project`.
fn walk(dir: &Path, project: &Path) -> Vec<String> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path, project));
            } else if let Ok(rel) = path.strip_prefix(project) {
                out.push(rel.display().to_string());
            }
        }
    }
    out
}

/// Why the script at `path` can't run, or none when it can (or is no script): a `#!` file must be executable, a shell
/// script must parse.
fn problem(project: &Path, path: &str) -> Option<String> {
    let file = project.join(path);
    let bytes = std::fs::read(&file).ok()?;
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).lines().next().unwrap_or_default().to_string();
    let shebang = head.starts_with("#!");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = std::fs::metadata(&file).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
        if shebang && !executable {
            return Some(format!("{path}: starts with #! but isn't executable (chmod +x)"));
        }
    }
    let shell = if head.contains("bash") { "bash" } else { "sh" };
    let sh_line = shebang && (head.trim_end().ends_with("/sh") || head.trim_end().ends_with(" sh") || head.trim_end().ends_with("bash"));
    if path.ends_with(".sh") || sh_line {
        let out = Command::new(shell).arg("-n").arg(&file).output().ok()?;
        if !out.status.success() {
            let said: String = String::from_utf8_lossy(&out.stderr).split_whitespace().collect::<Vec<_>>().join(" ");
            return Some(format!("{path}: {shell} -n: {}", said.chars().take(200).collect::<String>()));
        }
    }
    None
}

/// Met when every script step `key` wrote or changed under `folders` can run.
fn runnable(project: &Path, key: &str, folders: &[String]) -> Verdict {
    let found: Vec<String> = changed(project, key, folders).iter().filter_map(|p| problem(project, p)).collect();
    if found.is_empty() {
        Verdict::binary(true).with_rationale(format!("every script the step wrote under {} can run", folders.join(", ")))
    } else {
        Verdict::binary(false).with_rationale(format!("scripts that can't run: {}", found.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(files: &[(&str, &str, u32)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (name, text, mode) in files {
            let p = tmp.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(*mode)).unwrap();
            }
        }
        tmp
    }

    /// An id is a key's longest prefix that names a step.
    #[test]
    fn a_keys_step_id_is_its_longest_prefix_that_names_a_step() {
        let known: BTreeSet<String> = ["transform", "transform-all", "merge"].iter().map(|s| s.to_string()).collect();
        assert_eq!(base_id("transform-U1", &known), "transform");
        assert_eq!(base_id("transform-all-U1", &known), "transform-all");
        assert_eq!(base_id("merge", &known), "merge");
        assert_eq!(base_id("unknown-U1", &known), "unknown-U1");
    }

    /// Paths missing are named, with the advice for outputs; nothing missing is met.
    #[test]
    fn missing_paths_are_named() {
        let tmp = dir(&[("a/x.md", "x", 0o644)]);
        let ok = missing(tmp.path(), &["a/x.md".into(), "a".into()], "declared outputs missing", "; write them");
        assert!(ok.met() && ok.rationale() == "every declared output exists", "{ok:?}");
        let bad = missing(tmp.path(), &["a/x.md".into(), "b.md".into(), "c".into()], "declared outputs missing", "; write them");
        assert!(!bad.met() && bad.rationale() == "declared outputs missing: b.md, c; write them", "{bad:?}");
        let inputs = missing(tmp.path(), &["gone".into()], "inputs an earlier step wrote are missing", "");
        assert_eq!(inputs.rationale(), "inputs an earlier step wrote are missing: gone");
    }

    /// A `#!` file must be executable and a shell script must parse; other files are left alone.
    #[cfg(unix)]
    #[test]
    fn a_script_that_cannot_run_is_named() {
        let tmp = dir(&[
            ("app/ok.sh", "#!/bin/sh\necho ok\n", 0o755),
            ("app/plain.txt", "no script\n", 0o644),
            ("app/loose.py", "#!/usr/bin/env python3\nprint(1)\n", 0o644),
            ("app/broken.sh", "#!/bin/sh\nif then\n", 0o755),
            ("app/lib.sh", "if then\n", 0o644),
        ]);
        let v = runnable(tmp.path(), "step", &["app".to_string()]);
        assert!(!v.met());
        let why = v.rationale();
        assert!(why.contains("app/loose.py: starts with #! but isn't executable (chmod +x)"), "{why}");
        assert!(why.contains("app/broken.sh: sh -n:") && why.contains("app/lib.sh: sh -n:"), "{why}");
        assert!(!why.contains("ok.sh") && !why.contains("plain.txt"), "{why}");
        let fine = dir(&[("app/ok.sh", "#!/bin/sh\necho ok\n", 0o755)]);
        let ok = runnable(fine.path(), "step", &["app".to_string()]);
        assert!(ok.met() && ok.rationale().contains("can run"), "{ok:?}");
    }
}
