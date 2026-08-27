//! The message bus — session-granular bidirectional comms.
//!
//! Honest platform truth: you CANNOT inject a message into a running headless
//! `claude -p` worker mid-session (Channels don't work in `-p`). So steering is
//! **session-granular**: the loop drains `bus/in/` at each session boundary and
//! applies the commands before launching the next worker.
//!
//! Layout under the project's AGG-OWNED `agg/private/` (the bus is the OPERATOR's channel — a
//! worker able to write here would raise its own budget or unpause itself, so it is carved out of
//! the sandbox writable set):
//! ```text
//! agg/private/bus/
//!   in/        # operator / outer-Claude → loop  (one JSON file per command)
//!   out/       # loop → operator                 (status/questions)
//!   log.jsonl  # append-only audit of everything drained
//! ```
//! A command is a tiny JSON file; the loop reads all of `in/`, sorts by filename
//! (timestamped on send), applies them in order, archives them to the log, and
//! deletes them — so a command is consumed exactly once.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A steering command from the operator (or outer Claude) to the loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    /// Prepend a high-priority instruction to the NEXT worker's resume prompt.
    InjectInstruction { text: String },
    /// Raise/lower the token budget (None = unlimited).
    SetBudget { total: Option<u64> },
    /// Pause the loop before the next session until a `resume`/`stop` arrives.
    Pause,
    /// Resume a paused loop.
    Resume,
    /// Stop the loop gracefully after the current session boundary.
    Stop { reason: String },
    /// A free-form note (logged, shown once; no behavior change).
    Note { text: String },
    //
    // ⛔ There is deliberately NO `Answer` here. An answer to a human ask is a DURABLE FACT, not a
    // steering message: it belongs in `agg/private/asks.jsonl`, which outlives the workflow that
    // raised the question, and a blocked driver polls that ledger rather than this queue. Routing it
    // through the bus made it the one command that had to be exempted from the liveness rule below,
    // which is what exposed the confusion. See `core::asks` and `agg answer`.
}

/// Queue a steering command onto a project's bus with a send-ordered filename.
///
/// Shared by every channel — the CLI (`agg send`) and the web API (`POST /api/send`) — so they
/// cannot disagree about what sending means. **The liveness rule lives HERE**, not in the callers:
/// it used to be decided independently in two files, and they decided differently (the CLI queued
/// with a warning, the API refused with 409), which is how the same command came to behave
/// differently depending on which channel you used.
///
/// # A queue only exists while a workflow runs
///
/// These are files, so they *can* sit on disk with nothing listening — but a steering message with
/// no workflow to steer is not a queued message, it is a landmine: a `stop` written now would fire
/// at the startup of whatever runs next, hours later, with nobody connecting the two. So sending
/// with no workflow running is an ERROR naming the missing prerequisite, and [`purge`] clears
/// anything stale when a workflow starts.
pub fn queue_command(dir: &Path, cmd: &Command) -> std::io::Result<PathBuf> {
    if crate::os::detach::live_pid(dir).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "no workflow is running in this project, so there is no queue to append to — \
             start one with `agg run` and send again. (A human ask is different: it outlives its \
             workflow, so answer it any time with `agg answer <id> <value>`.)",
        ));
    }
    let b = Bus::open(dir)?;
    // monotonic-ish millis stamp for send-order filenames.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{:013}", d.as_millis()))
        .unwrap_or_else(|_| "0000000000000".into());
    b.send(cmd, &stamp)
}

