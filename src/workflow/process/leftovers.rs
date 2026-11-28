//! The processes a step leaves running (A-37): every few seconds a sampler notes the run's descendants;
//! when a step ends, the noted ones that are orphans now (their parent gone, adopted by init) end too. The notes are
//! kept in the project's git folder, so a run stopped from outside is cleaned up by the next one.
//!
//! One sampler serves the whole process, however many runs a lane scheduler has in it: each [`Watch`] notes and ends
//! only its own run's processes (a lane's are the workers and scripts it spawned, `own`), so a step ending in one lane
//! never ends the dev server a step in another lane started.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// A process by its pid and start time (a pid alone may be reused).
type Process = (u32, String);

const FILE: &str = "rlt-processes";
const EVERY: Duration = Duration::from_secs(2);

/// Whose descendants a watch notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Roots {
    /// everything below this process: the run that owns the process
    Process,
    /// what the run's own thread spawned (`own`): a lane's
    Spawned,
}

/// What one run's watch knows.
struct State {
    roots: Roots,
    spawned: Mutex<Vec<u32>>,
    /// the processes the current step started, as noted by the sampler
    noted: Mutex<Vec<Process>>,
    file: PathBuf,
}

/// The watches that are alive; the sampler walks them and forgets the dropped.
static ALL: Mutex<Vec<Weak<State>>> = Mutex::new(Vec::new());
static SAMPLER: OnceLock<()> = OnceLock::new();

thread_local! {
    /// The watch of the run on this thread: where `own` registers a pid.
    static CURRENT: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
}

/// One run's watch of what its steps leave running.
pub struct Watch(Arc<State>);

/// Starts the watch of the run in `project`, after ending what an earlier, stopped run left noted there; the watch and
/// how many that was.
pub fn watch(project: &Path, roots: Roots) -> (Watch, usize) {
    let file = crate::git::git_dir(project).join(FILE);
    let earlier: Vec<Process> = std::fs::read_to_string(&file).unwrap_or_default().lines()
        .filter_map(|l| l.split_once('\t').and_then(|(p, s)| Some((p.parse().ok()?, s.to_string())))).collect();
    let ended = end_orphans(&earlier);
    let state = Arc::new(State { roots, spawned: Mutex::new(Vec::new()), noted: Mutex::new(Vec::new()), file });
    ALL.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&state));
    CURRENT.with(|c| *c.borrow_mut() = Some(state.clone()));
    SAMPLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            sample();
            std::thread::sleep(EVERY);
        });
    });
    (Watch(state), ended)
}

/// Registers `pid`, a worker or script the run on this thread spawned, as one of its own processes.
pub fn own(pid: u32) {
    CURRENT.with(|c| {
        if let Some(state) = c.borrow().as_ref() {
            state.spawned.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
        }
    });
}

impl Watch {
    /// Ends what the step that ran left running: its noted processes that are orphans now. Returns how many.
    pub fn reap(&self) -> usize {
        let noted = std::mem::take(&mut *self.0.noted.lock().unwrap_or_else(|e| e.into_inner()));
        end_orphans(&noted)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            let mut current = c.borrow_mut();
            if current.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.0)) {
                *current = None;
            }
        });
    }
}

/// One pass of the sampler: the process table once, and what every live watch has below its roots.
fn sample() {
    let live: Vec<Arc<State>> = {
        let mut all = ALL.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|w| w.strong_count() > 0);
        all.iter().filter_map(Weak::upgrade).collect()
    };
    if live.is_empty() {
        return;
    }
    let table = table();
    for state in live {
        let roots: Vec<u32> = match state.roots {
            Roots::Process => vec![std::process::id()],
            Roots::Spawned => state.spawned.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        let seen: Vec<Process> = roots.iter().flat_map(|r| descendants(*r, &table)).collect();
        note(&state, &seen);
    }
}

/// Adds `seen` to the watch's notes of the current step and writes them to its file.
fn note(state: &State, seen: &[Process]) {
    let mut noted = state.noted.lock().unwrap_or_else(|e| e.into_inner());
    for p in seen {
        if !noted.contains(p) {
            noted.push(p.clone());
        }
    }
    let text: String = noted.iter().map(|(p, s)| format!("{p}\t{s}\n")).collect();
    let _ = std::fs::write(&state.file, text);
}

