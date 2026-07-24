//! Blast-radius isolation — the OS sandbox wrapped around a worker process.
//!
//! This is a DIFFERENT axis from agg's git session isolation (see [`crate::git::session`] /
//! `SessionIsolation` in config): that one protects the repo HISTORY from bad work; THIS one
//! protects the HOST (fs / creds / other processes) from an errant worker running in auto mode.
//! They compose. See `internal/ISOLATION.md`.
//!
//! # Scope
//! `none` (direct subprocess, today's behaviour), `sandbox` (kernel-enforced FS confinement) and
//! `container` (the command re-hosted inside a container — [`container`]). `vm` / `remote` are
//! designed in the doc but not built.
//!
//! # The policy
//! `sandbox` gives the worker: **write = cwd (+subfolders) + `$TMPDIR` + the agent's own state
//! dir; read = everything; network = open.** No egress policy — the owner wants full internet.
//! `container` targets the same policy from the other side: the only host paths that exist inside
//! the container are the ones bind-mounted, so read is confined too.
//!
//! # Agent-awareness lives at the seam, not here
//! Codex has its OWN kernel sandbox (`--sandbox workspace-write`), so it is confined by flags in
//! its backend and NEVER wrapped ([`crate::backend::AgentBackend::self_sandboxes`] is true for it).
//! Claude/Copilot have only permission layers, not a kernel jail, so they are wrapped by this
//! module. [`Isolation`] itself is a LEAF type: it knows nothing about config or backends; they
//! import it.
//!
//! # Platform
//! Linux → `bwrap` (bubblewrap, rootless via user namespaces). macOS → `sandbox-exec` (Seatbelt).
//! Any other OS → a LOUD error, never a silent downgrade to `none`.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;

// The container tier is NOT cfg-split: one `docker run` argv works on every host that has an
// engine (macOS reaches a Linux VM, which is the engine's problem, not ours).
pub mod container;
pub use container::{container_available, containerize, DEFAULT_IMAGE};

// Any OS that is neither Linux nor macOS has no wrapper. Both entry points fail LOUD — the whole
// point of this feature is that an unavailable mechanism is refused, never silently ignored.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;
    pub fn available() -> bool {
        false
    }
    pub fn build(_cwd: &Path, _writable: &[PathBuf], _prog: &std::ffi::OsStr, _args: &[std::ffi::OsString]) -> Result<Command> {
        anyhow::bail!(
            "`isolation: sandbox` is not supported on this operating system — only Linux (bubblewrap) \
             and macOS (sandbox-exec) have an OS wrapper. Use `isolation: none`, or run on a supported OS."
        )
    }
}

/// The per-step blast-radius isolation tier. `none` = direct subprocess (today); `sandbox` =
/// kernel-enforced FS confinement; `container` = the command runs inside a container.
/// Deserialized from the lowercase tier name in agg.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// Direct subprocess, no confinement — full host blast radius (today's behaviour).
    #[default]
    None,
    /// Kernel-enforced FS confinement: write = cwd + tmp + agent state; read = all; net = open.
    Sandbox,
    /// Re-hosted inside a container ([`container`]): write = the bind-mounted cwd (+ the agent's
    /// state dirs); read = only what is mounted; net = open. The strongest tier agg ships.
    Container,
}

/// Is the OS sandbox mechanism available on this host (the wrapper binary is present)?
///
/// Linux probes `bwrap`; macOS probes `sandbox-exec`; every other OS is `false`. Used by
/// [`crate::capability::check`] (refuse `sandbox` up front when unavailable) and `agg doctor`.
pub fn available() -> bool {
    imp::available()
}

