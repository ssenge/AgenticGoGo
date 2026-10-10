//! Unit tests for `agg wiki lint`: every rule on a minimal wiki, and the wiki the worker's own
//! instructions tell it to write.

use super::*;

/// A wiki with exactly `files` (`(relative path, content)`), returned with its tempdir guard.
fn wiki(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    tmp
}

fn lint_files(files: &[(&str, &str)]) -> LintReport {
    let w = wiki(files);
    lint(w.path(), true).unwrap()
}

fn rules(f: &[Finding]) -> Vec<&'static str> {
    f.iter().map(|f| f.rule).collect()
}

const GOOD: &str = "---\ntype: decision\ntitle: Parser\n---\nbody [x](x.md)\n";

#[test]
fn a_conformant_wiki_passes_and_counts_its_files() {
    let r = lint_files(&[
        ("parser.md", GOOD),
        ("sub/deep.md", "---\ntype: plan\nstatus: draft\ngenerated: { by: agent/m, at: 2026-06-20T22:53:05Z }\n---\n"),
        ("index.md", "---\nokf_version: \"0.2\"\n---\n# Decisions\n\n* [Parser](parser.md) - why\n- [Sub](sub/) - more\n"),
        ("sub/index.md", "# Plans\n* [Deep](deep.md)\n"),
        ("log.md", "# Log\n\n## 2026-05-22\n* **Update**: x\n\n## 2026-05-15\nprose is fine\n"),
        ("notes.txt", "not markdown — ignored"),
    ]);
    assert!(r.ok, "{:#?}", r.problems);
    assert!(r.warnings.is_empty(), "{:#?}", r.warnings);
    assert_eq!((r.files, r.concepts), (5, 2));
}

#[test]
fn a_concept_without_frontmatter_or_type_is_a_violation() {
    let r = lint_files(&[
        ("none.md", "# just a heading\n"),
        ("open.md", "---\ntype: x\nno closing fence\n"),
        ("bad.md", "---\ntype: [unclosed\n---\n"),
        ("list.md", "---\n- a\n- b\n---\n"),
        ("empty.md", "---\n---\nbody\n"),
        ("blank.md", "---\ntype: \"  \"\n---\n"),
        ("nul.md", "---\ntype:\n---\n"),
        ("num.md", "---\ntype: 3\n---\n"),
    ]);
    assert!(!r.ok);
    let got: Vec<(&str, &str)> = r.problems.iter().map(|f| (f.file.as_str(), f.rule)).collect();
    assert_eq!(
        got,
        [
            ("bad.md", "frontmatter.yaml"),
            ("blank.md", "type.empty"),
            ("empty.md", "type.missing"),
            ("list.md", "frontmatter.yaml"),
            ("none.md", "frontmatter.missing"),
            ("nul.md", "type.empty"),
            ("num.md", "type.invalid"),
            ("open.md", "frontmatter.unterminated"),
        ]
    );
    let bad = r.problems.iter().find(|f| f.file == "bad.md").unwrap();
    assert!(bad.line.is_some_and(|l| l >= 2), "a YAML error is located inside the block: {bad:?}");
}

#[test]
fn the_optional_families_only_warn() {
    let r = lint_files(&[(
        "c.md",
        "---\ntype: x\nstatus: final\ngenerated: { at: yesterday }\nverified: [{ by: human:a, at: 2026-06-25T09:00:00Z }, nope, { at: 2026-06-25T09:00:00 }]\nstale_after: 2026-09-23\nsources: [{ id: a }]\ntimestamp: 2026-01-01\n---\n",
    )]);
    assert!(r.ok, "§11: a consumer must not reject a concept over an optional family");
    let w = rules(&r.warnings);
    for rule in ["status.unknown", "generated.by", "timestamp.format", "verified.worker", "verified.shape", "sources.resource", "timestamp.legacy"] {
        assert!(w.contains(&rule), "expected {rule} in {w:?}");
    }
    // `generated.at: yesterday`, `verified[2].at` without an offset, `stale_after` a bare date.
    assert_eq!(w.iter().filter(|r| **r == "timestamp.format").count(), 3, "{w:?}");
}

