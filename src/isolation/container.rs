//! Container tier (`isolation: container`) — confinement by RE-HOSTING the command.
//!
//! `sandbox` ([`super::linux`] / [`super::macos`]) jails a host process. This tier jails nothing on
//! the host: it runs the command INSIDE a container with the project dir bind-mounted, so the
//! container boundary IS the confinement. Same target policy — **write = cwd, the extra mounts,
//! and the container's own tmp; network = open** — reached a different way, and the READ side is
//! much tighter than `sandbox`, because the host filesystem simply is not in there.
//!
//! Recipe (`internal/ISOLATION.md` §4 the ladder, §15 this tier):
//! ```text
//! docker run --rm --network host -v <cwd>:<cwd> -w <cwd> [-v <w>:<w> …] [-e K=V …] <image> <prog> <args…>
//! ```
//! * `--rm` — no leaked containers, ever; a session is ephemeral by construction.
//! * `--network host` — full internet (the owner's constraint; there is no egress policy).
//! * mounts are CANONICALIZED first: a symlinked source silently mis-mounts, the same defect class
//!   that silently denied every write under Seatbelt (§11 BUG 1). A source that is still relative
//!   after that is a LOUD error — `docker` requires an absolute source, and a container that fails
//!   to start is not confinement, it is a broken session.
//! * the mount target equals the host path, so every path in a log / in the agent's own output
//!   still means what the operator thinks it means.
//!
//! # Scope (deliberate)
//! This confines the worker against a plain BASE image. Running the real agent CLI *inside* the
//! container with its auth (the "container problem", §2 constraint 3) needs an image with node +
//! the CLI + a mounted credential store, and is a documented follow-up — see `internal/ISOLATION.md`
//! §15, not this module.

use anyhow::{bail, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The base image an `isolation: container` step runs in when `image:` is not set. Small, present
/// on every engine's cache, and enough to prove the mechanism — it is NOT an agent image (above).
pub const DEFAULT_IMAGE: &str = "alpine:3.20";

/// The engines we speak, in preference order. Both take the same `run` argv for what we use.
const ENGINES: [&str; 2] = ["docker", "podman"];

/// Is a container engine (docker or podman) runnable on this host?
///
/// Used by [`crate::capability::check`] (refuse `isolation: container` up front when there is none)
/// and by `agg doctor`.
pub fn container_available() -> bool {
    engine().is_some()
}

/// Reshape a built [`Command`] into `<engine> run …` that runs it inside `image`, with `cwd`
/// bind-mounted read-write and set as the workdir, plus a mount per `writable` path.
///
/// The analogue of [`super::wrap`] for this tier, and it keeps that function's two contracts:
/// the inner program + args survive IN ORDER (after the image name), and the uniform stdio is
/// re-applied (stdin `/dev/null`, stdout/stderr piped) so the caller's `process_group(0)` lands on
/// the engine client — which is correct, since killing it tears down the attached run.
///
/// Env is forwarded as `-e K=V`, not by setting it on the engine process: a container does NOT
/// inherit the caller's environment, so anything the caller set (a script judge's `AGG_*` contract)
/// would otherwise be silently dropped. A `.env_remove()` needs no counterpart — the container
/// starts from the image's environment, so the variable is absent already.
///
/// Deliberately does NOT fail when no engine is installed: "there is no engine" is a STARTUP
/// refusal ([`crate::capability::check`]), not a per-session surprise, and building the argv must
/// stay possible on a host with no daemon so the argv-shape tests remain hermetic. A missing engine
/// binary then surfaces loudly at spawn.
pub fn containerize(cmd: Command, cwd: &Path, writable: &[PathBuf], image: &str) -> Result<Command> {
    let prog = cmd.get_program().to_owned();
    let args: Vec<OsString> = cmd.get_args().map(|a| a.to_owned()).collect();
    let envs: Vec<(OsString, Option<OsString>)> =
        cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();

    let cwd_resolved = canonical(cwd);
    let mut out = Command::new(engine().unwrap_or(ENGINES[0]));
    out.arg("run").arg("--rm").arg("--network").arg("host");
    mount(&mut out, &cwd_resolved)?;
    out.arg("-w").arg(&cwd_resolved);
    for w in writable {
        mount(&mut out, &canonical(w))?;
    }
    // The CARVE-OUT: agg's private state is inside the mounted cwd, so re-mount it READ-ONLY on top.
    // The engine applies the more specific mount, and `:ro` is enforced by the kernel inside the
    // container exactly as the Seatbelt deny / bwrap `--ro-bind` are on the other tiers.
    //
    // Only when it EXISTS: `docker run` creates a missing bind source as an empty root-owned
    // directory, which would leave a stray dir in a fresh project.
    let private = canonical(&crate::paths::private_dir(&cwd_resolved));
    if private.exists() {
        let mut spec = OsString::from(&private);
        spec.push(":");
        spec.push(&private);
        spec.push(":ro");
        out.arg("-v").arg(spec);
    }
    for (k, v) in envs {
        if let Some(v) = v {
            let mut kv = k;
            kv.push("=");
            kv.push(v);
            out.arg("-e").arg(kv);
        }
    }
    // the image, then the ORIGINAL command — nothing may come between them or after them.
    out.arg(image).arg(prog);
    for a in args {
        out.arg(a);
    }
    out.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(out)
}

/// The engine binary to use: `docker` if it runs, else `podman`. `None` = neither.
///
/// Probed ONCE per process: the answer cannot meaningfully change mid-run, and the probe is a
/// subprocess we would otherwise pay for on every session, judge, and doctor check.
fn engine() -> Option<&'static str> {
    static ENGINE: OnceLock<Option<&'static str>> = OnceLock::new();
    *ENGINE.get_or_init(|| ENGINES.iter().copied().find(|e| runnable(e)))
}

