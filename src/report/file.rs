//! The report on disk: the per-run handle, the stale-report rule, and the atomic write.

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::Report;

/// Where the report lives: `agg/private/report.json`. Private because the worker forging it could
/// change "what agg believes happened" — the classification rule in `paths.rs`.
pub fn path(dir: &Path) -> PathBuf {
    crate::paths::report_json(dir)
}

thread_local! {
    /// The report this THREAD last wrote. `driver::run` reads it to turn a driver's `Err` return
    /// into `outcome: error` after `Drop for Agg` already wrote the report — thread-local because a
    /// test binary runs many drivers on parallel threads, and a process-wide slot would cross them.
    static LAST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The report path this thread wrote last, if any.
pub fn last_written_on_this_thread() -> Option<PathBuf> {
    LAST.with(|l| l.borrow().clone())
}

/// One run's identity and its "has the report been written" latch.
pub struct RunHandle {
    dir: PathBuf,
    run_id: String,
    started_at: u64,
    written: Cell<bool>,
}

/// START a run's reporting: delete any previous report, mint a fresh `run_id`, and stamp it into
/// `agg/private/run.log`.
///
/// ⚠ Call it only behind the double-run guard: the report it deletes would otherwise be a LIVE
/// run's. Deleting first is the "no stale reports" rule — a consumer that finds a report after
/// this point is reading this run's, or none.
pub fn begin(dir: &Path) -> RunHandle {
    begin_as(dir, None)
}

/// [`begin`] for a run that CONTINUES an earlier one (`agg run --resume`): it reports under the
/// earlier run's id, so both halves of one run carry one `run_id`. `None` mints a fresh one.
pub fn begin_as(dir: &Path, run_id: Option<String>) -> RunHandle {
    clear(dir);
    let run_id = run_id.unwrap_or_else(mint_run_id);
    log_line(dir, &format!("agg run {run_id} started (pid {})", std::process::id()));
    RunHandle { dir: dir.to_path_buf(), run_id, started_at: crate::util::now_epoch(), written: Cell::new(false) }
}

impl RunHandle {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn started_at(&self) -> u64 {
        self.started_at
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// Whether this run's report is already on disk — so a fallback never overwrites a rich one.
    pub fn written(&self) -> bool {
        self.written.get()
    }

    /// Write this run's report and log its ending next to the start line.
    pub fn finish(&self, report: &Report) -> std::io::Result<()> {
        write(&self.dir, report)?;
        self.written.set(true);
        log_line(
            &self.dir,
            &format!("agg run {} finished: {:?} (exit {})", self.run_id, report.outcome, report.exit_code),
        );
        Ok(())
    }

    /// Write a bare `error` report under THIS run's id — for a fault before the loop state existed,
    /// or a panic the rich report could not be built through.
    pub fn finish_error(&self, project: &str, error: &str) -> std::io::Result<()> {
        self.finish(&super::build::fallback(&self.dir, project, &self.run_id, self.started_at, error))
    }
}

/// `run-<epoch>-<pid>-<n>`: unique on a host without a UUID dependency — the pid separates two runs
/// started in the same second, the counter two runs of one process.
fn mint_run_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("run-{}-{}-{}", crate::util::now_epoch(), std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// Append one timestamped line to `agg/private/run.log`. Best-effort: the log is for humans, and a
/// failed log line must never fail a run.
fn log_line(dir: &Path, line: &str) {
    let log = crate::paths::run_log(dir);
    let _ = std::fs::create_dir_all(crate::paths::private_dir(dir));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "[{}] {line}", crate::util::human_time(crate::util::now_epoch()));
    }
}

/// Delete the report, if any.
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(path(dir));
}

/// The report's raw text, if one exists.
pub fn read_text(dir: &Path) -> Option<String> {
    std::fs::read_to_string(path(dir)).ok()
}

