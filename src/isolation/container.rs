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
//! * `--network host` — full internet, unless the run has an egress allow-list: then an
//!   `--internal` network whose only way out is agg's proxy ([`super::egress::bridge`]).
//! * mounts are CANONICALIZED first: a symlinked source silently mis-mounts, the same defect class
//!   that silently denied every write under Seatbelt (§11 BUG 1). A source that is still relative
//!   after that is a LOUD error — `docker` requires an absolute source, and a container that fails
//!   to start is not confinement, it is a broken session.
//! * the mount target equals the host path, so every path in a log / in the agent's own output
//!   still means what the operator thinks it means.
//!
//! # The agent runtime
//! [`containerize`] confines a command against any image. Running the AGENT CLI in there — the
//! image contract, a fresh `HOME`, credentials by name or read-only mount, the project owner's uid,
//! judges on a read-only project — is [`containerize_spec`] driven by [`super::runtime`], and the
//! startup check that each image has its CLI is [`super::probe`].

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

/// The container engine `isolation: container` would use (`docker`, else `podman`), if one runs.
/// Reported by `agg doctor --json`.
pub fn container_engine() -> Option<&'static str> {
    engine()
}

/// Reshape a built [`Command`] into `<engine> run …` that runs it inside `image`, with `cwd`
/// bind-mounted read-write and set as the workdir, plus a mount per `writable` path and a `:ro`
/// re-mount per denied one.
///
/// `denies` is THIS STEP's `readonly` minus `writable`, and it is an ADDITION to the derived
/// `agg/private/` carve-out, never a replacement for it — see [`super::deny_set`] and BUILD.md §3.8.
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
pub fn containerize(
    cmd: Command,
    cwd: &Path,
    writable: &[PathBuf],
    denies: &[String],
    image: &str,
) -> Result<Command> {
    containerize_spec(cmd, cwd, &Spec { image, writable, denies, ..Spec::default() })
}

/// One container agg starts: the image, what it may write, and what crosses into it. The plain
/// [`containerize`] is the `Spec` with only the first three set; the agent runtime adds the rest.
#[derive(Debug, Default)]
pub struct Spec<'a> {
    pub image: &'a str,
    /// extra host paths mounted read-write at the same path.
    pub writable: &'a [PathBuf],
    /// extra host paths mounted READ-ONLY at the same path (a library judge's directory).
    pub readonly: &'a [PathBuf],
    /// this step's `readonly` minus `writable` — added to the derived `agg/private/` carve-out.
    pub denies: &'a [String],
    /// mount the project READ-ONLY (a judge grades the tree, it never changes it).
    pub project_ro: bool,
    /// the container's `HOME`: a host dir mounted read-write at the same path. The agent's own
    /// state lives here, not in the host's `~` — which is not in the container at all.
    pub home: Option<&'a Path>,
    /// env var NAMES passed by name when set in agg's environment (an agent's credentials). Added
    /// to the run's `isolation_policy.pass_env`.
    pub env_names: &'a [&'a str],
    /// `~`-relative host files mounted read-only at the same place under `home` when they exist (an
    /// agent's credential file). Added to the run's `isolation_policy.credentials`.
    pub credential_files: &'a [&'a str],
}