/// `<engine> version` — exits 0 only when the CLI is present AND the daemon answers.
///
/// `--version` would NOT do: it succeeds against a dead daemon, so a step would pass the startup
/// check and then fail every session — the silent-downgrade shape this ladder exists to avoid.
fn runnable(bin: &str) -> bool {
    Command::new(bin)
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve symlinks so a mount source means what it says; a path that does not exist yet stays as
/// it is (and then has to be absolute — see [`mount`]).
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Add one `-v <p>:<p>` bind mount, host path == container path.
///
/// Both refusals are LOUD because both fail silently otherwise: a relative source makes the engine
/// reject the run outright, and a `:` in the path makes it split the spec in the wrong place and
/// mount something else entirely — a "confined" session writing to a path nobody meant.
fn mount(cmd: &mut Command, p: &Path) -> Result<()> {
    if !p.is_absolute() {
        bail!(
            "`isolation: container` needs an absolute path to bind-mount, got `{}` — the container \
             engine rejects a relative mount source.",
            p.display()
        );
    }
    if p.as_os_str().as_encoded_bytes().contains(&b':') {
        bail!(
            "`isolation: container` cannot bind-mount `{}`: a `:` in the path splits the engine's \
             `-v host:container` spec, which would mount the WRONG directory.",
            p.display()
        );
    }
    let mut spec = p.as_os_str().to_owned();
    spec.push(OsStr::new(":"));
    spec.push(p.as_os_str());
    cmd.arg("-v").arg(spec);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// The argv shape: an ephemeral `run` with the network open, cwd both mounted and set as the
    /// workdir, and the inner command LAST — after the image, in order, untouched.
    #[test]
    fn containerize_builds_an_ephemeral_run_with_cwd_mounted_and_the_inner_command_last() {
        let mut inner = Command::new("claude");
        inner.arg("-p").arg("do the thing");
        let cwd = std::env::temp_dir();
        let cmd = containerize(inner, &cwd, &[], "alpine:3.20").expect("build the run");
        let args = argv(&cmd);

        assert_eq!(cmd.get_program().to_string_lossy(), engine().unwrap_or(ENGINES[0]));
        assert_eq!(args[0], "run");
        assert!(args.iter().any(|a| a == "--rm"), "ephemeral: {args:?}");
        assert!(args.windows(2).any(|w| w[0] == "--network" && w[1] == "host"), "net open: {args:?}");
        let canon = canonical(&cwd).to_string_lossy().into_owned();
        assert!(
            args.windows(2).any(|w| w[0] == "-v" && w[1] == format!("{canon}:{canon}")),
            "cwd bind-mounted host==container: {args:?}"
        );
        assert!(args.windows(2).any(|w| w[0] == "-w" && w[1] == canon), "cwd is the workdir: {args:?}");
        let img = args.iter().position(|a| a == "alpine:3.20").expect("image in argv");
        assert_eq!(&args[img + 1..], &["claude", "-p", "do the thing"], "inner command last, in order");
    }

    /// Each `writable` path gets its own mount — the agent's state dirs, exactly as `wrap` grants
    /// them under `sandbox`.
    #[test]
    fn containerize_mounts_every_writable_path() {
        let cwd = std::env::temp_dir();
        let extra = cwd.join("agg-container-writable-probe");
        let cmd = containerize(Command::new("true"), &cwd, std::slice::from_ref(&extra), DEFAULT_IMAGE)
            .expect("build the run");
        let want = format!("{p}:{p}", p = canonical(&extra).display());
        assert!(argv(&cmd).contains(&want), "writable path mounted: {:?}", argv(&cmd));
    }

    /// A container does not inherit our environment, so env the caller set must cross as `-e K=V`
    /// or it is silently lost — the same regression `wrap` carries env to prevent, one tier up.
    #[test]
    fn containerize_forwards_the_inner_command_env() {
        let mut inner = Command::new("true");
        inner.env("AGG_JUDGE", "answered").env_remove("SHOULD_BE_ABSENT");
        let cmd = containerize(inner, &std::env::temp_dir(), &[], DEFAULT_IMAGE).expect("build the run");
        let args = argv(&cmd);
        assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == "AGG_JUDGE=answered"), "env forwarded: {args:?}");
        // a removal needs no flag: the container starts from the image's env, so it is absent already
        assert!(!args.iter().any(|a| a.contains("SHOULD_BE_ABSENT")), "no flag for a removal: {args:?}");
    }

    /// A mount source the engine would reject (or silently mis-resolve) is refused HERE, with a
    /// message that names the path — not left to a raw engine error mid-session.
    #[test]
    fn containerize_refuses_a_mount_source_it_cannot_express() {
        let err = containerize(Command::new("true"), Path::new("relative/not-a-real-dir"), &[], DEFAULT_IMAGE)
            .unwrap_err()
            .to_string();
        assert!(err.contains("absolute"), "must say why: {err}");
        assert!(err.contains("relative/not-a-real-dir"), "must name the path: {err}");
    }

    #[test]
    fn container_available_returns_a_bool() {
        let _: bool = container_available();
    }
}
