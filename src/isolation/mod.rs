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
//! # agg ALWAYS confines — no agent is exempt
//! Under `sandbox` this module wraps EVERY agent, including Codex, which has a kernel sandbox of
//! its own (`sandbox_mode=workspace-write`, still emitted by its backend). Those are two nested
//! layers and agg owns the outer one: an agent's own sandbox is the agent's promise, and it also
//! has no delivery channel for agg's per-path denies. See `internal/BUILD.md` §2.4 and the
//! `an_agents_own_kernel_sandbox_nests_inside_aggs` spike below.
//! [`crate::backend::AgentBackend::self_sandboxes`] survives as a REPORTING flag only.
//! [`Isolation`] itself is a LEAF type: it knows nothing about config or backends; they import it.
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
    pub fn build(_cwd: &Path, _writable: &[PathBuf], _denied: &[PathBuf], _prog: &std::ffi::OsStr, _args: &[std::ffi::OsString]) -> Result<Command> {
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

impl Isolation {
    /// Confinement STRENGTH — `none` < `sandbox` < `container`.
    fn rank(self) -> u8 {
        match self {
            Isolation::None => 0,
            Isolation::Sandbox => 1,
            Isolation::Container => 2,
        }
    }

    /// The stronger of two tiers.
    ///
    /// A RUN's tier is the strongest any of its steps asked for — never the weakest, and never
    /// whichever step happened to run last. That is what lets a judge's confinement follow from its
    /// ROLE (§2.5) instead of from its caller: a judge fired after an `isolation: none` step must not
    /// be the one unconfined process in a run that sandboxes everywhere else, because a judge script
    /// is exactly what a confined worker rewrites in order to escape.
    pub fn strongest(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
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
///
/// # Two deny channels, and the second never replaces the first
/// `denies` is THIS STEP's `readonly` minus `writable` ([`crate::core::config::ResolvedStep::denied`]),
/// project-relative. It is handed to [`deny_set`] as an ADDITION to the carve-out [`denied`] derives
/// from `cwd` — a parameter is a thing a future call site can forget, and forgetting `agg/private/`
/// is the one hole this module exists to keep shut (BUILD.md §3.8).
pub fn wrap(cmd: Command, cwd: &Path, writable: &[PathBuf], denies: &[String]) -> Result<Command> {
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

    let mut wrapped =
        imp::build(&cwd_resolved, &writable, &deny_set(&cwd_resolved, denies), &prog, &args)?;
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

/// Normalise a per-step `readonly:`/`writable:` entry to ONE canonical spelling, or reject it.
///
/// # Why this exists at all
/// `writable` SUBTRACTS from what `readonly` accumulated, and the subtraction compares STRINGS.
/// Without this, `writable(["agg/judges"])` against `readonly(["agg/judges/"])` subtracts nothing
/// while looking to its author like it worked — a step that believes it may write the graders and
/// silently cannot, or (with the lists swapped) a deny the author thinks they lifted and did not.
/// One spelling in, one spelling stored, one comparison that means what it reads like.
///
/// The canonical form drops a trailing `/`, drops `.` and empty components (so `./src`, `src` and
/// `src//` are one path), and resolves `..` lexically (`agg/judges/../judges` ⇒ `agg/judges`).
/// A leading `/` survives — an absolute entry stays absolute.
///
/// # `None` = rejected
/// Two inputs have no canonical form: one that climbs ABOVE the project root (`../secrets`), and
/// one that names the root itself (`""`, `"."`, `"/"`). Both are dropped by [`normalize_paths`],
/// loudly. Dropping is the SAFE direction in both lists: a path outside the project is already
/// unwritable under any confining tier (the jail grants cwd + tmp + the agent's state dir and
/// nothing else), so a dropped `readonly` entry loses no protection and a dropped `writable` entry
/// only fails to lift a deny that was never in force.
pub fn normalize_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let absolute = raw.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in raw.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                // nothing left to pop ⇒ the entry escapes the project root.
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    Some(if absolute { format!("/{joined}") } else { joined })
}

/// [`normalize_path`] over a list, dropping what it rejects with a WARNING naming the entry.
///
/// A warning rather than a hard error because a driver's `.readonly([..])` is an infallible builder
/// method — making it return a `Result` would put a `?` on every line of every step definition for
/// a case that is a typo, and dropping is the safe direction (see [`normalize_path`]).
/// ponytail: the ceiling is that a warning can be scrolled past. Upgrade path when that bites: a
/// `try_readonly` returning `Result`, or a startup refusal collected in `capability::check`.
pub fn normalize_paths<I, S>(paths: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::new();
    for p in paths {
        let raw = p.as_ref();
        match normalize_path(raw) {
            // dedup: a repeated path is not two denies, and an accumulating template chain repeats.
            Some(n) if !out.contains(&n) => out.push(n),
            Some(_) => {}
            None => eprintln!(
                "  ⚠ ignoring the isolation path `{raw}` — it names the project root or climbs \
                 above it, which no per-step deny can express"
            ),
        }
    }
    out
}

/// The paths a step may NOT write: `readonly` minus `writable`.
///
/// The asymmetry is the point (BUILD.md §0.2 rule 5). `readonly` ACCUMULATES down a template chain
/// so a derived step cannot silently lose a protection its template set; `writable` SUBTRACTS so a
/// step that legitimately needs one of them re-grants exactly that one instead of re-listing the
/// ones it still wants.
///
/// Matching is EXACT, on the canonical spelling both lists arrive in (see [`normalize_path`]) —
/// `writable(["src/foo"])` does not carve a hole in `readonly(["src"])`. A subtree re-grant would
/// need the wrapper to express deny-then-allow ordering, which neither backend does today.
pub fn denied_paths(readonly: &[String], writable: &[String]) -> Vec<String> {
    readonly.iter().filter(|p| !writable.contains(p)).cloned().collect()
}

/// The write CARVE-OUT: paths that stay readable but are denied for WRITING, even though they sit
/// inside the otherwise-writable `cwd`. Today that is exactly `agg/private/` — see [`crate::paths`]
/// for what lives there and why.
///
/// DERIVED FROM `cwd` rather than passed in, deliberately. `wrap` confines three different spawn
/// surfaces (worker, script judge, `agg.yaml` hook) and every one of them is reachable by a worker
/// that edits files in its own cwd. A parameter is a thing a future call site can forget; deriving
/// it here means adding a fourth surface cannot silently reopen the hole.
///
/// Returned even when the directory does not exist yet (a fresh project, or a unit test whose cwd
/// is a bare temp dir): both backends match on the path STRING, so denying a not-yet-created
/// directory is correct and becomes effective the moment agg creates it.
fn denied(cwd: &Path) -> Vec<PathBuf> {
    let p = crate::paths::private_dir(cwd);
    // canonicalize when it resolves — macOS Seatbelt matches canonical paths ONLY, so an
    // unresolved `/tmp/...` deny would silently match nothing on a host where `/tmp` is a symlink
    // (ISOLATION.md §11 BUG 1, the same trap that once broke the ALLOW side).
    vec![std::fs::canonicalize(&p).unwrap_or(p)]
}

/// The COMPLETE write-deny set a confining tier is handed: the derived carve-out ([`denied`]) plus
/// this step's own `readonly` minus `writable`, resolved to absolute paths against `cwd`.
///
/// The derived half comes FIRST and is never conditional on the step's list — that ordering is the
/// invariant BUILD.md §3.8 pins down: the per-step list is a SECOND argument, never a replacement,
/// so a step that lists nothing still cannot write `agg/private/`.
///
/// Both tiers share this so `readonly:` means one thing across `sandbox` and `container`, and a
/// relative entry (`agg/judges`) resolves against the same cwd the wrapper already canonicalized.
pub(crate) fn deny_set(cwd: &Path, denies: &[String]) -> Vec<PathBuf> {
    let mut out = denied(cwd);
    for d in denies {
        // An absolute entry stays absolute (`normalize_path` preserves a leading `/`); everything
        // else is project-relative, which is what a `readonly:` list reads like.
        let p = if d.starts_with('/') { PathBuf::from(d) } else { cwd.join(d) };
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Make a deny path exist, because the two MOUNT-based backends cannot deny one that does not.
///
/// Seatbelt denies by path STRING, so `(deny file-write* (subpath "…/agg/judges"))` binds whether
/// or not the directory is there yet — and keeps binding the moment it appears. `bwrap` and a
/// container engine deny by READ-ONLY BIND, and a bind needs a mount point, so both used to SKIP a
/// path that did not exist. That is the same driver getting two different security policies with no
/// diagnostic: on Linux a step could CREATE the very directory its `readonly:` list named and then
/// write it, while on macOS it could not.
///
/// Creating the directory closes the gap. It is also what the engine would have created under the
/// mount point anyway — doing it ourselves just means it is owned by the user rather than by root.
///
/// ponytail: an entry naming a not-yet-existing FILE materialises as an empty DIRECTORY. The
/// ceiling is real but narrow (every deny list agg has ever written names directories); the upgrade
/// path when it bites is to carry the entry's kind on the list and `File::create` instead.
pub(crate) fn materialize_deny(p: &Path) -> bool {
    p.exists() || std::fs::create_dir_all(p).is_ok()
}

/// The warning for a step that lists `readonly:`/`writable:` paths but runs under
/// [`Isolation::None`] — BUILD.md §0.2 rule 4. `None` = there is nothing to warn about.
///
/// Returns the TEXT rather than printing it so the rule is unit-testable without capturing stderr;
/// the caller ([`crate::features::run`]) prints it.
///
/// # Why a warning and not a refusal
/// The lists are inert under `none` — the deny is delivered by the OS wrapper and no wrapper is
/// invoked — but a driver may legitimately keep them on a template it also instantiates unconfined.
/// Refusing would make the safe spelling (list the protections once, on the family) the one that
/// fails. What is NOT acceptable is silence, or the word "ignored": the operator has to read the
/// consequence, which is that the step **can write** every path it thought it had protected.
pub fn unenforced_denies(
    step: &str,
    tier: Isolation,
    readonly: &[String],
    writable: &[String],
) -> Option<String> {
    if tier != Isolation::None || (readonly.is_empty() && writable.is_empty()) {
        return None;
    }
    let paths = denied_paths(readonly, writable);
    let list =
        if paths.is_empty() { "every path it lists".to_string() } else { paths.join(", ") };
    Some(format!(
        "  ⚠ step `{step}` lists readonly:/writable: paths but runs under `isolation: none` — no OS \
         wrapper is invoked, so the step CAN WRITE these paths: {list}. Set `isolation: sandbox` \
         (or `container`) to make the list bind."
    ))
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

    /// The trailing slash is the whole reason normalisation exists: `writable` subtracts by string.
    #[test]
    fn one_directory_has_exactly_one_spelling() {
        for spelling in ["agg/judges", "agg/judges/", "./agg/judges", "agg//judges//", " agg/judges "] {
            assert_eq!(normalize_path(spelling).as_deref(), Some("agg/judges"), "spelled `{spelling}`");
        }
        // `..` resolves lexically rather than surviving into the comparison
        assert_eq!(normalize_path("agg/judges/../judges").as_deref(), Some("agg/judges"));
        // an absolute entry stays absolute
        assert_eq!(normalize_path("/etc/").as_deref(), Some("/etc"));
    }

    /// Rejected: a path that climbs above the project root, and one that names the root itself.
    #[test]
    fn a_path_with_no_canonical_form_is_rejected() {
        for bad in ["..", "../secrets", "a/../..", "", ".", "/", "./"] {
            assert_eq!(normalize_path(bad), None, "`{bad}` must have no canonical form");
        }
        // …and the list form drops it rather than storing a lie
        assert_eq!(normalize_paths(["src/", "../escape", "src"]), vec!["src".to_string()]);
    }

    /// `writable` subtracts from what `readonly` accumulated — and only ever exactly.
    #[test]
    fn writable_subtracts_from_readonly() {
        let ro = normalize_paths(["tests/", "agg/judges/", "src/"]);
        let w = normalize_paths(["agg/judges"]); // spelled WITHOUT the slash on purpose
        assert_eq!(denied_paths(&ro, &w), vec!["tests".to_string(), "src".to_string()]);
        // exact match only: a subtree does not carve a hole
        assert_eq!(denied_paths(&normalize_paths(["src/"]), &normalize_paths(["src/foo"])), vec!["src".to_string()]);
    }

    #[test]
    fn available_returns_a_bool() {
        // just exercise the probe — the value depends on the host, but it must not panic.
        let _: bool = available();
    }

    /// ⛔ THE MOAT. The per-step list is a SECOND argument (BUILD.md §3.8): `agg/private/` is in the
    /// deny set whether the step listed anything or not, and it stays FIRST so no future edit can
    /// make it conditional on the step's own list.
    #[test]
    fn the_per_step_denies_are_added_to_the_derived_carve_out_never_instead_of_it() {
        let cwd = Path::new("/repo/proj");
        let private = crate::paths::private_dir(cwd);

        assert_eq!(deny_set(cwd, &[]), vec![private.clone()], "no list ⇒ still the carve-out");

        let with = deny_set(cwd, &["agg/judges".to_string(), "src".to_string()]);
        assert_eq!(with[0], private, "the derived carve-out comes first and is never dropped");
        assert_eq!(with[1..], [cwd.join("agg/judges"), cwd.join("src")], "relative ⇒ under cwd");

        // an absolute entry stays absolute (`normalize_path` preserves the leading `/`) — and is
        // CANONICALIZED when it resolves, because Seatbelt matches canonical paths only and `/etc`
        // is a symlink to `/private/etc` on every macOS host (ISOLATION.md §11 BUG 1).
        let abs = PathBuf::from("/etc/ssh");
        let want = std::fs::canonicalize(&abs).unwrap_or(abs.clone());
        assert!(deny_set(cwd, &[abs.to_string_lossy().into_owned()]).contains(&want));
        // …and a step that names the private dir itself does not make it two denies
        let dup = deny_set(cwd, &[private.strip_prefix(cwd).unwrap().to_string_lossy().into_owned()]);
        assert_eq!(dup.len(), 1, "the same path twice is one deny: {dup:?}");
    }

    /// BUILD.md §0.2 rule 4: a list on an UNCONFINED step is inert, and agg says so in the words
    /// that describe the consequence. "Ignored" would read like agg made a choice; the operator
    /// needs to read that the step can write the paths it thought it had protected.
    #[test]
    fn a_deny_list_without_a_tier_warns_that_the_step_can_write_the_paths() {
        let ro = normalize_paths(["tests/", "agg/judges/"]);
        let w = normalize_paths(["tests"]);

        let msg = unenforced_denies("implement", Isolation::None, &ro, &w)
            .expect("a list under `none` must warn");
        assert!(msg.contains("implement"), "names the step: {msg}");
        assert!(msg.contains("CAN WRITE"), "states the consequence: {msg}");
        assert!(!msg.contains("ignor"), "never `ignored` — agg did not choose this: {msg}");
        assert!(msg.contains("agg/judges"), "names the path that is not protected: {msg}");
        assert!(msg.contains("sandbox"), "names the fix: {msg}");

        // …and it is a WARNING, not a refusal: nothing here returns an Err. Under a confining tier
        // there is nothing to say, and an empty list on an unconfined step is not worth a line.
        assert_eq!(unenforced_denies("implement", Isolation::Sandbox, &ro, &w), None);
        assert_eq!(unenforced_denies("implement", Isolation::Container, &ro, &w), None);
        assert_eq!(unenforced_denies("implement", Isolation::None, &[], &[]), None);
        // a `writable:`-only step still warns — the list is just as inert, and saying nothing is
        // how the author concludes it bound.
        assert!(unenforced_denies("x", Isolation::None, &[], &w).is_some());
    }

    /// On a supported OS, `wrap` preserves the original program + args (after the wrapper prefix)
    /// and sets the wrapper as argv[0]. On an unsupported OS it must return a loud Err.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn wrap_preserves_the_inner_program_and_args() {
        let mut inner = Command::new("claude");
        inner.arg("-p").arg("do the thing");
        let cwd = std::env::temp_dir();
        let wrapped = wrap(inner, &cwd, &[], &[]).expect("supported OS builds a wrapper");
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
        let wrapped = wrap(inner, &cwd, &[], &[]).expect("supported OS builds a wrapper");
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
        let out = wrap(inner, &jail, &[], &[])
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

    /// THE CARVE-OUT, proven by the kernel rather than by reading the profile text. This is the
    /// test the `verdicts.jsonl` forgery has to fail against: a confined worker may still write its
    /// own state (`agg/state/`) and the project source, but NOT `agg/private/` — and it may still
    /// READ the private files, because a judge reads the ledger and the worker reads its brief.
    ///
    /// `#[ignore]`d like its siblings: it spawns the real OS sandbox, which a nested sandbox (CI,
    /// Claude Code's own jail) refuses. Run on a real host:
    /// `cargo test -- --ignored private_dir_is_carved_out --nocapture`
    #[test]
    #[ignore = "spawns the real OS sandbox; run by hand on a real host"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn private_dir_is_carved_out_of_the_writable_cwd() {
        assert!(available(), "no OS sandbox on this host — cannot prove confinement");
        let proj = std::env::temp_dir().join(format!("agg-carve-{}", std::process::id()));
        let private = crate::paths::private_dir(&proj);
        let state = crate::paths::agg_dir(&proj);
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        // a pre-existing ledger the worker will try to forge a row into, and read back
        let ledger = crate::paths::verdicts_jsonl(&proj);
        std::fs::write(&ledger, "{\"judge\":\"real\"}\n").unwrap();

        let script = format!(
            "echo work > '{p}/source.txt' && echo SRC_OK; \
             echo advice > '{s}/STATE.md' && echo STATE_OK; \
             cat '{l}' >/dev/null && echo READ_OK; \
             echo forged >> '{l}' && echo FORGE_OK || echo FORGE_DENIED; \
             echo x > '{pr}/new.txt' && echo NEWFILE_OK || echo NEWFILE_DENIED",
            p = proj.display(), s = state.display(), l = ledger.display(), pr = private.display()
        );
        let mut inner = Command::new("/bin/sh");
        inner.arg("-c").arg(&script);
        let out = wrap(inner, &proj, &[], &[]).expect("build the wrapper").output().expect("run the sandbox");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let ledger_after = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&proj);

        // the worker's own world still works — a carve-out that breaks the loop is not a fix
        assert!(stdout.contains("SRC_OK"), "the worker must still write the project:\n{stdout}\n{stderr}");
        assert!(stdout.contains("STATE_OK"), "…and its own agg/state/:\n{stdout}\n{stderr}");
        assert!(stdout.contains("READ_OK"), "…and READ the ledger (a judge does):\n{stdout}\n{stderr}");
        // …and the hole is shut
        assert!(stdout.contains("FORGE_DENIED"), "the ledger forgery must be DENIED:\n{stdout}\n{stderr}");
        assert!(stdout.contains("NEWFILE_DENIED"), "no new files in private/ either:\n{stdout}\n{stderr}");
        assert!(!ledger_after.contains("forged"), "the ledger was MODIFIED — the carve-out leaked");
    }

    /// THE PER-STEP DENY LIST, proven by the kernel — the acceptance test for BUILD.md commit 9.
    ///
    /// Everything upstream of here (`Step::readonly`'s accumulation, `writable`'s subtraction, the
    /// trailing-slash normalisation) is tested against strings. This is the one test that asks the
    /// OS whether any of it actually bound, and it exercises the whole chain in one shot: a driver's
    /// template denies `tests/` and `agg/judges/`, a derived step adds `src/` and re-grants exactly
    /// `tests/`, and the kernel is asked which of the four the worker can write.
    ///
    /// The fourth assertion is the load-bearing one: `agg/private/` is still carved out. The
    /// per-step list is a SECOND argument to [`wrap`], and if a future edit ever makes it a
    /// replacement, everything else here still passes while the moat is gone.
    ///
    /// `#[ignore]`d like its siblings — it spawns the real OS sandbox, which CI and nested sandboxes
    /// refuse. Run on a real host:
    /// `cargo test --lib -- --ignored a_steps_readonly_list_reaches_the_kernel --nocapture`
    #[test]
    #[ignore = "spawns the real OS sandbox; run by hand on a real host"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_steps_readonly_list_reaches_the_kernel() {
        use crate::driver::Step;
        assert!(available(), "no OS sandbox on this host — cannot prove confinement");
        let proj = std::env::temp_dir().join(format!("agg-denylist-{}", std::process::id()));
        for d in ["tests", "src", "docs", "agg/judges"] {
            std::fs::create_dir_all(proj.join(d)).unwrap();
        }
        std::fs::create_dir_all(crate::paths::private_dir(&proj)).unwrap();
        let ledger = crate::paths::verdicts_jsonl(&proj);
        let pristine = "{\"judge\":\"real\"}\n";
        std::fs::write(&ledger, pristine).unwrap();

        // The driver's own chain, verbatim from the sample: the family protects, the member
        // re-grants the ONE thing it legitimately needs.
        let sandboxed =
            Step::template().isolation(Isolation::Sandbox).readonly(["tests/", "agg/judges/"]);
        let step = sandboxed.create("implement").readonly(["src/"]).writable(["tests"]);
        assert_eq!(
            step.denied(),
            ["agg/judges", "src"],
            "inherited from the template + its own, minus the one re-grant"
        );

        let script = format!(
            "echo t > '{p}/tests/new.txt' && echo TESTS_OK || echo TESTS_DENIED; \
             echo s > '{p}/src/new.txt' && echo SRC_OK || echo SRC_DENIED; \
             echo d > '{p}/docs/new.txt' && echo DOCS_OK || echo DOCS_DENIED; \
             echo j > '{p}/agg/judges/evil.sh' && echo JUDGES_OK || echo JUDGES_DENIED; \
             cat '{l}' >/dev/null && echo READ_OK; \
             echo forged >> '{l}' && echo FORGE_OK || echo FORGE_DENIED",
            p = proj.display(),
            l = ledger.display()
        );
        let mut inner = Command::new("/bin/sh");
        inner.arg("-c").arg(&script);
        let out = wrap(inner, &proj, &[], &step.denied())
            .expect("build the wrapper")
            .output()
            .expect("run the sandbox");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let ledger_after = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&proj);

        // `.writable()` re-grants EXACTLY that path — and nothing the step never listed is denied.
        assert!(text.contains("TESTS_OK"), "`writable([\"tests\"])` must re-grant it:\n{text}");
        assert!(text.contains("DOCS_OK"), "an unlisted path stays writable:\n{text}");
        // …the step's own `readonly` binds…
        assert!(text.contains("SRC_DENIED"), "the step's own `readonly([\"src/\"])`:\n{text}");
        // …and so does the one it INHERITED from the template it was created from.
        assert!(text.contains("JUDGES_DENIED"), "`agg/judges/` inherited from the template:\n{text}");
        // ⛔ and the DERIVED carve-out is still there beside the per-step list.
        assert!(text.contains("READ_OK"), "reads stay open — a judge reads the ledger:\n{text}");
        assert!(text.contains("FORGE_DENIED"), "`agg/private/` is still carved out:\n{text}");
        assert_eq!(ledger_after, pristine, "the ledger was MODIFIED — the carve-out leaked");
    }

    /// The most permissive kernel sandbox this platform can express, wrapped around `/bin/sh -c`.
    ///
    /// This stands in for an agent that confines ITSELF — Codex's `sandbox_mode=workspace-write`
    /// (Seatbelt on macOS, Landlock on Linux, the same primitives). Deliberately permissive: the
    /// spike below must attribute a denial to agg's OUTER jail, so the inner layer has to be one
    /// that would allow the write on its own. A real `codex` binary is not used — it would need an
    /// auth'd account and a live model call to prove a property of the kernel, not of the agent.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn self_sandboxing_agent_shaped(script: &str) -> Command {
        #[cfg(target_os = "macos")]
        {
            let mut c = Command::new("sandbox-exec");
            c.arg("-p").arg("(version 1)(allow default)").arg("--").arg("/bin/sh").arg("-c").arg(script);
            c
        }
        #[cfg(target_os = "linux")]
        {
            let mut c = Command::new("bwrap");
            c.arg("--dev-bind").arg("/").arg("/").arg("--share-net").arg("--").arg("/bin/sh").arg("-c").arg(script);
            c
        }
    }

    /// ⚡ **THE NESTING SPIKE** (`internal/BUILD.md` §2.4) — and it comes back **RED on macOS**.
    ///
    /// §2.4 makes agg wrap EVERY agent under `Isolation::Sandbox`, including one that then applies
    /// its own kernel sandbox (Codex). Nothing in agg had ever exercised two nested kernel jails,
    /// and the ruling assumed they compose. Measured on a real host, 2026-08-05, macOS 15 (Darwin
    /// 25.5): **they do not.**
    ///
    /// > `sandbox-exec: sandbox_apply: Operation not permitted`
    ///
    /// Seatbelt permits a second `sandbox_apply` only from a process whose current profile is
    /// **entirely unrestricted**. Probed one rule at a time: `(allow default)` nests fine;
    /// `(allow default)(deny nvram*)` — a deny on an operation nothing here touches — already
    /// fails, as does an inner profile that is strictly *more* restrictive than the outer. It is
    /// not a missing clause in agg's profile; no operation name grants it (`sandbox-create` /
    /// `system-sandbox` do not even parse), and adding `system-privilege`, `job-creation`,
    /// `process-info*`, `system*` changes nothing. Any real confinement disables nesting.
    ///
    /// **What that costs, measured with the real binary** (`codex exec` under agg's profile, with
    /// `-c sandbox_mode=workspace-write` as §2.4 ships it): the codex PROCESS survives — it streams
    /// JSON, reaches the network, writes `~/.codex` — but every shell tool call it makes dies, and
    /// it answers *"I couldn't create the file because the shell sandbox denied the operation."*
    /// A session that does zero work while reporting success is worse than one that fails to spawn.
    ///
    /// **What DOES deliver the ruling** (same run, inner layer dropped via
    /// `--dangerously-bypass-approvals-and-sandbox` — exactly what `Isolation::Container` already
    /// does): real work succeeds AND agg's carve-out binds. Codex's own shell reported
    /// `zsh:1: operation not permitted: agg/private/verdicts.jsonl` and the ledger stayed
    /// byte-identical. **One layer — agg's — is both sufficient and strictly better than Codex's**
    /// (`workspace-write` has no per-path deny list at all). The fix is therefore to treat
    /// `Sandbox` like `Container` in `codex/mod.rs`; that file is frozen by BUILD.md §2.4 item (b),
    /// so this commit does not make the change. Owner's call.
    ///
    /// Linux (`bwrap` inside `bwrap`) is UNVERIFIED — no Linux host was available. The mechanisms
    /// differ enough (mount namespaces, not a MAC policy) that it may well nest; do not assume the
    /// macOS answer carries.
    ///
    /// The test asserts what we WANT, so it stays red until the mechanism changes. The two
    /// assertions before it pass today and are the load-bearing ones: agg's jail ALONE delivers
    /// §2.4, and the control proves the denial is attributable to agg rather than to the inner
    /// layer. Run it: `cargo test --lib -- --ignored an_agents_own_kernel_sandbox_nests --nocapture`
    #[test]
    #[ignore = "KNOWN RED on macOS (nesting is refused by Seatbelt); spawns real OS sandboxes — run by hand"]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn an_agents_own_kernel_sandbox_nests_inside_aggs() {
        assert!(available(), "no OS sandbox on this host — cannot prove confinement");
        let proj = std::env::temp_dir().join(format!("agg-nest-{}", std::process::id()));
        std::fs::create_dir_all(crate::paths::private_dir(&proj)).unwrap();
        let ledger = crate::paths::verdicts_jsonl(&proj);
        let pristine = "{\"judge\":\"real\"}\n";

        let script = format!(
            "echo INNER_RAN; \
             echo work > '{p}/source.txt' && echo SRC_OK; \
             cat '{l}' >/dev/null && echo READ_OK; \
             echo forged >> '{l}' && echo FORGE_OK || echo FORGE_DENIED",
            p = proj.display(), l = ledger.display()
        );
        // Every run starts from the same ledger, so `ledger_after` is attributable to that run alone.
        let run = |mut c: Command| {
            std::fs::write(&ledger, pristine).unwrap();
            let out = c.output().expect("run the sandbox");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (text, std::fs::read_to_string(&ledger).unwrap_or_default())
        };

        // 1. THE SHAPE §2.4 SHIPS: agg's jail outside, the agent's own inside.
        let (nested, nested_ledger) =
            run(wrap(self_sandboxing_agent_shaped(&script), &proj, &[], &[]).expect("build the wrapper"));
        // 2. agg's jail ALONE — the same worker with no inner layer.
        let mut sh = Command::new("/bin/sh");
        sh.arg("-c").arg(&script);
        let (outer_only, outer_ledger) = run(wrap(sh, &proj, &[], &[]).expect("build the wrapper"));
        // 3. CONTROL: the inner layer alone, so a denial in (1)/(2) is attributable to agg's.
        let (control, _) = run(self_sandboxing_agent_shaped(&script));

        let _ = std::fs::remove_dir_all(&proj);
        eprintln!("--- nested:\n{nested}\n--- agg's jail alone:\n{outer_only}\n--- control:\n{control}");

        // agg's own layer delivers the ruling on its own. This is the half that PASSES.
        assert!(outer_only.contains("SRC_OK"), "real work must survive agg's jail:\n{outer_only}");
        assert!(outer_only.contains("READ_OK"), "reads stay open:\n{outer_only}");
        assert!(outer_only.contains("FORGE_DENIED"), "agg's carve-out must bind:\n{outer_only}");
        assert_eq!(outer_ledger, pristine, "the ledger was MODIFIED under agg's jail");
        assert!(
            control.contains("FORGE_OK"),
            "the CONTROL must be able to forge — an inner layer that denies on its own would prove \
             nothing about agg's:\n{control}"
        );

        // ⚡ THE SPIKE. RED on macOS: Seatbelt refuses `sandbox_apply` from a restricted process.
        assert!(
            nested.contains("INNER_RAN"),
            "NESTING DOES NOT COMPOSE — the agent's own kernel sandbox could not start inside \
             agg's, so a Codex step under `isolation: sandbox` can run no tool at all. §2.4 needs \
             the inner layer DROPPED (as `Isolation::Container` already does), not stacked; agg's \
             own jail is sufficient and strictly stronger. See this test's doc comment.\n{nested}"
        );
        assert!(nested.contains("SRC_OK"), "real work must survive BOTH layers:\n{nested}");
        assert!(nested.contains("FORGE_DENIED"), "agg's carve-out must survive the nesting:\n{nested}");
        assert_eq!(nested_ledger, pristine, "the ledger was MODIFIED through the nested sandbox");
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn wrap_is_loud_on_an_unsupported_os() {
        let inner = Command::new("claude");
        let err = wrap(inner, Path::new("/tmp"), &[], &[]).unwrap_err().to_string();
        assert!(err.contains("not supported"), "must refuse loudly: {err}");
    }
}