/// Drop every pending inbound command. Called when a workflow STARTS.
///
/// Without this a queue outlives the workflow it was meant for: a message sent while the last run
/// was alive, and never drained because the run ended first, would be applied by an unrelated run
/// days later. Only `in/` is cleared — `out/` is the operator's own record of what the workflow
/// said, and the durable facts (asks and their answers) live in the ledger, not here.
pub fn purge(dir: &Path) -> usize {
    let Ok(b) = Bus::open(dir) else { return 0 };
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(&b.inbox) {
        for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            if let Ok(text) = std::fs::read_to_string(&p) {
                b.append_log("purged-stale", &text);
            }
            if std::fs::remove_file(&p).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Resolve the bus directories under a project dir, creating them if needed.
pub struct Bus {
    pub inbox: PathBuf,
    pub outbox: PathBuf,
    pub log: PathBuf,
}

impl Bus {
    pub fn open(dir: &Path) -> std::io::Result<Bus> {
        let root = crate::paths::bus_dir(dir);
        let inbox = root.join("in");
        let outbox = root.join("out");
        std::fs::create_dir_all(&inbox)?;
        std::fs::create_dir_all(&outbox)?;
        Ok(Bus { log: root.join("log.jsonl"), inbox, outbox })
    }

    /// Send a command into `in/`. The filename is `{stamp}-{pid}-{seq}.json` — the
    /// pid+monotonic-seq suffix guarantees uniqueness even for two commands sent in
    /// the same millisecond (a bare `{stamp}.json` would silently overwrite the first,
    /// breaking the consumed-exactly-once guarantee). Same-width seq keeps lexicographic
    /// == send order. The caller's `stamp` should be a fixed-width millis epoch.
    pub fn send(&self, cmd: &Command, stamp: &str) -> std::io::Result<PathBuf> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let file = self.inbox.join(format!("{stamp}-{}-{seq:06}.json", std::process::id()));
        // ATOMIC: write to a sibling tmp path, then rename INTO the drained inbox. A plain write
        // makes the file visible in `in/` the instant it's created, so `drain()` (which picks up
        // any *.json) could read a half-written command — a real race for a programmatic sender
        // like the web API. rename(2) within the same dir is atomic, so drain only ever sees a
        // complete file. The `.tmp` suffix is not `.json`, so a mid-write tmp is never drained.
        let tmp = self.inbox.join(format!(".{stamp}-{}-{seq:06}.json.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(cmd).unwrap_or_default())?;
        std::fs::rename(&tmp, &file)?;
        Ok(file)
    }

    /// Drain ALL pending commands from `in/`, in filename (send) order. Each is
    /// archived to `log.jsonl` then deleted, so it's consumed exactly once.
    pub fn drain(&self) -> Vec<Command> {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&self.inbox) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect(),
            Err(_) => return vec![],
        };
        entries.sort();

        let mut out = Vec::new();
        for path in entries {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            match serde_json::from_str::<Command>(&text) {
                Ok(cmd) => {
                    self.append_log("drained", &text);
                    out.push(cmd);
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => {
                    // malformed command: log it and move it aside, don't crash.
                    self.append_log("bad-command", &format!("{{\"error\":\"{e}\",\"file\":\"{}\"}}", path.display()));
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        out
    }

    /// Write a message to the operator (`out/`), e.g. a question or a status push.
    pub fn emit(&self, kind: &str, text: &str, stamp: &str) -> std::io::Result<()> {
        let file = self.outbox.join(format!("{stamp}-{kind}.txt"));
        std::fs::write(&file, text)?;
        self.append_log("emit", &format!("{{\"kind\":\"{kind}\"}}"));
        Ok(())
    }

    fn append_log(&self, event: &str, payload: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log) {
            let _ = writeln!(f, "{{\"event\":\"{event}\",\"payload\":{payload}}}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        // unique per-test dir (tests run in parallel — must not share a bus dir)
        let base = std::env::temp_dir().join(format!("agg-bus-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn send_drain_roundtrip_in_order() {
        let dir = tmpdir("roundtrip");
        let bus = Bus::open(&dir).unwrap();
        bus.send(&Command::Note { text: "first".into() }, "0001").unwrap();
        bus.send(&Command::InjectInstruction { text: "do X".into() }, "0002").unwrap();
        bus.send(&Command::Stop { reason: "done".into() }, "0003").unwrap();

        let cmds = bus.drain();
        assert_eq!(cmds.len(), 3);
        // order preserved by filename sort
        matches!(cmds[0], Command::Note { .. });
        matches!(cmds[1], Command::InjectInstruction { .. });
        matches!(cmds[2], Command::Stop { .. });

        // consumed exactly once: a second drain is empty
        assert!(bus.drain().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_command_does_not_crash_drain() {
        let dir = tmpdir("malformed");
        let bus = Bus::open(&dir).unwrap();
        std::fs::write(bus.inbox.join("0001.json"), "{not valid json").unwrap();
        bus.send(&Command::Pause, "0002").unwrap();
        let cmds = bus.drain();
        assert_eq!(cmds.len(), 1); // the good one survives, the bad one is logged+removed
        matches!(cmds[0], Command::Pause);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_stamp_sends_do_not_collide() {
        let dir = tmpdir("collide");
        let bus = Bus::open(&dir).unwrap();
        // two commands with the SAME stamp must both survive (unique filenames)
        bus.send(&Command::Note { text: "a".into() }, "0001").unwrap();
        bus.send(&Command::Note { text: "b".into() }, "0001").unwrap();
        assert_eq!(bus.drain().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn command_tags_serialize_kebab() {
        let j = serde_json::to_string(&Command::InjectInstruction { text: "hi".into() }).unwrap();
        assert!(j.contains("\"cmd\":\"inject-instruction\""));
        let j = serde_json::to_string(&Command::SetBudget { total: Some(5) }).unwrap();
        assert!(j.contains("\"cmd\":\"set-budget\""));
    }
}
