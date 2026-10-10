//! The content-hash snapshot behind the gate check: every filesystem entry a `write_deny` pattern
//! covers — tracked, untracked AND gitignored — fingerprinted before the worker runs and compared
//! after it, plus the restore that puts a violated path back.
//!
//! # Why not `git diff`
//! git sees neither ignored nor (in a diff) untracked files, and an oracle directory is exactly the
//! kind of place that holds generated, ignored fixtures. A walk of the real filesystem is the only
//! source that cannot be dodged by choosing which files git is told about.
//!
//! # What an entry's fingerprint holds
//! A file: its permission bits and the SHA-256 of its bytes. A directory: its permission bits. A
//! symlink: its target string. So a content edit, a `chmod`, a delete, a new file, a file swapped
//! for a symlink, and a symlink retargeted are ALL changes — and a write through a hard link or a
//! symlink changes the real file's hash, whichever name it was written through.

use super::{sha256, PatternSet};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One fingerprinted filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    File { mode: u32, sha: String },
    Dir { mode: u32 },
    Link { target: PathBuf },
    /// a fifo, socket or device — recorded so it cannot appear unnoticed; never restorable.
    Other { mode: u32 },
}

/// Every protected entry, keyed by its project-relative path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub entries: BTreeMap<String, Entry>,
}

/// Walk `root` and fingerprint every entry `set` covers. With `store`, each protected file's bytes
/// are also kept in that content-addressed directory (once per distinct hash) so a violation can be
/// undone even for a file git never knew about.
///
/// Skipped, always: `.git/` (agg owns git; nothing a worker should touch, and not a place a pattern
/// means) and `agg/private/` (agg itself writes there while the worker runs — `state.json`, the
/// log — so covering it would flag agg's own writes).
pub fn take(root: &Path, set: &PatternSet, store: Option<&Path>) -> Result<Snapshot> {
    let canon_root = std::fs::canonicalize(root).with_context(|| format!("resolving {}", root.display()))?;
    let mut w = Walker { root: canon_root, set, store, snap: Snapshot::default(), seen: BTreeSet::new() };
    w.seen.insert(w.root.clone());
    let root_dir = w.root.clone();
    w.dir(&root_dir, "", false)?;
    Ok(w.snap)
}

struct Walker<'a> {
    root: PathBuf,
    set: &'a PatternSet,
    store: Option<&'a Path>,
    snap: Snapshot,
    /// canonical directories already walked — a symlink loop must not walk forever.
    seen: BTreeSet<PathBuf>,
}

impl Walker<'_> {
    /// Walk one directory whose project-relative spelling is `rel`. `via_link` = some ancestor on
    /// this spelling was a symlink, so an entry's REAL path may differ from `rel`.
    fn dir(&mut self, abs: &Path, rel: &str, via_link: bool) -> Result<()> {
        let rd = std::fs::read_dir(abs).with_context(|| format!("reading {}", abs.display()))?;
        let mut names: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if child_rel == ".git" || child_rel == private_rel() {
                continue;
            }
            self.entry(&abs.join(&name), &child_rel, via_link)?;
        }
        Ok(())
    }

    /// Fingerprint one entry if it is protected, and descend into it if a match can live below.
    fn entry(&mut self, abs: &Path, rel: &str, via_link: bool) -> Result<()> {
        let md = std::fs::symlink_metadata(abs).with_context(|| format!("stat {}", abs.display()))?;
        let ft = md.file_type();
        if ft.is_symlink() {
            if self.set.matches(rel) {
                let target = std::fs::read_link(abs)?;
                self.snap.entries.insert(rel.to_string(), Entry::Link { target });
            }
            // A symlinked directory is followed where a match can live below it, so a protected
            // tree that is itself a symlink (to a shared fixture checkout, say) is still hashed.
            if self.set.could_match_below(rel) {
                if let Ok(real) = std::fs::canonicalize(abs) {
                    if real.is_dir() && self.seen.insert(real.clone()) {
                        self.dir(&real, rel, true)?;
                    }
                }
            }
            return Ok(());
        }
        // The REAL path decides too: a file reached through a symlinked directory is protected if
        // its resolved location is, and it is keyed by that location so both spellings agree.
        let key = match via_link.then(|| self.real_rel(abs)).flatten() {
            Some(real) => real,
            None => rel.to_string(),
        };
        if self.set.matches(rel) || self.set.matches(&key) {
            let e = self.fingerprint(abs, &md)?;
            self.snap.entries.insert(key, e);
        }
        if ft.is_dir() && self.set.could_match_below(rel) {
            self.dir(abs, rel, via_link)?;
        }
        Ok(())
    }

    /// `abs` resolved and made relative to the (canonical) root; `None` when it lies outside.
    fn real_rel(&self, abs: &Path) -> Option<String> {
        let real = std::fs::canonicalize(abs).ok()?;
        let rel = real.strip_prefix(&self.root).ok()?;
        Some(rel.to_string_lossy().into_owned())
    }

    fn fingerprint(&self, abs: &Path, md: &std::fs::Metadata) -> Result<Entry> {
        let mode = mode_of(md);
        Ok(if md.is_dir() {
            Entry::Dir { mode }
        } else if md.is_file() {
            let sha = sha256::file_hex(abs).with_context(|| format!("hashing protected {}", abs.display()))?;
            if let Some(store) = self.store {
                keep_blob(store, &sha, abs)?;
            }
            Entry::File { mode, sha }
        } else {
            Entry::Other { mode }
        })
    }
}