/// [`containerize`] for a full [`Spec`].
///
/// Beyond the plain recipe, every container runs as the OWNER of the project dir (`--user
/// uid:gid`): files it writes into the bind mount belong to the operator rather than to root (the
/// Linux ownership problem), and an agent CLI that refuses to run with auto-approval as root
/// (Claude Code) runs. The image must therefore work for an arbitrary non-root uid — the image
/// contract (`docs/CONFIG.md`).
///
/// Credentials cross by NAME: `-e NAME` makes the engine client read the value from ITS
/// environment, which is agg's — so the secret is never on a command line (`ps`), and agg never
/// prints the argv. Files cross as read-only mounts. Neither is ever copied or baked into an image.
pub fn containerize_spec(cmd: Command, cwd: &Path, spec: &Spec) -> Result<Command> {
    let prog = cmd.get_program().to_owned();
    let args: Vec<OsString> = cmd.get_args().map(|a| a.to_owned()).collect();
    let envs: Vec<(OsString, Option<OsString>)> =
        cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();

    let cwd_resolved = canonical(cwd);
    let run = super::policy::for_dir(&cwd_resolved);
    let mut out = Command::new(engine().unwrap_or(ENGINES[0]));
    out.arg("run").arg("--rm");
    // The network: the host's (open), or — under an allow-list — an `--internal` network whose only
    // reachable address is the relay to agg's proxy ([`super::egress::bridge`]). NET_RAW goes too:
    // a raw socket has nowhere to go there anyway, and nothing an agent does needs one.
    let proxy_url = match &run.egress {
        None => {
            out.arg("--network").arg("host");
            None
        }
        Some(e) => {
            let (network, url) = e.container().map_err(anyhow::Error::msg)?;
            out.arg("--network").arg(network).arg("--cap-drop").arg("NET_RAW");
            Some(url)
        }
    };
    if let Some(owner) = owner(&cwd_resolved) {
        out.arg("--user").arg(owner);
    }
    mount(&mut out, &cwd_resolved, spec.project_ro)?;
    out.arg("-w").arg(&cwd_resolved);
    for w in spec.writable {
        mount(&mut out, &canonical(w), false)?;
    }
    for r in spec.readonly {
        mount(&mut out, &canonical(r), true)?;
    }
    // The DENIES: agg's private state and this step's `readonly` minus `writable` all sit inside the
    // mounted cwd, so re-mount each of them READ-ONLY on top. The engine applies the more specific
    // mount, and `:ro` is enforced by the kernel inside the container exactly as the Seatbelt deny /
    // bwrap `--ro-bind` are on the other tiers.
    //
    // The derived carve-out comes from the SAME [`super::deny_set`] the `sandbox` tier uses, so the
    // two tiers cannot drift about what `readonly:` means — or about `agg/private/` being in the set
    // whether or not the step listed anything.
    //
    // A path that does not exist is materialized first rather than skipped ([`super::materialize_deny`]):
    // skipping was a silent hole (the step could create the directory it named and then write it),
    // and creating it ourselves also avoids the root-owned stray dir `docker run` leaves behind when
    // it invents a missing bind source.
    for d in super::deny_set(&cwd_resolved, spec.denies) {
        if super::materialize_deny(&d) {
            mount(&mut out, &d, true)?;
        } else {
            eprintln!(
                "  ⚠ cannot deny writes to `{}` — it does not exist and could not be created, and \
                 the container tier denies by read-only bind mount, which needs a source",
                d.display()
            );
        }
    }
    // The run's context dirs, read-only at `/context/<as>` (derived from the project dir — see
    // [`super::policy`] — so every surface that containerizes gets them).
    for c in &run.contexts {
        mount_at(&mut out, &c.host, &c.container_path(), true)?;
    }
    if let Some(home) = spec.home {
        // mounted AFTER the read-only `agg/private/` it lives in: the engine mounts a deeper target
        // over a shallower one, so the home is writable while the rest of `private/` is not.
        mount(&mut out, home, false)?;
        out.arg("-e").arg(format!("HOME={}", home.display()));
        for (host, at) in credential_mounts(home, spec.credential_files, &run.credentials) {
            mount_at(&mut out, &host, &at, true)?;
        }
    }
    for (k, v) in envs {
        if let Some(v) = v {
            let mut kv = k;
            kv.push("=");
            kv.push(v);
            out.arg("-e").arg(kv);
        }
    }
    for name in pass_names(spec.env_names, &run.pass_env) {
        out.arg("-e").arg(name);
    }
    for (k, v) in run.context_env(super::Isolation::Container) {
        out.arg("-e").arg(format!("{k}={}", v.display()));
    }
    if let Some(url) = &proxy_url {
        for (k, v) in super::egress::proxy_env(url) {
            out.arg("-e").arg(format!("{k}={v}"));
        }
    }
    // the image, then the ORIGINAL command — nothing may come between them or after them.
    out.arg(spec.image).arg(prog);
    for a in args {
        out.arg(a);
    }
    out.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(out)
}

