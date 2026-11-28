//! The files a lane keeps apart from the run's: the append-only ones every step writes at the end (the history, the
//! notes, the open points), which would conflict at the join if every lane appended to the tracked file, and the worker
//! memory under `rlt/state/`. A lane works on copies, and the run takes back what it added, in item order, when it joins
//! the lane.

use super::format::Process;
use crate::lanes::worktree::copy_dir;
use std::path::{Path, PathBuf};

/// A file of the run a lane keeps its own copy of: where the run has it, where the lane has it, and how long the run's
/// was when the lane started (what the lane added lies after that).
#[derive(Debug, Clone)]
pub struct Redirect {
    pub run_path: String,
    pub lane_path: String,
    pub seed_len: u64,
}

/// The lane's copy of the process: the history, the notes and the open points go to files under `rlt/private/lane/`,
/// seeded with the run's content. The redirects to take back when the lane is joined.
pub fn redirected(spec: &Process, project: &Path, lane_dir: &Path) -> (Process, Vec<Redirect>) {
    let mut spec = spec.clone();
    let mut redirects = vec![];
    let mut take = |slot: &mut Option<String>, name: &str| {
        let Some(path) = slot.clone() else { return };
        let lane_path = format!("rlt/private/lane/{name}");
        let to = lane_dir.join(&lane_path);
        let sidecar = lane_dir.join(format!("{lane_path}.seed"));
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // a resumed lane keeps what it wrote, and the length of the run's file when it started: the run's file may have
        // grown since (lanes joined before it)
        let seed_len = match (to.exists(), std::fs::read_to_string(&sidecar).ok().and_then(|t| t.trim().parse::<u64>().ok())) {
            (true, Some(n)) => n,
            _ => {
                let seed = std::fs::read(project.join(&path)).unwrap_or_default();
                let _ = std::fs::write(&to, &seed);
                let _ = std::fs::write(&sidecar, seed.len().to_string());
                seed.len() as u64
            }
        };
        redirects.push(Redirect { run_path: path, lane_path: lane_path.clone(), seed_len });
        *slot = Some(lane_path);
    };
    take(&mut spec.run.history, "history");
    take(&mut spec.run.notes, "notes");
    take(&mut spec.run.open_points, "open-points");
    (spec, redirects)
}

/// Appends to the run's files what the lane added after the content it was seeded with, under the lane's key for the
/// plain text files; the history's lines are the lane's own, as they are.
pub fn take_back(project: &Path, lane_dir: &Path, key: &str, redirects: &[Redirect]) {
    for r in redirects {
        let Ok(bytes) = std::fs::read(lane_dir.join(&r.lane_path)) else { continue };
        let added = bytes.get(r.seed_len as usize..).unwrap_or_default();
        if added.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let target = project.join(&r.run_path);
        let _ = target.parent().map(std::fs::create_dir_all);
        let mut text = std::fs::read(&target).unwrap_or_default();
        if !text.is_empty() && !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        if !r.lane_path.ends_with("history") {
            text.extend_from_slice(format!("<!-- lane {key} -->\n").as_bytes());
        }
        text.extend_from_slice(added);
        let _ = std::fs::write(target, text);
    }
}

/// Seeds the lane's `rlt/state/` (the worker memory its briefs read) with the run's; the lengths of the files, for
/// [`take_back_state`].
pub fn seed_state(project: &Path, lane_dir: &Path) {
    let (from, to) = (crate::paths::rlt_dir(project), crate::paths::rlt_dir(lane_dir));
    if from.is_dir() && !to.exists() {
        copy_dir(&from, &to);
        // what belongs to the run alone: the workers' ask requests, long tasks, the flow's scratch
        for own in OWN {
            let _ = std::fs::remove_dir_all(to.join(own));
            let _ = std::fs::remove_file(to.join(own));
        }
    }
}

/// What under `rlt/state/` is the run's alone and stays out of a lane.
const OWN: [&str; 4] = ["asks", "flow", "spawns", "spawns.json"];

/// Whether `rel` (under `rlt/state/`) is one of [`OWN`].
fn is_own(rel: &Path) -> bool {
    rel.components().next().and_then(|c| c.as_os_str().to_str()).is_some_and(|c| OWN.contains(&c))
}

/// Takes back the memory a lane wrote: a file the run lacks comes as it is, one both have grows by what the lane added
/// after the run's content (as it was when the lane started, `seed`), under the lane's key.
pub fn take_back_state(project: &Path, lane_dir: &Path, key: &str, seed: &[(PathBuf, u64)]) {
    let (lane_state, run_state) = (crate::paths::rlt_dir(lane_dir), crate::paths::rlt_dir(project));
    for file in files(&lane_state) {
        let rel = file.strip_prefix(&lane_state).unwrap_or(&file).to_path_buf();
        if is_own(&rel) {
            continue;
        }
        let target = run_state.join(&rel);
        let Ok(bytes) = std::fs::read(&file) else { continue };
        if !target.exists() {
            let _ = target.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(&target, bytes);
            continue;
        }
        let was = seed.iter().find(|(p, _)| *p == rel).map_or(0, |(_, n)| *n) as usize;
        let added = bytes.get(was..).unwrap_or_default();
        if added.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let mut text = std::fs::read(&target).unwrap_or_default();
        text.extend_from_slice(format!("\n<!-- lane {key} -->\n").as_bytes());
        text.extend_from_slice(added);
        let _ = std::fs::write(&target, text);
    }
}

