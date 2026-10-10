//! `agg wiki lint` through the real binary: the exit contract, the human and JSON reports, and the
//! JSON validated against its PUBLISHED schema (`docs/schemas/wiki_lint.v1.json`) by an independent
//! validator. Also: the lint writes nothing — the wiki is the worker's.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn agg(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("agg").unwrap().current_dir(dir).args(args).output().unwrap()
}

fn validator() -> jsonschema::Validator {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/schemas/wiki_lint.v1.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    jsonschema::validator_for(&schema).expect("wiki_lint.v1.json is a valid JSON Schema")
}

/// Parse `--json` stdout and hold it to the published schema.
fn json_report(out: &Output) -> Value {
    let doc: Value = serde_json::from_slice(&out.stdout).expect("--json prints JSON");
    let errs: Vec<String> = validator().iter_errors(&doc).map(|e| format!("{e} at {}", e.instance_path())).collect();
    assert!(errs.is_empty(), "violates docs/schemas/wiki_lint.v1.json:\n{}\n{doc:#}", errs.join("\n"));
    doc
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// Every file under `dir` with its bytes, sorted — to prove a lint left the tree untouched.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(snapshot(&p));
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn a_project_without_a_wiki_has_nothing_to_violate() {
    let tmp = tempfile::tempdir().unwrap();
    let out = agg(tmp.path(), &["wiki", "lint", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let r = json_report(&out);
    assert_eq!((r["ok"].as_bool(), r["exists"].as_bool(), r["files"].as_u64()), (Some(true), Some(false), Some(0)));
    let human = agg(tmp.path(), &["wiki", "lint"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("nothing to lint"));
}

#[test]
fn violations_exit_1_with_every_finding_located_and_nothing_written() {
    let tmp = tempfile::tempdir().unwrap();
    let wiki = tmp.path().join("agg/state/wiki");
    write(&wiki.join("plan.md"), "---\ntype: plan\nverified: { by: human:me, at: 2026-06-25T09:00:00Z }\n---\nthe plan\n");
    write(&wiki.join("notes.md"), "no frontmatter at all\n");
    write(&wiki.join("log.md"), "## 2026-01-01\n* a\n## 2026-02-01\n* b\n");
    let before = snapshot(&wiki);

    let out = agg(tmp.path(), &["wiki", "lint", "--json"]);
    assert_eq!(out.status.code(), Some(1), "a violation fails the lint");
    let r = json_report(&out);
    assert_eq!(r["ok"], false);
    assert_eq!((r["files"].as_u64(), r["concepts"].as_u64()), (Some(3), Some(2)));
    let problems: Vec<(&str, &str)> = r["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["file"].as_str().unwrap(), f["rule"].as_str().unwrap()))
        .collect();
    assert_eq!(problems, [("log.md", "log.order"), ("notes.md", "frontmatter.missing")]);
    // the worker's wiki claiming a human review is surfaced, never acted on.
    assert_eq!(r["warnings"][0]["rule"], "verified.worker");

    let human = agg(tmp.path(), &["wiki", "lint"]);
    assert_eq!(human.status.code(), Some(1));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("✗ notes.md:1 [frontmatter.missing]") && text.contains("✗ log.md:3 [log.order]"), "{text}");
    assert!(text.contains("2 violation(s)"), "{text}");

    assert_eq!(snapshot(&wiki), before, "agg wiki lint never writes into the wiki");
}

#[test]
fn a_path_argument_lints_any_bundle_and_a_missing_one_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("kb/metric.md"), "---\ntype: Metric\nverified: { by: human:a, at: 2026-06-25T09:00:00Z }\n---\n");
    write(&tmp.path().join("kb/index.md"), "# Metrics\n* [Metric](metric.md) - one\n");
    let out = agg(tmp.path(), &["wiki", "lint", "kb", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    let r = json_report(&out);
    assert_eq!(r["warnings"], serde_json::json!([]), "`verified` is legitimate outside the worker's wiki");

    let missing = agg(tmp.path(), &["wiki", "lint", "nope"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no wiki at nope"));
}

#[test]
fn the_cli_help_documents_the_lint() {
    let tmp = tempfile::tempdir().unwrap();
    let help = String::from_utf8_lossy(&agg(tmp.path(), &["wiki", "lint", "--help"]).stdout).into_owned();
    for needle in ["OKF v0.2", "--json", "[PATH]", "Writes", "nothing"] {
        assert!(help.contains(needle), "`{needle}` missing from:\n{help}");
    }
    let top = String::from_utf8_lossy(&agg(tmp.path(), &["--help"]).stdout).into_owned();
    assert!(top.contains("wiki"), "{top}");
}