/// `uid:gid` of the project dir's owner — who the container runs as. `None` off unix.
pub(crate) fn owner(dir: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).ok().map(|m| format!("{}:{}", m.uid(), m.gid()))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// The env var names to pass by name: the spec's and the run's, deduplicated, and only those SET in
/// agg's environment — `-e NAME` for an unset one would hand the container an empty value that an
/// agent may read as "authenticated with nothing".
fn pass_names(spec: &[&str], run: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in spec.iter().map(|s| s.to_string()).chain(run.iter().cloned()) {
        if std::env::var_os(&n).is_some() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// Where each credential goes: an agent's `~`-relative files that exist on the host, and the run's
/// `isolation_policy.credentials` — under `home` at their `~`-relative path when they live under
/// the host's `~`, else at their own absolute path.
fn credential_mounts(home: &Path, agent_files: &[&str], run: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let host_home = std::env::var_os("HOME").map(PathBuf::from).map(|h| canonical(&h));
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    if let Some(hh) = &host_home {
        for rel in agent_files {
            let host = hh.join(rel);
            if host.exists() {
                out.push((canonical(&host), home.join(rel)));
            }
        }
    }
    for p in run {
        let at = match host_home.as_ref().and_then(|hh| p.strip_prefix(hh).ok()) {
            Some(rel) => home.join(rel),
            None => p.clone(),
        };
        if !out.iter().any(|(_, a)| a == &at) {
            out.push((p.clone(), at));
        }
    }
    out
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

/// Add one `-v <p>:<p>[:ro]` bind mount, host path == container path. `ro` is what turns a mount
/// into a DENY: the same path, re-mounted read-only on top of the writable cwd.
///
/// Both refusals are LOUD because both fail silently otherwise: a relative source makes the engine
/// reject the run outright, and a `:` in the path makes it split the spec in the wrong place and
/// mount something else entirely — a "confined" session writing to a path nobody meant.
fn mount(cmd: &mut Command, p: &Path, ro: bool) -> Result<()> {
    mount_at(cmd, p, p, ro)
}

/// [`mount`] with a container path of its own — for a host directory that appears somewhere fixed
/// inside (a context at `/context/<as>`). Same two refusals, on BOTH sides of the `:`.
fn mount_at(cmd: &mut Command, p: &Path, at: &Path, ro: bool) -> Result<()> {
    for side in [p, at] {
        check_mountable(side)?;
    }
    let mut spec = p.as_os_str().to_owned();
    spec.push(OsStr::new(":"));
    spec.push(at.as_os_str());
    if ro {
        spec.push(OsStr::new(":ro"));
    }
    cmd.arg("-v").arg(spec);
    Ok(())
}

/// A path the engine's `-v host:container` spec can carry: absolute, and with no `:` in it.
fn check_mountable(p: &Path) -> Result<()> {
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
        let cmd = containerize(inner, &cwd, &[], &[], "alpine:3.20").expect("build the run");
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
        let cmd =
            containerize(Command::new("true"), &cwd, std::slice::from_ref(&extra), &[], DEFAULT_IMAGE)
                .expect("build the run");
        let want = format!("{p}:{p}", p = canonical(&extra).display());
        assert!(argv(&cmd).contains(&want), "writable path mounted: {:?}", argv(&cmd));
    }

    /// A step's `readonly` minus `writable` reaches the engine as a `:ro` re-mount — and the DERIVED
    /// `agg/private/` carve-out is still there beside it. The second argument never replaces the
    /// first (BUILD.md §3.8), which is the assertion that keeps the moat from being one edit away
    /// from a step that lists nothing.
    #[test]
    fn containerize_denies_the_per_step_paths_and_still_carves_out_private() {
        let cwd = std::env::temp_dir().join(format!("agg-container-deny-{}", std::process::id()));
        std::fs::create_dir_all(cwd.join("agg/judges")).unwrap();
        let cmd = containerize(Command::new("true"), &cwd, &[], &["agg/judges".to_string()], DEFAULT_IMAGE)
            .expect("build the run");
        let args = argv(&cmd);
        let ro = |p: PathBuf| format!("{p}:{p}:ro", p = canonical(&p).display());
        assert!(args.contains(&ro(cwd.join("agg/judges"))), "the step's deny is mounted :ro: {args:?}");
        assert!(
            args.contains(&ro(crate::paths::private_dir(&canonical(&cwd)))),
            "the DERIVED carve-out survives the per-step list: {args:?}"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    /// The platform divergence, closed on this tier too: a deny path that does not exist yet is
    /// MATERIALIZED and mounted, not skipped. Skipping let a step create the very directory its
    /// `readonly:` list named and then write it — while the same config bound on macOS, where
    /// Seatbelt denies by path string.
    #[test]
    fn containerize_denies_a_path_that_does_not_exist_yet() {
        let cwd = std::env::temp_dir().join(format!("agg-container-absent-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let absent = cwd.join("agg/judges");
        assert!(!absent.exists(), "the point of the test is that it is not there");
        let cmd = containerize(Command::new("true"), &cwd, &[], &["agg/judges".to_string()], DEFAULT_IMAGE)
            .expect("build the run");
        let want = format!("{p}:{p}:ro", p = canonical(&absent).display());
        assert!(argv(&cmd).contains(&want), "an absent deny still binds: {:?}", argv(&cmd));
        std::fs::remove_dir_all(&cwd).ok();
    }

    /// A container does not inherit our environment, so env the caller set must cross as `-e K=V`
    /// or it is silently lost — the same regression `wrap` carries env to prevent, one tier up.
    #[test]
    fn containerize_forwards_the_inner_command_env() {
        let mut inner = Command::new("true");
        inner.env("AGG_JUDGE", "answered").env_remove("SHOULD_BE_ABSENT");
        let cmd =
            containerize(inner, &std::env::temp_dir(), &[], &[], DEFAULT_IMAGE).expect("build the run");
        let args = argv(&cmd);
        assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == "AGG_JUDGE=answered"), "env forwarded: {args:?}");
        // a removal needs no flag: the container starts from the image's env, so it is absent already
        assert!(!args.iter().any(|a| a.contains("SHOULD_BE_ABSENT")), "no flag for a removal: {args:?}");
    }

    /// A mount source the engine would reject (or silently mis-resolve) is refused HERE, with a
    /// message that names the path — not left to a raw engine error mid-session.
    #[test]
    fn containerize_refuses_a_mount_source_it_cannot_express() {
        let err =
            containerize(Command::new("true"), Path::new("relative/not-a-real-dir"), &[], &[], DEFAULT_IMAGE)
                .unwrap_err()
                .to_string();
        assert!(err.contains("absolute"), "must say why: {err}");
        assert!(err.contains("relative/not-a-real-dir"), "must name the path: {err}");
    }

    /// The run's context dirs reach EVERY containerized process read-only at `/context/<as>`, with
    /// their env var — derived from the project dir, so the caller passes nothing.
    #[test]
    fn containerize_mounts_the_runs_context_dirs_read_only() {
        let proj = tempfile::tempdir().unwrap();
        let kb = tempfile::tempdir().unwrap();
        let host = canonical(kb.path());
        let _g = super::super::policy::register(
            proj.path(),
            super::super::policy::RunPolicy {
                contexts: vec![super::super::policy::ContextMount { name: "kb".into(), host: host.clone() }],
                ..Default::default()
            },
        );
        let args = argv(&containerize(Command::new("true"), proj.path(), &[], &[], DEFAULT_IMAGE).unwrap());
        assert!(args.contains(&format!("{}:/context/kb:ro", host.display())), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == "AGG_CONTEXT_KB=/context/kb"), "{args:?}");
    }

    #[test]
    fn container_available_returns_a_bool() {
        let _: bool = container_available();
    }
}