#[test]
fn family_shapes_that_are_not_mappings_warn() {
    let r = lint_files(&[("c.md", "---\ntype: x\ngenerated: agent\nverified: { by: process:n, at: 2026-06-26T02:00:00Z }\nsources: one\n---\n")]);
    assert!(r.ok);
    assert_eq!(rules(&r.warnings), ["generated.shape", "verified.worker", "sources.shape"]);
}

/// `verified` is legitimate in a bundle someone reviewed; it is only suspect in the worker's wiki.
#[test]
fn verified_is_flagged_only_in_the_worker_wiki() {
    let w = wiki(&[("c.md", "---\ntype: x\nverified: { by: human:a, at: 2026-06-25T09:00:00Z }\n---\n")]);
    assert_eq!(rules(&lint(w.path(), true).unwrap().warnings), ["verified.worker"]);
    assert!(lint(w.path(), false).unwrap().warnings.is_empty());
}

#[test]
fn an_index_follows_section_8() {
    let r = lint_files(&[
        ("index.md", "* [Early](a.md) - before any heading\n# Group\n* not a link\nsome prose\n"),
        ("flat/index.md", "just text\n"),
        ("deep/index.md", "---\nokf_version: \"0.2\"\n---\n# G\n* [a](a.md)\n"),
        ("root2/x.md", GOOD),
    ]);
    let got: Vec<(&str, &str)> = r.problems.iter().map(|f| (f.file.as_str(), f.rule)).collect();
    assert_eq!(
        got,
        [
            ("deep/index.md", "index.frontmatter"),
            ("flat/index.md", "index.sections"),
            ("index.md", "index.entry"),
            ("index.md", "index.entry"),
        ]
    );
    assert_eq!(r.problems[2].line, Some(1));
    assert_eq!(r.problems[3].line, Some(3));
    assert_eq!(rules(&r.warnings), ["index.text", "index.text"]);
}

#[test]
fn a_root_index_may_carry_only_okf_version() {
    let r = lint_files(&[("index.md", "---\nokf_version: \"0.2\"\ntype: index\n---\n# G\n* [a](a.md)\n")]);
    assert_eq!(rules(&r.problems), ["index.frontmatter"]);
    let r = lint_files(&[("index.md", "---\nokf_version: [\n---\n# G\n")]);
    assert_eq!(rules(&r.problems), ["frontmatter.yaml"]);
    let r = lint_files(&[("index.md", "---\nokf_version: \"0.2\"\n# G\n")]);
    assert_eq!(rules(&r.problems), ["frontmatter.unterminated"]);
}

#[test]
fn a_log_is_dated_and_newest_first() {
    let r = lint_files(&[(
        "log.md",
        "# Log\nan entry before any date\n## 2026-05-15\n* a\n## 2026-05-22\n* b\n## 2026-05-22\n## May 1st\n### 2026-13-01\n",
    )]);
    let got: Vec<(&str, Option<usize>)> = r.problems.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(
        got,
        [
            ("log.entry", Some(2)),
            ("log.order", Some(5)),
            ("log.order", Some(7)),
            ("log.date", Some(8)),
            ("log.date", Some(9)),
        ]
    );
}

#[test]
fn an_empty_log_warns_and_frontmatter_is_skipped() {
    let r = lint_files(&[("log.md", "---\nx: 1\n---\n# Log\n")]);
    assert!(r.ok);
    assert_eq!(rules(&r.warnings), ["log.empty"]);
}

#[test]
fn hidden_entries_are_skipped_and_symlinks_not_followed() {
    let w = wiki(&[("a.md", GOOD), (".obsidian/x.md", "no frontmatter"), ("sub/b.md", GOOD)]);
    #[cfg(unix)]
    std::os::unix::fs::symlink(w.path().join("sub"), w.path().join("loop")).unwrap();
    let r = lint(w.path(), true).unwrap();
    assert!(r.ok, "{:#?}", r.problems);
    assert_eq!(r.files, 2);
    #[cfg(unix)]
    assert_eq!(rules(&r.warnings), ["file.symlink"]);
}