/// The lengths of the run's `rlt/state/` files that the lane in `lane_dir` was seeded with: measured when it is seeded and
/// kept with it, so a resumed lane takes back what it added after those lengths, not after the files' lengths now.
pub fn state_lengths_kept(project: &Path, lane_dir: &Path) -> Vec<(PathBuf, u64)> {
    let sidecar = lane_dir.join("rlt/private/lane/state.seed.json");
    if let Some(kept) = std::fs::read_to_string(&sidecar).ok().and_then(|t| serde_json::from_str::<Vec<(PathBuf, u64)>>(&t).ok()) {
        return kept;
    }
    let now = state_lengths(project);
    let _ = sidecar.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(&sidecar, serde_json::to_string(&now).unwrap_or_default());
    now
}

/// The files of the run's `rlt/state/` and their lengths: what a lane is seeded with.
pub fn state_lengths(project: &Path) -> Vec<(PathBuf, u64)> {
    let root = crate::paths::rlt_dir(project);
    files(&root).into_iter().filter_map(|f| Some((f.strip_prefix(&root).ok()?.to_path_buf(), f.metadata().ok()?.len())))
        .filter(|(rel, _)| !is_own(rel)).collect()
}

/// Every file below `dir`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(extra: &str) -> Process {
        Process::parse(&format!("version: 1\nrun:\n{extra}\nphases: []\n")).unwrap()
    }

    /// The tracked append-only files are copied into the lane's private folder and what the lane adds comes back after
    /// the run's own content, the notes under the lane's key, the history's lines as they are.
    #[test]
    fn what_a_lane_adds_to_the_append_only_files_comes_back_in_order() {
        let (run, lane) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(run.path().join("steps.jsonl"), "{\"a\":1}\n").unwrap();
        std::fs::write(run.path().join("notes.md"), "## earlier\nline\n").unwrap();
        let (lane_spec, redirects) = redirected(&spec("  history: steps.jsonl\n  notes: notes.md"), run.path(), lane.path());
        assert_eq!(lane_spec.run.history.as_deref(), Some("rlt/private/lane/history"));
        assert_eq!(lane_spec.run.notes.as_deref(), Some("rlt/private/lane/notes"));
        assert!(lane_spec.run.open_points.is_none());
        for (path, line) in [("rlt/private/lane/history", "{\"lane\":1}\n"), ("rlt/private/lane/notes", "## build-U1\nmine\n")] {
            let mut text = std::fs::read_to_string(lane.path().join(path)).unwrap();
            text.push_str(line);
            std::fs::write(lane.path().join(path), text).unwrap();
        }
        // another lane joined first and wrote after the seed
        std::fs::write(run.path().join("steps.jsonl"), "{\"a\":1}\n{\"other\":1}\n").unwrap();
        take_back(run.path(), lane.path(), "U1", &redirects);
        assert_eq!(std::fs::read_to_string(run.path().join("steps.jsonl")).unwrap(), "{\"a\":1}\n{\"other\":1}\n{\"lane\":1}\n");
        let notes = std::fs::read_to_string(run.path().join("notes.md")).unwrap();
        assert_eq!(notes, "## earlier\nline\n<!-- lane U1 -->\n## build-U1\nmine\n");
    }

    /// A resumed lane keeps the length its seed had: the run's file may have grown since (lanes joined before it).
    #[test]
    fn a_resumed_lane_keeps_the_length_of_its_seed() {
        let (run, lane) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(run.path().join("notes.md"), "seed\n").unwrap();
        let (_, first) = redirected(&spec("  notes: notes.md"), run.path(), lane.path());
        assert_eq!(first[0].seed_len, 5);
        std::fs::write(run.path().join("notes.md"), "seed\nadded by a lane joined before\n").unwrap();
        let (_, again) = redirected(&spec("  notes: notes.md"), run.path(), lane.path());
        assert_eq!(again[0].seed_len, 5, "not the run's file's length now");
    }

    /// The lengths of the memory files a lane was seeded with are kept with it.
    #[test]
    fn the_memory_lengths_of_a_lane_are_kept_with_it() {
        let (run, lane) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::paths::rlt_dir(run.path());
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("STATE.md"), "base\n").unwrap();
        assert_eq!(state_lengths_kept(run.path(), lane.path()), [(PathBuf::from("STATE.md"), 5)]);
        std::fs::write(state.join("STATE.md"), "base\ngrown since\n").unwrap();
        assert_eq!(state_lengths_kept(run.path(), lane.path()), [(PathBuf::from("STATE.md"), 5)], "the first measure stays");
    }

    /// Worker memory: new files come as they are, a shared one grows by what the lane added.
    #[test]
    fn worker_memory_comes_back_new_files_whole_and_shared_ones_by_what_was_added() {
        let (run, lane) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::paths::rlt_dir(run.path());
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("STATE.md"), "base\n").unwrap();
        let seed = state_lengths(run.path());
        seed_state(run.path(), lane.path());
        let lane_state = crate::paths::rlt_dir(lane.path());
        assert_eq!(std::fs::read_to_string(lane_state.join("STATE.md")).unwrap(), "base\n");
        std::fs::write(lane_state.join("STATE.md"), "base\nadded by the lane\n").unwrap();
        std::fs::write(lane_state.join("mem-U1.md"), "own memory").unwrap();
        take_back_state(run.path(), lane.path(), "U1", &seed);
        assert_eq!(std::fs::read_to_string(state.join("mem-U1.md")).unwrap(), "own memory");
        assert_eq!(std::fs::read_to_string(state.join("STATE.md")).unwrap(), "base\n\n<!-- lane U1 -->\nadded by the lane\n");
    }
}
