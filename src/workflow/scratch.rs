//! A flow's scratch folder outside the project (`agg-flow-<pid>-<n>` in the temp dir): made per flow, removed
//! when the flow ends, and a crashed run's removed when the next flow starts. A file left there outlives the
//! run that meant it, and a later run's agent that finds it takes its outdated claim for news.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// What every flow's folder name starts with.
const PREFIX: &str = "agg-flow-";

/// Flows this process made: several flows in one process (a test binary) each get their own folder, so one
/// that ends never removes another's.
static MADE: AtomicUsize = AtomicUsize::new(0);

/// A new flow's folder in `temp`, not created yet.
pub(crate) fn folder(temp: &Path) -> PathBuf {
    temp.join(format!("{PREFIX}{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::Relaxed)))
}

/// Removes the flow folders in `temp` whose process is gone: a run that crashed never removed its own.
pub(crate) fn sweep(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        if owner(&entry.file_name().to_string_lossy()).is_some_and(|pid| !crate::os::proc::pid_alive(pid)) {
            remove(&entry.path());
        }
    }
}

/// The pid of the process a flow folder named `name` belongs to; `None` for any other name.
fn owner(name: &str) -> Option<u32> {
    name.strip_prefix(PREFIX)?.split('-').next()?.parse().ok()
}

/// Removes `dir` and everything in it, made writable first: a step may leave files and folders read-only.
pub(crate) fn remove(dir: &Path) {
    if dir.exists() {
        writable(dir);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Gives the owner write access to `path` and, for a folder, to everything in it (`chmod -R u+w`); links are
/// left alone, so nothing outside the folder changes.
fn writable(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() {
        return;
    }
    let mut perms = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // a folder also needs to be listable to be emptied
        let add = if meta.is_dir() { 0o700 } else { 0o200 };
        perms.set_mode(perms.mode() | add);
    }
    // on windows the flag is only the owner's write bit; there is no group or world to open it to
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(path, perms);
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            writable(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder with a read-only file in a read-only subfolder, as a step may leave it.
    fn locked(dir: &Path) {
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/7.1-cannot-fix.md"), "old claim").unwrap();
        for p in [dir.join("sub/7.1-cannot-fix.md"), dir.join("sub")] {
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&p, perms).unwrap();
        }
    }

    /// A pid that is certainly no process's: a child that ran and was reaped.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// A folder with read-only files and folders in it is removed whole.
    #[test]
    fn a_read_only_folder_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let dir = folder(temp.path());
        locked(&dir);
        remove(&dir);
        assert!(!dir.exists());
        remove(&dir);
    }

    /// The sweep removes the folders of processes that are gone, and keeps a live one's and any other.
    #[test]
    fn the_sweep_removes_only_dead_flows_folders() {
        let temp = tempfile::tempdir().unwrap();
        let (dead, legacy) = (temp.path().join(format!("{PREFIX}{}-0", dead_pid())),
                              temp.path().join(format!("{PREFIX}{}", dead_pid())));
        let (live, other) = (folder(temp.path()), temp.path().join(format!("{PREFIX}x")));
        for dir in [&dead, &legacy, &live, &other] {
            locked(dir);
        }
        sweep(temp.path());
        assert_eq!([&dead, &legacy, &live, &other].map(|d| d.exists()), [false, false, true, true]);
        assert_eq!((owner("agg-flow-42-3"), owner("agg-flow-42"), owner("agg-flowx-42")), (Some(42), Some(42), None));
        remove(&live);
        remove(&other);
    }
}
