//! Unit tests for `write_deny`: the snapshot catches every way of changing a protected path, the
//! restore undoes each one, and the per-tier refusal matrix says what it claims.

use super::snapshot::{self, Entry, Snapshot};
use super::*;
use std::fs;

/// A project with a tracked-looking oracle, a nested oracle, an unprotected source file and a
/// sibling directory that must never be walked.
fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    fs::create_dir_all(r.join("tests/oracles/deep")).unwrap();
    fs::create_dir_all(r.join("src")).unwrap();
    fs::create_dir_all(r.join("agg/private")).unwrap();
    fs::write(r.join("tests/oracles/x.json"), "orig").unwrap();
    fs::write(r.join("tests/oracles/deep/y.json"), "deep").unwrap();
    fs::write(r.join("tests/oracles/cache.gen"), "generated").unwrap();
    fs::write(r.join("src/lib.txt"), "base").unwrap();
    t
}

fn set(root: &Path, pats: &[&str]) -> PatternSet {
    PatternSet::compile(root, &pats.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}

fn store(root: &Path) -> PathBuf {
    crate::paths::protected_store(root)
}

/// Snapshot, run `attack`, and return what changed — then restore and prove the tree is back.
fn caught(attack: impl FnOnce(&Path)) -> Vec<String> {
    let t = project();
    let r = t.path();
    let s = set(r, &["tests/oracles/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    attack(r);
    let after = snapshot::take(r, &s, None).unwrap();
    let changed = snapshot::diff(&before, &after);
    snapshot::restore(r, &before, &changed, &store(r)).unwrap();
    let back = snapshot::take(r, &s, None).unwrap();
    assert_eq!(snapshot::diff(&before, &back), Vec::<String>::new(), "the restore left a difference");
    assert_eq!(fs::read_to_string(r.join("tests/oracles/x.json")).unwrap(), "orig");
    changed
}

#[test]
fn the_snapshot_covers_exactly_the_protected_tree_including_the_directory_itself() {
    let t = project();
    let s = set(t.path(), &["tests/oracles/**"]);
    let snap = snapshot::take(t.path(), &s, None).unwrap();
    let keys: Vec<&str> = snap.entries.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["tests/oracles", "tests/oracles/cache.gen", "tests/oracles/deep", "tests/oracles/deep/y.json", "tests/oracles/x.json"]
    );
    assert!(matches!(snap.entries["tests/oracles"], Entry::Dir { .. }));
}

/// agg writes `agg/private/` while the worker runs, and `.git/` is agg's; neither may ever be read
/// as the worker's doing, even under a pattern that covers everything.
#[test]
fn agg_private_and_git_are_never_part_of_the_snapshot() {
    let t = project();
    fs::create_dir_all(t.path().join(".git")).unwrap();
    fs::write(t.path().join(".git/HEAD"), "ref").unwrap();
    fs::write(t.path().join("agg/private/state.json"), "{}").unwrap();
    let snap = snapshot::take(t.path(), &set(t.path(), &["**"]), None).unwrap();
    assert!(snap.entries.keys().all(|k| !k.starts_with(".git") && !k.starts_with("agg/private")), "{:?}", snap.entries.keys());
    assert!(snap.entries.contains_key("src/lib.txt"));
}

// ---- the adversarial suite, at the filesystem level (the `none` tier's whole defence) ----

#[test]
fn a_direct_write_is_caught() {
    assert_eq!(caught(|r| fs::write(r.join("tests/oracles/x.json"), "evil").unwrap()), ["tests/oracles/x.json"]);
}

#[test]
fn delete_and_recreate_is_caught_and_an_identical_recreate_is_not_a_change() {
    assert_eq!(
        caught(|r| {
            fs::remove_file(r.join("tests/oracles/x.json")).unwrap();
            fs::write(r.join("tests/oracles/x.json"), "evil").unwrap();
        }),
        ["tests/oracles/x.json"]
    );
    // same bytes, same mode, new inode: nothing a judge could observe changed.
    assert!(caught(|r| {
        fs::remove_file(r.join("tests/oracles/x.json")).unwrap();
        fs::write(r.join("tests/oracles/x.json"), "orig").unwrap();
    })
    .is_empty());
}

#[test]
fn a_plain_delete_is_caught() {
    assert_eq!(caught(|r| fs::remove_file(r.join("tests/oracles/x.json")).unwrap()), ["tests/oracles/x.json"]);
}

#[test]
fn renaming_over_the_file_is_caught() {
    assert_eq!(
        caught(|r| {
            fs::write(r.join("evil.json"), "evil").unwrap();
            fs::rename(r.join("evil.json"), r.join("tests/oracles/x.json")).unwrap();
        }),
        ["tests/oracles/x.json"]
    );
}

#[test]
#[cfg(unix)]
fn a_write_through_a_symlink_from_outside_is_caught() {
    assert_eq!(
        caught(|r| {
            std::os::unix::fs::symlink(r.join("tests/oracles/x.json"), r.join("link")).unwrap();
            fs::write(r.join("link"), "evil").unwrap();
        }),
        ["tests/oracles/x.json"]
    );
}

#[test]
#[cfg(unix)]
fn swapping_the_file_for_a_symlink_is_caught_and_undone() {
    assert_eq!(
        caught(|r| {
            fs::write(r.join("mine.json"), "evil").unwrap();
            fs::remove_file(r.join("tests/oracles/x.json")).unwrap();
            std::os::unix::fs::symlink(r.join("mine.json"), r.join("tests/oracles/x.json")).unwrap();
        }),
        ["tests/oracles/x.json"]
    );
}

/// The restore must BREAK the worker's hard link (rename, not write-in-place), or the worker's own
/// name for the oracle survives and a later write through it changes the oracle again.
#[test]
fn a_write_through_a_hard_link_is_caught_and_the_link_is_severed() {
    let t = project();
    let r = t.path();
    let s = set(r, &["tests/oracles/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    fs::hard_link(r.join("tests/oracles/x.json"), r.join("hl")).unwrap();
    fs::write(r.join("hl"), "evil").unwrap();
    let changed = snapshot::diff(&before, &snapshot::take(r, &s, None).unwrap());
    assert_eq!(changed, ["tests/oracles/x.json"]);
    snapshot::restore(r, &before, &changed, &store(r)).unwrap();
    fs::write(r.join("hl"), "evil again").unwrap();
    assert_eq!(fs::read_to_string(r.join("tests/oracles/x.json")).unwrap(), "orig", "the link still reaches the oracle");
}

#[test]
#[cfg(unix)]
fn chmod_is_caught_and_the_mode_restored() {
    use std::os::unix::fs::PermissionsExt;
    let t = project();
    let r = t.path();
    let p = r.join("tests/oracles/x.json");
    let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o7777;
    let s = set(r, &["tests/oracles/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o777)).unwrap();
    let changed = snapshot::diff(&before, &snapshot::take(r, &s, None).unwrap());
    assert_eq!(changed, ["tests/oracles/x.json"]);
    snapshot::restore(r, &before, &changed, &store(r)).unwrap();
    assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o7777, mode);
}

#[test]
fn a_new_file_under_a_protected_directory_is_caught_and_removed() {
    let t = project();
    let r = t.path();
    let s = set(r, &["tests/oracles/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    fs::create_dir_all(r.join("tests/oracles/new/dir")).unwrap();
    fs::write(r.join("tests/oracles/new/dir/z.json"), "z").unwrap();
    fs::write(r.join("tests/oracles/extra.json"), "x").unwrap();
    let changed = snapshot::diff(&before, &snapshot::take(r, &s, None).unwrap());
    assert_eq!(changed, ["tests/oracles/extra.json", "tests/oracles/new", "tests/oracles/new/dir", "tests/oracles/new/dir/z.json"]);
    snapshot::restore(r, &before, &changed, &store(r)).unwrap();
    assert!(!r.join("tests/oracles/new").exists() && !r.join("tests/oracles/extra.json").exists());
}

/// git never sees an ignored file, which is the whole reason the check walks the filesystem.
#[test]
fn a_gitignored_file_inside_a_protected_directory_is_caught() {
    assert_eq!(caught(|r| fs::write(r.join("tests/oracles/cache.gen"), "tampered").unwrap()), ["tests/oracles/cache.gen"]);
}

/// Moving an ANCESTOR away takes the whole protected tree with it.
#[test]
fn renaming_an_ancestor_away_is_caught_and_the_tree_put_back() {
    let changed = caught(|r| fs::rename(r.join("tests"), r.join("tests_moved")).unwrap());
    assert!(changed.contains(&"tests/oracles/x.json".to_string()), "{changed:?}");
}

#[test]
fn a_case_variant_write_on_a_case_insensitive_volume_is_caught() {
    let t = project();
    if !case_insensitive(t.path()) {
        eprintln!("SKIP: this volume is case-sensitive, a case variant is a different file");
        return;
    }
    assert_eq!(caught(|r| fs::write(r.join("TESTS/Oracles/X.JSON"), "evil").unwrap()), ["tests/oracles/x.json"]);
    // …and a NEW file spelled in another case still lands under the protected directory.
    let changed = caught(|r| fs::write(r.join("Tests/ORACLES/new.json"), "n").unwrap());
    assert_eq!(changed, ["tests/oracles/new.json"]);
}

/// A protected tree that is itself a symlink (to a shared checkout, say) is followed, so a write to
/// the real files behind it is caught; a symlink loop does not hang the walk.
#[test]
#[cfg(unix)]
fn a_protected_tree_behind_a_symlink_is_followed_and_loops_terminate() {
    let shared = tempfile::tempdir().unwrap();
    fs::write(shared.path().join("g.json"), "golden").unwrap();
    std::os::unix::fs::symlink(shared.path(), shared.path().join("loop")).unwrap();
    let t = project();
    let r = t.path();
    std::os::unix::fs::symlink(shared.path(), r.join("tests/golden")).unwrap();
    let s = set(r, &["tests/golden/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    assert!(before.entries.contains_key("tests/golden/g.json"), "{:?}", before.entries.keys());
    fs::write(shared.path().join("g.json"), "weakened").unwrap();
    let changed = snapshot::diff(&before, &snapshot::take(r, &s, None).unwrap());
    assert_eq!(changed, ["tests/golden/g.json"]);
    snapshot::restore(r, &before, &changed, &store(r)).unwrap();
    assert_eq!(fs::read_to_string(shared.path().join("g.json")).unwrap(), "golden");
}

/// Unprotected work is invisible to the check — the session's real output is not a violation.
#[test]
fn changes_outside_the_patterns_are_not_violations() {
    assert!(caught(|r| {
        fs::write(r.join("src/lib.txt"), "work").unwrap();
        fs::create_dir_all(r.join("tests/unit")).unwrap();
        fs::write(r.join("tests/unit/t.py"), "t").unwrap();
    })
    .is_empty());
}

/// Under `isolation: none` the worker can write `agg/private/`, so a planted copy must not be what
/// the restore writes back.
#[test]
fn a_tampered_kept_copy_is_refused_not_restored() {
    let t = project();
    let r = t.path();
    let s = set(r, &["tests/oracles/**"]);
    let before = snapshot::take(r, &s, Some(&store(r))).unwrap();
    let Entry::File { sha, .. } = &before.entries["tests/oracles/x.json"] else { panic!("a file") };
    fs::write(store(r).join(sha), "evil").unwrap();
    fs::write(r.join("tests/oracles/x.json"), "evil").unwrap();
    let changed = snapshot::diff(&before, &snapshot::take(r, &s, None).unwrap());
    let err = snapshot::restore(r, &before, &changed, &store(r)).unwrap_err().to_string();
    assert!(err.contains("tampered"), "{err}");
}

#[test]
fn diff_reports_appeared_vanished_and_changed_keys_sorted() {
    let mut a = Snapshot::default();
    let mut b = Snapshot::default();
    a.entries.insert("x".into(), Entry::Dir { mode: 0o755 });
    a.entries.insert("gone".into(), Entry::Dir { mode: 0o755 });
    b.entries.insert("x".into(), Entry::Dir { mode: 0o700 });
    b.entries.insert("new".into(), Entry::Other { mode: 0 });
    assert_eq!(snapshot::diff(&a, &b), ["gone", "new", "x"]);
    assert!(snapshot::diff(&a, &a).is_empty());
}

// ---- configuration, the platform matrix, and the kernel rules ----

#[test]
fn union_adds_and_dedupes_across_spellings() {
    let u = union(&["tests/oracles/".to_string()], &["./tests/oracles/**".to_string(), "rules.yml".to_string()]);
    assert_eq!(u, ["tests/oracles/**", "rules.yml"]);
    // a pattern that does not parse is KEPT, so the startup check can refuse it by name
    assert_eq!(normalize(["../escape"]), ["../escape"]);
}

#[test]
fn none_warns_that_only_the_gate_applies() {
    let w = check_step("impl", Isolation::None, &["tests/oracles/**".into()]).unwrap().expect("a warning");
    assert!(w.contains("GATE CHECK") && w.contains("impl") && w.contains("tests/oracles/**"), "{w}");
    assert_eq!(check_step("impl", Isolation::None, &[]), Ok(None), "nothing listed, nothing to say");
}

#[test]
fn an_unparseable_pattern_is_refused_on_every_tier() {
    for tier in [Isolation::None, Isolation::Sandbox, Isolation::Container] {
        let e = check_step("s", tier, &["/abs".into()]).unwrap_err();
        assert!(e[0].contains("absolute"), "{e:?}");
    }
}

/// A bind-based tier refuses a pattern it cannot express — and accepts directory and literal ones.
#[test]
fn a_bind_tier_refuses_deep_wildcards_and_accepts_dirs_and_literals() {
    let ok: Vec<String> = vec!["tests/oracles/**".into(), ".gitlab-ci.yml".into()];
    assert!(check_step("s", Isolation::Container, &ok).unwrap().is_none());
    let e = check_step("s", Isolation::Container, &["**/*.golden".into()]).unwrap_err();
    assert!(e[0].contains("cannot be enforced by the kernel") && e[0].contains("container"), "{e:?}");
    // the sandbox tier: Seatbelt takes any glob; bwrap is a bind tier like the container.
    let sb = check_step("s", Isolation::Sandbox, &["**/*.golden".into()]);
    if cfg!(target_os = "macos") {
        assert_eq!(sb, Ok(None));
    } else {
        assert!(sb.is_err());
    }
}

#[test]
fn bind_paths_name_the_directory_or_the_existing_literal() {
    let t = project();
    fs::write(t.path().join("rules.yml"), "r").unwrap();
    let pats: Vec<String> = vec!["tests/oracles/**".into(), "rules.yml".into(), "absent.yml".into(), "**/*.g".into()];
    assert_eq!(bind_paths(t.path(), &pats), ["tests/oracles", "rules.yml"]);
}

#[test]
fn materialize_creates_subtree_roots_only_on_a_bind_tier() {
    let t = project();
    let pats = vec!["fixtures/**".to_string()];
    materialize(t.path(), Isolation::None, &pats);
    assert!(!t.path().join("fixtures").exists());
    materialize(t.path(), Isolation::Container, &pats);
    assert!(t.path().join("fixtures").is_dir());
}

#[test]
fn seatbelt_rules_deny_the_tree_and_the_removal_of_its_ancestors() {
    let t = project();
    let root = fs::canonicalize(t.path()).unwrap();
    let rules = seatbelt_rules(&root, &["tests/oracles/**".into(), "a/b/*.x".into()]).unwrap();
    let joined = rules.join("\n");
    assert!(joined.contains("(deny file-write* (regex #\"^"), "{joined}");
    assert!(joined.contains("(/.*)?$\"))"), "the tree and the directory itself: {joined}");
    // `tests` (ancestor of a dir/** pattern) and both `a` and `a/b` (above a deeper wildcard)
    let unlinks = rules.iter().filter(|r| r.starts_with("(deny file-write-unlink")).count();
    assert_eq!(unlinks, 3, "{joined}");
    assert!(seatbelt_rules(&root, &[]).unwrap().is_empty());
}

#[test]
fn case_insensitivity_is_probed_without_writing() {
    let t = project();
    let before: Vec<_> = fs::read_dir(t.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    let _ = case_insensitive(t.path());
    let after: Vec<_> = fs::read_dir(t.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before.len(), after.len(), "the probe must not create anything");
    assert!(!case_insensitive(Path::new("/definitely/not/here")));
}

#[test]
fn the_ledger_round_trips() {
    let t = project();
    let rec = ViolationRecord {
        session: 2,
        step: "worker".into(),
        reason: VIOLATION.into(),
        window: "session".into(),
        paths: vec!["tests/oracles/x.json".into()],
        restored: true,
        ts: 1,
    };
    append(t.path(), &rec).unwrap();
    append(t.path(), &rec).unwrap();
    assert_eq!(read(t.path()), vec![rec.clone(), rec]);
    assert!(read(Path::new("/nope")).is_empty());
}