/// `agg/private` as a project-relative string, from [`crate::paths`] rather than restated.
fn private_rel() -> String {
    crate::paths::private_dir(Path::new("")).to_string_lossy().into_owned()
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o7777
}
#[cfg(not(unix))]
fn mode_of(md: &std::fs::Metadata) -> u32 {
    u32::from(md.permissions().readonly())
}

/// Copy `src` into the content-addressed `store` as `<sha>`, unless that blob is already there.
/// Written to a temp name and renamed, so a half-written blob never sits under a real hash.
fn keep_blob(store: &Path, sha: &str, src: &Path) -> Result<()> {
    let dest = store.join(sha);
    if dest.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(store)?;
    let tmp = store.join(format!(".{sha}.{}", std::process::id()));
    std::fs::copy(src, &tmp).with_context(|| format!("keeping a copy of {}", src.display()))?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

/// The keys whose entry differs between two snapshots — changed, appeared or disappeared — sorted.
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let keys: BTreeSet<&String> = before.entries.keys().chain(after.entries.keys()).collect();
    keys.into_iter()
        .filter(|k| before.entries.get(*k) != after.entries.get(*k))
        .cloned()
        .collect()
}

/// Put every key in `changed` back to how `before` had it: remove what appeared, recreate what
/// disappeared, rewrite what changed, from the blobs in `store`.
///
/// A file is rewritten by write-to-temp-then-rename, never in place: if the worker hard-linked the
/// protected file to a name of its own, an in-place write would push the restored bytes through
/// the worker's link too, and a later write through that link would change the oracle again.
///
/// Errors are HARD. A violation that cannot be undone leaves a weakened oracle on disk for the next
/// session's judges, which is the outcome this whole feature exists to prevent.
pub fn restore(root: &Path, before: &Snapshot, changed: &[String], store: &Path) -> Result<()> {
    // deepest first: clear what is in the way (an appeared tree, a file now standing where a
    // directory was), so the recreation pass below finds the ground empty.
    for key in changed.iter().rev() {
        let p = root.join(key);
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        let keep_dir = md.is_dir() && matches!(before.entries.get(key), Some(Entry::Dir { .. }));
        if keep_dir {
            continue;
        }
        if md.is_dir() {
            make_writable_tree(&p);
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        }
        .with_context(|| format!("removing {}", p.display()))?;
    }
    // shallowest first: directories before what goes in them.
    for key in changed {
        let p = root.join(key);
        match before.entries.get(key) {
            None => {}
            Some(Entry::Dir { .. }) => std::fs::create_dir_all(&p)?,
            Some(Entry::File { mode, sha }) => put_file(&p, store, sha, *mode)?,
            Some(Entry::Link { target }) => symlink(target, &p)?,
            Some(Entry::Other { .. }) => bail!("cannot restore the special file {}", p.display()),
        }
    }
    // directory modes LAST and deepest first, so a protected read-only directory does not block
    // writing its own restored children.
    for key in changed.iter().rev() {
        if let Some(Entry::Dir { mode }) = before.entries.get(key) {
            set_mode(&root.join(key), *mode)?;
        }
    }
    Ok(())
}

fn put_file(p: &Path, store: &Path, sha: &str, mode: u32) -> Result<()> {
    let blob = store.join(sha);
    let got = sha256::file_hex(&blob).with_context(|| format!("reading the kept copy of {}", p.display()))?;
    // The store sits under `agg/private/`, which a worker under `isolation: none` can write. The
    // hash in agg's MEMORY is the authority; a blob that no longer matches it is not restored.
    if got != sha {
        bail!("the kept copy of {} was tampered with (hash {got}, expected {sha})", p.display());
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = p.with_file_name(format!(".{name}.agg-restore.{}", std::process::id()));
    std::fs::copy(&blob, &tmp)?;
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, p).with_context(|| format!("restoring {}", p.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("restoring the mode of {}", p.display()))
}
#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, p).with_context(|| format!("restoring the link {}", p.display()))
}
#[cfg(not(unix))]
fn symlink(_target: &Path, p: &Path) -> Result<()> {
    bail!("cannot restore the symlink {} on this platform", p.display())
}

/// A worker may have `chmod -w`'d a directory it created so it cannot be removed; undo that first.
fn make_writable_tree(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    make_writable_tree(&e.path());
                }
            }
        }
    }
}