#[test]
fn a_non_utf8_file_is_a_violation() {
    let w = wiki(&[]);
    std::fs::write(w.path().join("bin.md"), [0xff, 0xfe, 0x00]).unwrap();
    let r = lint(w.path(), true).unwrap();
    assert_eq!(rules(&r.problems), ["file.encoding"]);
    assert_eq!(r.concepts, 0);
}

#[test]
fn split_handles_a_bom_and_reports_the_body_line() {
    match concept::split("\u{feff}---\ntype: x\n---\nbody\n") {
        concept::Split::Block { yaml, body, body_line } => assert_eq!((yaml, body, body_line), ("type: x\n", "body\n", 4)),
        other => panic!("{other:?}"),
    }
    assert_eq!(concept::split(""), concept::Split::None);
    assert!(concept::is_instant("2026-06-30T14:00:00+02:00"));
    assert!(!concept::is_instant("2026-06-30T14:00:00"), "an instant names its offset");
}

#[test]
fn lint_refuses_a_path_that_is_not_a_directory() {
    let w = wiki(&[("a.md", GOOD)]);
    assert!(lint(&w.path().join("a.md"), false).is_err());
    assert!(lint(&w.path().join("missing"), false).is_err());
}

#[test]
fn run_treats_a_missing_default_wiki_as_conformant_and_a_missing_path_as_an_error() {
    let project = tempfile::tempdir().unwrap();
    assert!(run(project.path(), None, true).unwrap(), "no wiki yet: nothing to violate");
    assert!(run(project.path(), Some(&project.path().join("nope")), false).is_err());
    let own = crate::paths::wiki_dir(project.path());
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("bad.md"), "no frontmatter").unwrap();
    assert!(!run(project.path(), None, false).unwrap());
    assert!(!run(project.path(), Some(&own), true).unwrap());
    assert!(same_dir(&own, &own) && !same_dir(&own, project.path()));
}

#[test]
fn render_names_every_finding_and_the_verdict() {
    let r = lint_files(&[("a.md", "x"), ("b.md", "---\ntype: x\nstatus: odd\n---\n")]);
    let s = render(&r);
    assert!(s.contains("✗ a.md:1 [frontmatter.missing]"), "{s}");
    assert!(s.contains("⚠ b.md [status.unknown]"), "{s}");
    assert!(s.contains("1 violation(s)"), "{s}");
    let ok = render(&lint_files(&[("a.md", GOOD)]));
    assert!(ok.contains("✔ conformant: 1 file(s), 1 concept(s), 0 warning(s)"), "{ok}");
    assert!(render(&absent(Path::new("w"))).contains("no wiki at w yet"));
}

/// THE contract between agg's instructions and its lint: a worker that copies the shapes the brief's
/// footer shows produces a conformant wiki. If the footer's examples drift from OKF, this fails.
#[test]
fn the_wiki_the_worker_footer_prescribes_passes_the_lint() {
    let pages = footer_examples(crate::loop_::EXIT_FOOTER);
    let names: Vec<&str> = pages.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(names, ["parser-approach.md", "index.md", "log.md"], "the footer shows a concept and both reserved files");
    let files: Vec<(&str, &str)> = pages.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let r = lint_files(&files);
    assert!(r.ok && r.warnings.is_empty(), "{:#?} {:#?}", r.problems, r.warnings);
    assert!(!crate::loop_::EXIT_FOOTER.contains("timestamp"), "`timestamp` is OKF v0.1 (superseded by generated.at)");
}

/// The fenced examples of the footer, each under its "`agg/state/wiki/<page>`:" line.
pub(super) fn footer_examples(footer: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = footer.lines();
    while let Some(line) = lines.next() {
        let Some(page) = line.strip_prefix("`agg/state/wiki/").and_then(|r| r.strip_suffix("`:")) else { continue };
        assert_eq!(lines.next(), Some("```"), "an example opens a fence right after its path");
        let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        out.push((page.to_string(), body.join("\n") + "\n"));
    }
    out
}
