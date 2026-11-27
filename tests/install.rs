//! `scripts/install.sh` against fake release assets (`RLT_DOWNLOAD_URL`, a `file://` URL) and a PATH of fake older copies: it
//! upgrades the copy first on PATH in place, lists the others with their versions, warns about an older one with the command
//! that fixes it, and says which copy `rlt` resolves to — without ever changing another copy.
//!
//! Unix-only (the script is POSIX sh and the fake copies are shell scripts).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fake(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("rlt");
    std::fs::write(&file, format!("#!/bin/sh\necho 'rlt {version}'\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file
}

fn version_of(file: &Path) -> String {
    String::from_utf8_lossy(&Command::new(file).output().unwrap().stdout).trim().to_string()
}

/// `sh scripts/install.sh` serving `release` as the download, with `path_dirs` first on PATH.
fn install(root: &Path, release: &Path, path_dirs: &[&Path], install_dir: Option<&Path>) -> (Output, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    let path: Vec<String> = path_dirs.iter().map(|d| d.display().to_string()).chain(["/usr/bin".into(), "/bin".into()]).collect();
    let mut command = Command::new("sh");
    command.arg(script).env_clear().env("PATH", path.join(":")).env("HOME", root.join("home"))
        .env("RLT_DOWNLOAD_URL", format!("file://{}", release.display()));
    if let Some(d) = install_dir {
        command.env("RLT_INSTALL_DIR", d);
    }
    let out = command.output().unwrap();
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out, text)
}

/// With no `RLT_INSTALL_DIR`, the copy first on PATH is replaced; an older copy behind it is listed and warned about with
/// the `cp` and `rm` that fix it, a newer one is only listed, and neither is changed.
#[test]
fn the_copy_first_on_path_is_upgraded_in_place_and_older_ones_behind_it_are_warned_about() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let release = fake(&r.join("release"), "0.0.51");
    let (first, older, newer) = (fake(&r.join("first"), "0.0.40"), fake(&r.join("older"), "0.0.30"), fake(&r.join("newer"), "0.0.99"));
    let (out, text) = install(r, &release, &[&r.join("first"), &r.join("older"), &r.join("newer")], None);
    assert!(out.status.success(), "{text}");
    assert_eq!(version_of(&first), "rlt 0.0.51", "upgraded in place: {text}");
    assert_eq!(version_of(&older), "rlt 0.0.30");
    assert_eq!(version_of(&newer), "rlt 0.0.99");
    assert!(text.contains(&format!("{}  rlt 0.0.30", older.display())) && text.contains(&format!("{}  rlt 0.0.99", newer.display())), "{text}");
    let warning = text.lines().find(|l| l.contains("WARNING")).unwrap_or_else(|| panic!("no warning:\n{text}"));
    assert!(warning.contains(&older.display().to_string()), "{warning}");
    assert!(warning.contains(&format!("cp \"{}\" \"{}\"", first.display(), older.display())), "{warning}");
    assert!(warning.contains(&format!("rm \"{}\"", older.display())), "{warning}");
    assert_eq!(text.matches("WARNING").count(), 1, "only the older copy is warned about:\n{text}");
    assert!(text.contains(&format!("rlt resolves to: {}", first.display())), "{text}");
}

/// An install into a directory the operator names leaves the copy first on PATH alone, warns that it is older, and says
/// that `rlt` still resolves to it.
#[test]
fn an_install_elsewhere_warns_that_the_older_copy_still_wins() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let release = fake(&r.join("release"), "0.0.51");
    let old = fake(&r.join("old"), "0.0.7");
    let target = r.join("target");
    let (out, text) = install(r, &release, &[&r.join("old")], Some(&target));
    assert!(out.status.success(), "{text}");
    assert_eq!(version_of(&target.join("rlt")), "rlt 0.0.51");
    assert_eq!(version_of(&old), "rlt 0.0.7", "never changed by the script");
    assert!(text.contains("WARNING") && text.contains(&format!("cp \"{}\" \"{}\"", target.join("rlt").display(), old.display())), "{text}");
    assert!(text.contains(&format!("rlt resolves to: {}", old.display())), "{text}");
}

/// An `agg` left on PATH from before the rename is named, with the advice to remove it once no project needs it.
#[test]
fn an_old_agg_on_path_is_named() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let release = fake(&r.join("release"), "0.1.0");
    let target = r.join("target");
    let old_dir = r.join("old");
    std::fs::create_dir_all(&old_dir).unwrap();
    let old = old_dir.join("agg");
    std::fs::write(&old, "#!/bin/sh\necho 'agg 0.0.51'\n").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (out, text) = install(r, &release, &[&target, &old_dir], Some(&target));
    assert!(out.status.success(), "{text}");
    assert!(text.contains(&format!("the old agg is still at {}: remove it once no project needs it", old.display())), "{text}");
}
