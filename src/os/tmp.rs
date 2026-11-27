//! A short temp folder per worker session, verifier and script step: `/tmp/rlt-<run>/<n>/`.
//!
//! Every session gets its own `TMPDIR`, emptied when it ends, so what a test or a tool leaves there (one project made
//! 682 synthesised stacks of 34 MB each in a day) never piles up on the host. The path is short on purpose: a unix
//! socket path may have at most 104 bytes on macOS, and its `$TMPDIR` alone is about 50.
//!
//! A run's root holds a `pid` file. The first folder a process makes also removes the roots of runs that are gone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// What every run root's name starts with.
const PREFIX: &str = "rlt-";

/// A dead run's root is removed when it is older than this.
const STALE: Duration = Duration::from_secs(3600);

/// The folder every root lives in: `/tmp` on unix (short), the temp dir elsewhere.
fn base() -> PathBuf {
    if cfg!(unix) && Path::new("/tmp").is_dir() {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

/// One project's run root and the next folder number in it.
struct Root {
    dir: PathBuf,
    next: u64,
}

/// The roots of this process, by (base folder, project).
static ROOTS: Mutex<Option<HashMap<(PathBuf, PathBuf), Root>>> = Mutex::new(None);

/// A temp folder made for one session, verifier or script; removed when dropped (unless `RLT_KEEP_TMP` is set).
pub struct SessionTmp {
    path: PathBuf,
}

impl SessionTmp {
    /// The folder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Makes `TMPDIR`, `TMP` and `TEMP` of the command this folder.
    pub fn apply(&self, command: &mut Command) {
        for key in ["TMPDIR", "TMP", "TEMP"] {
            command.env(key, &self.path);
        }
    }
}

impl Drop for SessionTmp {
    fn drop(&mut self) {
        if std::env::var_os("RLT_KEEP_TMP").is_none() {
            crate::workflow::scratch::remove(&self.path);
        }
    }
}

/// The user's cache folder: `$XDG_CACHE_HOME` or `~/.cache` (Linux), `~/Library/Caches` (macOS), `%LOCALAPPDATA%`
/// (Windows). Unlike `$TMPDIR` it is not purged while a run sleeps.
pub fn user_cache_dir() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    }
    if cfg!(target_os = "macos") {
        return home().join("Library/Caches");
    }
    std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".cache"))
}

/// A fresh temp folder for a session of the run in `project`.
pub fn session(project: &Path) -> std::io::Result<SessionTmp> {
    session_in(&base(), project)
}

/// [`session`] under another base folder (the tests').
pub(crate) fn session_in(base: &Path, project: &Path) -> std::io::Result<SessionTmp> {
    let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let key = (base.to_path_buf(), project.clone());
    let mut guard = ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    let roots = guard.get_or_insert_with(HashMap::new);
    if !roots.contains_key(&key) {
        sweep(base);
        let dir = base.join(format!("{PREFIX}{}", run_key(&project)));
        make(&dir)?;
        std::fs::write(dir.join("pid"), std::process::id().to_string())?;
        roots.insert(key.clone(), Root { dir, next: 0 });
    }
    let root = roots.get_mut(&key).expect("just inserted");
    root.next += 1;
    let path = root.dir.join(root.next.to_string());
    make(&path)?;
    Ok(SessionTmp { path })
}

/// 8 hex characters naming the run: the project, this process and the moment.
fn run_key(project: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (project, std::process::id(), SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))
        .hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

/// A folder only its owner can enter (a session's files are no one else's business).
fn make(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Removes the run roots in `base` whose process is gone and that are older than an hour.
pub(crate) fn sweep(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid = std::fs::read_to_string(entry.path().join("pid")).ok().and_then(|t| t.trim().parse::<u32>().ok());
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE);
        if name.starts_with(PREFIX) && old && pid.is_some_and(|p| !crate::os::proc::pid_alive(p)) {
            crate::workflow::scratch::remove(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each folder is its own, under one root named after the run, short, private, and gone when dropped.
    #[test]
    fn each_session_gets_its_own_short_private_folder_that_goes_with_it() {
        let base = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let (a, b) = (session_in(base.path(), project.path()).unwrap(), session_in(base.path(), project.path()).unwrap());
        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().parent(), b.path().parent(), "one root per run and project");
        assert!(a.path().parent().unwrap().join("pid").is_file());
        let rel = a.path().strip_prefix(base.path()).unwrap().to_string_lossy().len();
        assert!(rel < 20, "short: {rel}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777, 0o700);
        }
        std::fs::write(a.path().join("left-over"), "x").unwrap();
        let gone = a.path().to_path_buf();
        drop(a);
        assert!(!gone.exists() && b.path().exists());
    }

    /// The real base is `/tmp` on unix, so a socket path under it stays within 104 bytes.
    #[cfg(unix)]
    #[test]
    fn the_real_folder_is_under_tmp_and_leaves_room_for_a_socket() {
        let project = tempfile::tempdir().unwrap();
        let s = session(project.path()).unwrap();
        assert!(s.path().to_string_lossy().starts_with("/tmp/rlt-"), "{}", s.path().display());
        assert!(s.path().as_os_str().len() < 40, "{}", s.path().display());
        let mut c = Command::new("true");
        s.apply(&mut c);
        for key in ["TMPDIR", "TMP", "TEMP"] {
            assert!(c.get_envs().any(|(k, v)| k == key && v == Some(s.path().as_os_str())));
        }
    }

    /// A dead run's old root is removed, a live run's and a young one's are kept.
    #[test]
    fn the_sweep_removes_a_dead_runs_old_root_and_keeps_live_and_young_ones() {
        let base = tempfile::tempdir().unwrap();
        let root = |name: &str, pid: u32, age: Duration| {
            let dir = base.path().join(name);
            std::fs::create_dir_all(dir.join("1")).unwrap();
            std::fs::write(dir.join("pid"), pid.to_string()).unwrap();
            let when = SystemTime::now() - age;
            std::fs::File::open(&dir).unwrap().set_modified(when).unwrap();
            dir
        };
        let dead_pid = {
            let mut c = Command::new("true").spawn().unwrap();
            c.wait().unwrap();
            c.id()
        };
        let old_dead = root("rlt-dead", dead_pid, Duration::from_secs(7200));
        let young_dead = root("rlt-young", dead_pid, Duration::from_secs(60));
        let old_live = root("rlt-live", std::process::id(), Duration::from_secs(7200));
        let other = root("other-dead", dead_pid, Duration::from_secs(7200));
        sweep(base.path());
        assert!(!old_dead.exists());
        assert!(young_dead.exists() && old_live.exists() && other.exists());
    }
}