/// Write `report` ATOMICALLY: a temp file in the same directory, fsync'd, then renamed over the
/// target. A reader sees the old file, no file, or the whole new one — never a torn report.
///
/// Not [`crate::util::write_atomic`]: that one skips the fsync, so after a power cut the rename can
/// be durable while the bytes are not, leaving exactly the empty or partial report this forbids.
pub fn write(dir: &Path, report: &Report) -> std::io::Result<()> {
    let target = path(dir);
    std::fs::create_dir_all(crate::paths::private_dir(dir))?;
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    let tmp = target.with_file_name(format!(".report.json.{}.tmp", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &target)?;
    // the rename itself is durable only once the directory entry is; best-effort where a directory
    // cannot be opened for sync.
    if let Ok(d) = std::fs::File::open(crate::paths::private_dir(dir)) {
        let _ = d.sync_all();
    }
    LAST.with(|l| *l.borrow_mut() = Some(target));
    Ok(())
}

/// Rewrite the report this thread last wrote as an `error` ending (exit 1), keeping everything else
/// it recorded. A no-op when this thread wrote none.
pub fn amend_to_error(error: &str) -> std::io::Result<()> {
    let Some(p) = last_written_on_this_thread() else { return Ok(()) };
    let text = std::fs::read_to_string(&p)?;
    let mut rep: Report = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    rep.outcome = super::Outcome::Error;
    rep.exit_code = 1;
    rep.error = Some(error.to_string());
    rep.stop_reason = format!("error: {error}");
    // `report.json` lives at `<dir>/agg/private/report.json`, so the project dir is three up.
    let dir = p.ancestors().nth(3).map(Path::to_path_buf).unwrap_or_default();
    write(&dir, &rep)
}

/// Build (and, if `persist`, write) the report for a run that never got far enough to build its
/// own: config errors, a refused start, a fault before the loop state existed.
///
/// `persist` is false when ANOTHER run is live in the project: the report file is that run's, and a
/// refused second start must not overwrite it. The report is still returned, for `--report-json`.
pub fn write_fallback(dir: &Path, project: &str, error: &str, persist: bool) -> (Report, std::io::Result<()>) {
    let started_at = crate::util::now_epoch();
    let rep = super::build::fallback(dir, project, &mint_run_id(), started_at, error);
    let res = if persist { write(dir, &rep) } else { Ok(()) };
    (rep, res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// `begin` is the no-stale-reports rule: whatever the previous run left is gone before this run
    /// can end, and the new run's id is in the log.
    #[test]
    fn begin_deletes_the_previous_report_and_logs_a_fresh_run_id() {
        let d = project();
        let (old, _) = write_fallback(d.path(), "p", "previous run", true);
        assert!(path(d.path()).exists());

        let run = begin(d.path());
        assert!(!path(d.path()).exists(), "a stale report must not survive the start of a run");
        assert_ne!(run.run_id(), old.run_id, "every run gets a fresh id");
        let log = std::fs::read_to_string(crate::paths::run_log(d.path())).unwrap();
        assert!(log.contains(run.run_id()), "the run id must be in run.log:\n{log}");
        assert!(!run.written());
    }

    /// The write leaves exactly the report and no temp file, and what is on disk parses back to what
    /// was written.
    #[test]
    fn write_is_whole_and_leaves_no_temp_file() {
        let d = project();
        let run = begin(d.path());
        let rep = super::super::build::fallback(d.path(), "p", run.run_id(), run.started_at(), "x");
        run.finish(&rep).unwrap();
        assert!(run.written());
        let back: Report = serde_json::from_str(&read_text(d.path()).unwrap()).unwrap();
        assert_eq!(back, rep);
        let leftovers: Vec<_> = std::fs::read_dir(crate::paths::private_dir(d.path()))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a temp file was left behind: {leftovers:?}");
        assert_eq!(last_written_on_this_thread(), Some(path(d.path())));
    }

    /// A driver's `Err` return reaches `driver::run` after the report was written: the amendment
    /// flips the ending to `error` and keeps every fact the run recorded.
    #[test]
    fn amend_turns_the_last_report_into_an_error_and_keeps_the_rest() {
        let d = project();
        let run = begin(d.path());
        let mut rep = super::super::build::fallback(d.path(), "p", run.run_id(), run.started_at(), "x");
        rep.outcome = super::super::Outcome::Done;
        rep.exit_code = 0;
        rep.error = None;
        rep.sessions.total = 3;
        run.finish(&rep).unwrap();

        amend_to_error("driver failed").unwrap();
        let back: Report = serde_json::from_str(&read_text(d.path()).unwrap()).unwrap();
        assert_eq!((back.outcome, back.exit_code), (super::super::Outcome::Error, 1));
        assert_eq!(back.error.as_deref(), Some("driver failed"));
        assert_eq!((back.sessions.total, back.run_id.as_str()), (3, run.run_id()), "the rest is kept");
    }

    /// A refused start (another run is live) must not touch that run's file, but still yields a
    /// report to print.
    #[test]
    fn a_fallback_that_may_not_persist_writes_nothing() {
        let d = project();
        let (rep, res) = write_fallback(d.path(), "p", "another run is live", false);
        assert!(res.is_ok());
        assert_eq!(rep.outcome, super::super::Outcome::Error);
        assert!(!path(d.path()).exists());
    }

    /// An unwritable private dir is an `Err` the caller turns into exit 1 — never a silent success.
    #[test]
    fn an_unwritable_report_is_an_error() {
        let d = project();
        // a FILE where the private dir should be: create_dir_all fails, whatever the uid.
        std::fs::create_dir_all(crate::paths::config_base(d.path())).unwrap();
        std::fs::write(crate::paths::private_dir(d.path()), "not a dir").unwrap();
        let rep = super::super::build::fallback(d.path(), "p", "r", 0, "x");
        assert!(write(d.path(), &rep).is_err());
    }
}