/// Wrap a built [`Command`] in the OS sandbox, confining writes to `cwd` + `$TMPDIR` + `writable`
/// (the agent's own state dirs) while leaving reads and network open.
///
/// Used for the worker AND for a script judge — a confined worker can rewrite `agg/judges/*.sh`
/// (they live inside its writable cwd), so the judge must run in the same jail or it is a wide-open
/// escape (ISOLATION.md §12). A script judge passes `writable: &[]`: it may write cwd/tmp but must
/// not be able to write anywhere the worker couldn't.
///
/// Rebuilds a fresh `Command::new(<wrapper>)` whose argv is `[wrapper flags…, <orig program>,
/// <orig args…>]`, sets the working directory to `cwd`, carries the inner command's ENV, and
/// RE-APPLIES the uniform stdio (stdin `/dev/null`, stdout/stderr piped) — the caller's
/// `process_group(0)` then lands on the wrapper, which is correct.
///
/// Returns a LOUD `Err` on an unsupported OS rather than silently downgrading to a direct spawn.
pub fn wrap(cmd: Command, cwd: &Path, writable: &[PathBuf]) -> Result<Command> {
    // get_program / get_args / get_envs are queryable; that is what lets us reshape a caller's
    // Command without the caller knowing about sandboxing.
    let prog = cmd.get_program().to_owned();
    let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_owned()).collect();
    // Carry env EXPLICITLY. The worker backends set none, but a script judge sets `AGG_*` — and a
    // rebuilt Command starts from the parent env with no per-command overrides, so those would be
    // silently dropped (the judge would run without its contract). `get_envs()` yields the
    // overrides layered on the inherited env: `Some` = set, `None` = a `.env_remove()`.
    let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> =
        cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();

    // Resolve before handing paths to a wrapper. `cwd` arrives as the literal `"."` whenever the
    // user did not pass `--dir`, and may reach us through a symlink — on macOS that alone silently
    // denies every write the policy meant to ALLOW (ISOLATION.md §11 BUG 1), because Seatbelt
    // matches canonical paths only. Doing it here rather than per-platform means the Linux `bwrap`
    // binds get the same guarantee. The SPAWNED cwd below deliberately stays as the caller supplied
    // it, so paths in logs and in the agent's own output stay the ones the user recognises.
    let cwd_resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let writable: Vec<PathBuf> =
        writable.iter().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())).collect();

    let mut wrapped = imp::build(&cwd_resolved, &writable, &prog, &args)?;
    for (k, v) in envs {
        match v {
            Some(v) => wrapped.env(k, v),
            None => wrapped.env_remove(k),
        };
    }
    wrapped
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_the_tier_names_and_rejects_garbage() {
        assert_eq!(serde_yaml::from_str::<Isolation>("none").unwrap(), Isolation::None);
        assert_eq!(serde_yaml::from_str::<Isolation>("sandbox").unwrap(), Isolation::Sandbox);
        assert_eq!(serde_yaml::from_str::<Isolation>("container").unwrap(), Isolation::Container);
        assert!(serde_yaml::from_str::<Isolation>("nonsense").is_err());
        // capitalisation is not accepted — the tier name is lowercase.
        assert!(serde_yaml::from_str::<Isolation>("None").is_err());
    }

    #[test]
    fn the_default_is_none() {
        assert_eq!(Isolation::default(), Isolation::None);
    }

    #[test]
    fn available_returns_a_bool() {
        // just exercise the probe — the value depends on the host, but it must not panic.
        let _: bool = available();
    }

    /// On a supported OS, `wrap` preserves the original program + args (after the wrapper prefix)
    /// and sets the wrapper as argv[0]. On an unsupported OS it must return a loud Err.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn wrap_preserves_the_inner_program_and_args() {
        let mut inner = Command::new("claude");
        inner.arg("-p").arg("do the thing");
        let cwd = std::env::temp_dir();
        let wrapped = wrap(inner, &cwd, &[]).expect("supported OS builds a wrapper");
        let prog = wrapped.get_program().to_string_lossy().into_owned();
        let args: Vec<String> = wrapped.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        // wrapper program is bwrap / sandbox-exec, never the agent
        assert!(prog.ends_with("bwrap") || prog.ends_with("sandbox-exec"), "wrapper prog: {prog}");
        // both wrappers put the inner command after a `--` separator; the inner program + its args
        // survive in order there. (Key off `--`, not `-p`: sandbox-exec itself uses `-p` for its
        // profile flag, which would collide with the agent's own `-p`.)
        let sep = args.iter().position(|a| a == "--").expect("wrapper uses a -- separator");
        let inner: Vec<&String> = args[sep + 1..].iter().collect();
        assert_eq!(inner, vec!["claude", "-p", "do the thing"], "inner program + args survive in order after --");
    }

    /// The wrapper MUST carry the inner command's env, or a wrapped script judge loses its `AGG_*`
    /// contract (the exact regression this guards). A rebuilt Command otherwise keeps only the
    /// inherited parent env with none of the per-command overrides.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn wrap_carries_the_inner_command_env() {
        let mut inner = Command::new("/bin/sh");
        inner.arg("-c").arg("true").env("AGG_JUDGE", "answered").env("AGG_SESSION", "3");
        let cwd = std::env::temp_dir();
        let wrapped = wrap(inner, &cwd, &[]).expect("supported OS builds a wrapper");
        let got: std::collections::HashMap<String, String> = wrapped
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned())))
            .collect();
        assert_eq!(got.get("AGG_JUDGE").map(String::as_str), Some("answered"), "judge env must survive the wrap");
        assert_eq!(got.get("AGG_SESSION").map(String::as_str), Some("3"));
    }

    /// REAL kernel confinement, on a real host — the only test that proves the feature rather than
    /// the wiring. Everything else here (and the e2e's fake wrapper) asserts argv and profile TEXT;
    /// this one actually runs `sandbox-exec`/`bwrap` and checks what the kernel permits:
    /// a write INSIDE the jail succeeds, `/dev/null` succeeds, and a write OUTSIDE is DENIED.
    ///
    /// `#[ignore]`d on purpose — it needs the real OS mechanism, which CI containers and *nested*
    /// sandboxes refuse (Claude Code's own Seatbelt jail makes a nested `sandbox_apply` fail with
    /// "Operation not permitted"). Run it by hand on a real host:
    ///
    /// ```text
    /// cargo test -- --ignored real_sandbox_confines_writes --nocapture
    /// ```
    #[test]
    #[ignore = "spawns the real OS sandbox; run by hand on a real host (see the doc comment)"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn real_sandbox_confines_writes() {
        assert!(available(), "no OS sandbox mechanism on this host — cannot prove confinement");
        let jail = std::env::temp_dir().join(format!("agg-iso-real-{}", std::process::id()));
        std::fs::create_dir_all(&jail).expect("create the jail dir");
        // "Outside" must be outside the jail AND outside $TMPDIR (which the policy grants). The
        // crate dir is neither — and if confinement is broken we notice a stray file and clean it.
        let outside = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/agg-isolation-escape-probe.txt");
        let _ = std::fs::remove_file(&outside);

        let script = format!(
            "echo in > '{j}/inside.txt' && echo INSIDE_OK; \
             echo dev > /dev/null && echo DEVNULL_OK; \
             echo out > '{o}' && echo ESCAPE_OK || echo ESCAPE_DENIED",
            j = jail.display(),
            o = outside.display()
        );
        let mut inner = Command::new("/bin/sh");
        inner.arg("-c").arg(&script);
        let out = wrap(inner, &jail, &[])
            .expect("build the wrapper")
            .output()
            .expect("run the real OS sandbox");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let escaped = outside.exists();
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&jail);

        assert!(stdout.contains("INSIDE_OK"), "a write INSIDE the jail must succeed\n{stdout}\n{stderr}");
        assert!(stdout.contains("DEVNULL_OK"), "/dev/null must be writable\n{stdout}\n{stderr}");
        assert!(stdout.contains("ESCAPE_DENIED"), "a write OUTSIDE the jail must be denied\n{stdout}\n{stderr}");
        assert!(!escaped, "the worker ESCAPED — it wrote {}", outside.display());
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn wrap_is_loud_on_an_unsupported_os() {
        let inner = Command::new("claude");
        let err = wrap(inner, Path::new("/tmp"), &[]).unwrap_err().to_string();
        assert!(err.contains("not supported"), "must refuse loudly: {err}");
    }
}
