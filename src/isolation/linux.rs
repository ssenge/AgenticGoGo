//! Linux OS sandbox via `bwrap` (bubblewrap), rootless through unprivileged user namespaces.
//!
//! Recipe (`internal/ISOLATION.md` §6 / §10.5):
//! ```text
//! bwrap --die-with-parent --ro-bind / / --bind <cwd> <cwd> [--bind <w> <w> …]
//!       --tmpfs /tmp --proc /proc --dev /dev --share-net --chdir <cwd> -- <prog> <args…>
//! ```
//! `--ro-bind / /` makes the whole host read-only (the agent still READS its auth, binaries, node);
//! `--bind <cwd>` punches the one writable hole; extra `--bind`s add the agent's own state dirs;
//! `--tmpfs /tmp` gives a writable scratch tmp; `--share-net` leaves the network open.
//!
//! Caveat: needs unprivileged user namespaces (default on most distros; some hardened/enterprise
//! images disable it). We probe only that `bwrap` runs — a userns-disabled host surfaces at spawn.

use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Is `bwrap` on PATH and runnable?
pub fn available() -> bool {
    Command::new("bwrap")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the `bwrap` wrapper command around `prog args…`, writable = cwd + tmpfs /tmp + `writable`,
/// minus `denied` (agg's own private state, which lives inside cwd).
pub fn build(cwd: &Path, writable: &[PathBuf], denied: &[PathBuf], prog: &OsStr, args: &[OsString]) -> Result<Command> {
    let mut cmd = Command::new("bwrap");
    cmd.arg("--die-with-parent")
        // whole host read-only …
        .arg("--ro-bind").arg("/").arg("/")
        // … except the one writable hole: the project cwd (+ all subfolders)
        .arg("--bind").arg(cwd).arg(cwd);
    // …plus the agent's own state dirs (session logs etc.) — already filtered to existing dirs.
    for w in writable {
        cmd.arg("--bind").arg(w).arg(w);
    }
    // The CARVE-OUT: re-bind agg's private state READ-ONLY on top of the writable cwd. bwrap applies
    // binds in argv ORDER, so this must come AFTER `--bind <cwd>` — reversing them lets the later
    // writable bind win and silently reopens the hole.
    //
    // `--ro-bind`, not `--tmpfs`: reads must keep working (a judge reads `verdicts.jsonl`, the
    // worker reads its brief). A path that does not exist is skipped — bwrap FAILS the whole spawn
    // on a missing source, which would turn "no private dir yet" into "the worker cannot start".
    for d in denied {
        if d.exists() {
            cmd.arg("--ro-bind").arg(d).arg(d);
        }
    }
    cmd.arg("--tmpfs").arg("/tmp")
        .arg("--proc").arg("/proc")
        .arg("--dev").arg("/dev")
        .arg("--share-net")
        .arg("--chdir").arg(cwd)
        .arg("--");
    cmd.arg(prog);
    for a in args {
        cmd.arg(a);
    }
    Ok(cmd)
}
