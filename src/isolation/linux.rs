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
/// minus `denied` (agg's own private state plus this step's `readonly` set — see
/// [`super::deny_set`]), which lives inside cwd.
/// `jail.rules` are Seatbelt rules — macOS only; bwrap's protected paths arrive in `denied` instead.
pub fn build(jail: &super::Jail, prog: &OsStr, args: &[OsString]) -> Result<Command> {
    let (cwd, writable, denied) = (jail.cwd, jail.writable, jail.denied);
    // Seatbelt rules are macOS's channel; on Linux the protected paths arrive as `denied` binds.
    debug_assert!(jail.rules.is_empty(), "Seatbelt rules reached the bwrap wrapper");
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
    // The DENIES: agg's own `agg/private/` carve-out plus this step's `readonly` minus `writable`,
    // re-bound READ-ONLY on top of the writable cwd. bwrap applies binds in argv ORDER, so this must
    // come AFTER `--bind <cwd>` — reversing them lets the later writable bind win and silently
    // reopens the hole.
    //
    // `--ro-bind`, not `--tmpfs`: reads must keep working (a judge reads `verdicts.jsonl`, the
    // worker reads its brief).
    //
    // A path that does not exist is MATERIALIZED first rather than skipped. bwrap fails the whole
    // spawn on a missing bind source, so skipping was the only way to keep a fresh project bootable
    // — but it also meant a `readonly:` entry bound on macOS (where Seatbelt denies by path string)
    // and not on Linux, which is one config with two security policies. See
    // [`super::materialize_deny`] for what that costs.
    for d in denied {
        if super::materialize_deny(d) {
            cmd.arg("--ro-bind").arg(d).arg(d);
        } else {
            // Loud, and not fatal: everything outside the project is already read-only under
            // `--ro-bind / /`, so an unmakeable deny is almost always one that was never writable.
            eprintln!(
                "  ⚠ cannot deny writes to `{}` — it does not exist and could not be created, and \
                 bwrap denies by read-only bind, which needs a mount point",
                d.display()
            );
        }
    }
    cmd.arg("--tmpfs").arg("/tmp")
        .arg("--proc").arg("/proc")
        .arg("--dev").arg("/dev");
    // The run's context dirs, re-bound read-only AFTER the `/tmp` tmpfs: the host is already
    // read-only under `--ro-bind / /`, but a directory under `/tmp` would otherwise be hidden by the
    // tmpfs, and one under a writable bind (an agent's state dir) would be writable.
    for r in jail.readonly {
        cmd.arg("--ro-bind").arg(r).arg(r);
    }
    cmd.arg("--share-net")
        .arg("--chdir").arg(cwd)
        .arg("--");
    cmd.arg(prog);
    for a in args {
        cmd.arg(a);
    }
    Ok(cmd)
}
