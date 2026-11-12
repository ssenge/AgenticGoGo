//! The processes a step leaves running (A-37): every few seconds a sampler notes the run's descendants;
//! when a step ends, the noted ones that are orphans now (their parent gone, adopted by init) end too. The notes are
//! kept in the project's git folder, so a run stopped from outside is cleaned up by the next one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// A process by its pid and start time (a pid alone may be reused).
type Process = (u32, String);

/// The processes the current step started, as noted by the sampler.
static NOTED: Mutex<Vec<Process>> = Mutex::new(Vec::new());
const FILE: &str = "agg-processes";
const EVERY: Duration = Duration::from_secs(2);

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

/// Starts the sampler for the run of `project`, after ending what an earlier, stopped run left noted there; how many
/// that was.
pub fn watch(project: &Path) -> usize {
    let file = project.join(".git").join(FILE);
    let earlier: Vec<Process> = std::fs::read_to_string(&file).unwrap_or_default().lines()
        .filter_map(|l| l.split_once('\t').and_then(|(p, s)| Some((p.parse().ok()?, s.to_string())))).collect();
    let ended = end_orphans(&earlier);
    let me = std::process::id();
    std::thread::spawn(move || loop {
        note(&descendants(me, &table()), &file);
        std::thread::sleep(EVERY);
    });
    ended
}

/// Adds `seen` to the current step's notes and writes them to `file`.
fn note(seen: &[Process], file: &PathBuf) {
    let Ok(mut noted) = NOTED.lock() else { return };
    for p in seen {
        if !noted.contains(p) {
            noted.push(p.clone());
        }
    }
    let text: String = noted.iter().map(|(p, s)| format!("{p}\t{s}\n")).collect();
    let _ = std::fs::write(file, text);
}

/// Ends what the step that ran left running: its noted processes that are orphans now. Returns how many.
pub fn reap() -> usize {
    let noted = NOTED.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
    end_orphans(&noted)
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
        let seen = descendants(me, &table());
        assert!(seen.iter().any(|(p, _)| *p == parent.id()));
        assert_eq!(end_orphans(&seen), 0);   // its parent still runs
        parent.wait().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(end_orphans(&seen), 1);   // the background sleep, adopted by init now
        assert_eq!(end_orphans(&seen), 0);
    }
}
