//! Blast-radius isolation — the OS sandbox wrapped around a worker process.
//!
//! This is a DIFFERENT axis from agg's git session isolation (see [`crate::git::session`] /
//! `SessionIsolation` in config): that one protects the repo HISTORY from bad work; THIS one
//! protects the HOST (fs / creds / other processes) from an errant worker running in auto mode.
//! They compose. See `internal/ISOLATION.md`.
//!
//! # Scope
//! `none` (direct subprocess, today's behaviour) and `sandbox` (kernel-enforced FS confinement)
//! only. `container` / `vm` / `remote` are designed in the doc but not built.
//!
//! # The policy
//! `sandbox` gives the worker: **write = cwd (+subfolders) + `$TMPDIR` + the agent's own state
//! dir; read = everything; network = open.** No egress policy — the owner wants full internet.
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
/// kernel-enforced FS confinement. Deserialized from the lowercase tier name in agg.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// Direct subprocess, no confinement — full host blast radius (today's behaviour).
    #[default]
    None,
    /// Kernel-enforced FS confinement: write = cwd + tmp + agent state; read = all; net = open.
    Sandbox,
}

/// Is the OS sandbox mechanism available on this host (the wrapper binary is present)?
///
/// Linux probes `bwrap`; macOS probes `sandbox-exec`; every other OS is `false`. Used by
/// [`crate::capability::check`] (refuse `sandbox` up front when unavailable) and `agg doctor`.
pub fn available() -> bool {
    imp::available()
}

/// Wrap a built worker [`Command`] in the OS sandbox, confining writes to `cwd` + `$TMPDIR` +
/// `writable` (the agent's own state dirs) while leaving reads and network open.
///
/// Rebuilds a fresh `Command::new(<wrapper>)` whose argv is `[wrapper flags…, <orig program>,
/// <orig args…>]`, sets the working directory to `cwd`, and RE-APPLIES the uniform worker stdio
/// (stdin `/dev/null`, stdout/stderr piped) — the caller's `process_group(0)` then lands on the
/// wrapper, which is correct. No backend sets env on its command, so env is not carried (verified).
///
/// Returns a LOUD `Err` on an unsupported OS rather than silently downgrading to a direct spawn.
pub fn wrap(cmd: Command, cwd: &Path, writable: &[PathBuf]) -> Result<Command> {
    // get_program / get_args / get_current_dir are queryable; that is what lets us reshape a
    // backend's Command without the backend knowing about sandboxing.
    let prog = cmd.get_program().to_owned();
    let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_owned()).collect();

    let mut wrapped = imp::build(cwd, writable, &prog, &args)?;
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

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn wrap_is_loud_on_an_unsupported_os() {
        let inner = Command::new("claude");
        let err = wrap(inner, Path::new("/tmp"), &[]).unwrap_err().to_string();
        assert!(err.contains("not supported"), "must refuse loudly: {err}");
    }
}
