//! The old names do not come back: the product was AgenticGoGo (`agg`, `AGG_*`) and its checks were "judges".
//! This scans what ships (sources, plugin, examples, scripts, the top-level texts); the docs folder keeps its own
//! history notes and is checked by hand, and the CHANGELOG and the recorded streams say what they said.

use regex::Regex;
use std::path::{Path, PathBuf};

/// Every file under `dir` that is text we own (skips build output and dependencies).
fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(name.as_str(), "node_modules" | "build" | ".svelte-kit" | "target" | ".git") {
                files(&p, out);
            }
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("rs" | "js" | "svelte" | "sh" | "md" | "yaml" | "yml" | "json" | "toml" | "py" | "html" | "css")) {
            out.push(p);
        }
    }
}

/// No file that ships names the product or its checks the old way.
#[test]
fn the_old_names_are_gone_from_what_ships() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `\bagg\b` misses an old name glued behind an escape sequence ("\nagg/private/"), so that is its own alternative
    let old = Regex::new(r"(?i)\bagg\b|AGG_|agenticgogo|judg(e|ing|ed)|\\[ntr]agg").unwrap();
    let mut all = Vec::new();
    for d in ["src", "tests", "plugin", "examples", "scripts", ".github", "docker"] {
        files(&root.join(d), &mut all);
    }
    for f in ["README.md", "AGENTS.md", "Cargo.toml"] {
        all.push(root.join(f));
    }
    let mut hits = Vec::new();
    // this file names what it forbids, and the recorded streams say what they said
    all.retain(|f| !f.ends_with("tests/names.rs") && !f.to_string_lossy().contains("tests/fixtures/"));
    for f in all {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for (i, line) in text.lines().enumerate() {
            // `judgement` is the English word, not the old name
            let mut line_ = line.replace("judgement", "").replace("judgment", "");
            // the installer (and its test) name the old binary on purpose: it tells the user to remove it
            if f.ends_with("scripts/install.sh") || f.ends_with("tests/install.rs") {
                line_ = line_.replace("agg", "");
            }
            if old.is_match(&line_) {
                hits.push(format!("{}:{}: {}", f.strip_prefix(root).unwrap().display(), i + 1, line.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "old names are back:\n{}", hits.join("\n"));
}