/// Every process of this user: pid, parent pid, start time.
fn table() -> Vec<(u32, u32, String)> {
    let out = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,lstart="]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    text.lines().filter_map(|l| {
        let mut w = l.split_whitespace();
        let (pid, ppid) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
        Some((pid, ppid, w.collect::<Vec<_>>().join(" ")))
    }).collect()
}

/// The descendants of `root` in `table`.
fn descendants(root: u32, table: &[(u32, u32, String)]) -> Vec<Process> {
    let mut found: Vec<Process> = vec![];
    let mut parents = HashSet::from([root]);
    loop {
        let new: Vec<_> = table.iter().filter(|(p, pp, _)| parents.contains(pp) && !parents.contains(p)).collect();
        if new.is_empty() {
            return found;
        }
        for (pid, _, start) in new {
            parents.insert(*pid);
            found.push((*pid, start.clone()));
        }
    }
}

/// Ends each of `processes` that still runs (same start time) and whose parent is init; returns how many.
fn end_orphans(processes: &[Process]) -> usize {
    let now = table();
    let orphans: Vec<String> = processes.iter()
        .filter(|(pid, start)| now.iter().any(|(p, pp, s)| p == pid && *pp == 1 && s == start))
        .map(|(pid, _)| pid.to_string()).collect();
    if orphans.is_empty() {
        return 0;
    }
    let kill = |signal: &str| { let _ = std::process::Command::new("kill").arg(signal).args(&orphans).status(); };
    kill("-TERM");
    std::thread::sleep(Duration::from_secs(2));
    kill("-KILL");
    orphans.len()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A background process a child started is a descendant while its parent runs and an orphan after; then
    /// it is ended, and a process that still has its parent is not.
    #[test]
    fn an_orphaned_descendant_is_ended_and_a_parented_one_kept() {
        let me = std::process::id();
        let mut parent = std::process::Command::new("sh").args(["-c", "sleep 60 >/dev/null 2>&1 & sleep 1"])
            .spawn().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(descendants(me, &table()).iter().any(|(p, _)| *p == parent.id()));
        // only what this test's own child started: other tests of the process start children too
        let seen = descendants(parent.id(), &table());
        assert_eq!(end_orphans(&seen), 0);   // its parent still runs
        parent.wait().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(end_orphans(&seen), 1);   // the background sleep, adopted by init now
        assert_eq!(end_orphans(&seen), 0);
    }
}

#[cfg(all(test, unix))]
mod lane_tests {
    use super::*;

    /// Whether process `pid` still runs (not a zombie: `ps` prints its state).
    fn alive(pid: u32) -> bool {
        let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }

    /// A background process a shell started: its pid, once it has printed it.
    fn background(watch: &Watch) -> (std::process::Child, u32) {
        let _ = watch;
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "sleep 61 >/dev/null 2>&1 & echo $!; sleep 1"])
            .stdout(std::process::Stdio::piped()).spawn().unwrap();
        own(shell.id());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(shell.stdout.take().unwrap()), &mut line).unwrap();
        let pid = line.trim().parse().unwrap();
        (shell, pid)
    }

    /// Two runs in one process: when one run's step ends, only what its own step left running ends.
    #[test]
    fn a_step_end_in_one_run_ends_only_that_runs_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        // a lane's watch on this thread, and a sibling's on another: each registers the shell it spawns
        let (mine, _) = watch(dir.path(), Roots::Spawned);
        let (mut shell_a, sleeper_a) = background(&mine);
        let theirs = std::thread::spawn(|| {
            let dir = tempfile::tempdir().unwrap();
            let (other, _) = watch(dir.path(), Roots::Spawned);
            let (shell, sleeper) = background(&other);
            (other, shell, sleeper, dir)
        }).join().unwrap();
        let (other, mut shell_b, sleeper_b, _keep) = theirs;
        sample();
        sample();
        shell_a.wait().unwrap();
        shell_b.wait().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(mine.reap(), 1, "the sleeper its shell left");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!alive(sleeper_a), "a's leftover ended");
        assert!(alive(sleeper_b), "b's leftover is b's to end");
        assert_eq!(other.reap(), 1);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!alive(sleeper_b));
    }
}
