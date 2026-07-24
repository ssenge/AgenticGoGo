//! macOS OS sandbox via `sandbox-exec` (Seatbelt).
//!
//! Recipe (`internal/ISOLATION.md` §6 / §10.5) — a `(deny default)` profile that reads everything,
//! writes only cwd + `$TMPDIR` + `/tmp` + the agent's state paths, and leaves the network open:
//! ```scheme
//! (version 1)(deny default)
//! (allow process*)
//! (allow signal (target same-sandbox))
//! (allow file-read*)
//! (allow file-write* (subpath "<cwd>") (subpath "<TMPDIR>") (subpath "/private/tmp") (subpath "<w>")…)
//! (allow file-write-data (literal "/dev/null") (subpath "/dev/fd"))
//! (allow network*)(allow mach*)(allow sysctl-read)(allow system-sched)
//! ```
//! Every clause here was proved necessary by removing it and watching real tooling break; the
//! non-obvious ones:
//! * `signal` is NOT implied by `process*` — they are sibling operations. Without it the agent
//!   cannot kill its own children, so every Bash-tool timeout leaks a live process tree. The
//!   `(target same-sandbox)` filter is what keeps that from also granting signals to host processes.
//! * `sysctl-read` is where it fails hardest: node aborts in its allocator (`sysconf: Operation not
//!   permitted`) before it prints anything.
//! * `mach*` reaches securityd, i.e. the login keychain, i.e. the agent's OAuth credentials.
//! * `system-sched` grants `setpriority`/scheduling — zero blast radius (it cannot touch the
//!   filesystem, creds, or escape), kept so a subprocess that reniced (many build tools do) does
//!   not error. Measured NOT to be needed by claude's own Bash tool, so it is robustness, not a
//!   hard requirement; drop it if the profile is ever minimized against blast radius it doesn't add.
//! * `iokit-open` was in the design sketch and is NOT here: two independent real-host ablations
//!   found nothing that needs it. Every clause dropped is blast radius dropped.
//!
//! Known platform limit, not a profile bug: a sandboxed process cannot exec setuid binaries at all,
//! so `/bin/ps` and `/usr/bin/top` are unavailable to a confined worker no matter what we allow.
//!
//! Honest caveat: `sandbox-exec` is technically DEPRECATED (it prints a notice) but still works on
//! current macOS and is battle-tested (Chrome, and Claude Code itself sandbox this way). The
//! strategy stays swappable so a future macOS drop can move to the Seatbelt API or fall back.

use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Is `sandbox-exec` present and runnable? (`-n` names a builtin profile; a bad one still proves
/// the binary exists and parses args — we only need "is the tool here".)
pub fn available() -> bool {
    // `sandbox-exec` with no args prints usage and exits non-zero, so probe via `which`-style
    // resolution instead: try to run it with `-p` on a trivial always-allow profile against `true`.
    Command::new("sandbox-exec")
        .arg("-p")
        .arg("(version 1)(allow default)")
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the `sandbox-exec -p '<profile>' -- prog args…` wrapper.
pub fn build(cwd: &Path, writable: &[PathBuf], prog: &OsStr, args: &[OsString]) -> Result<Command> {
    // `$TMPDIR` is always writable (falling back to `/tmp` if unset).
    let tmp = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let profile = profile(cwd, &tmp, writable)?;
    let mut cmd = Command::new("sandbox-exec");
    cmd.arg("-p").arg(profile).arg("--").arg(prog);
    for a in args {
        cmd.arg(a);
    }
    Ok(cmd)
}

/// The `/dev` writes an agent CLI actually needs (`internal/ISOLATION.md` §11 BUG 2) — measured on
/// a real host against node/python/ruby/git/npm/cargo and all three real agent CLIs, not guessed.
///
/// Two deliberate choices:
/// * **Not `(subpath "/dev")`.** That would also grant `/dev/ttys*` and `/dev/console` — i.e. write
///   access to the user's other live terminals, which with `TIOCSTI` is command injection into an
///   unconfined shell. A jail that hands back a shell is not a jail.
/// * **`(subpath "/dev/fd")`, not `(literal "/dev/stdout")`.** `/dev/stdout` and `/dev/stderr` are
///   symlinks to `/dev/fd/N`, and Seatbelt matches the CANONICAL path — so literals for them are
///   DEAD CLAUSES that look like a fix while allowing nothing. (Same defect class as BUG 1, one
///   level down. Do not "fix" this by canonicalizing them: the fd number is dynamic.) `/dev/null`
///   still needs its own literal — `/dev/fd` alone regresses it.
///
/// `file-write-data` rather than `file-write*`: writing to these nodes is all anything needs;
/// create/unlink/chmod under `/dev` is not.
const DEV_WRITES: &str = "(allow file-write-data (literal \"/dev/null\") (subpath \"/dev/fd\"))";

/// Seatbelt matches the CANONICAL path, so a subpath handed in through a symlink never matches and
/// the intended-writable dir is silently DENIED. That is not theoretical on macOS: `/tmp` is a
/// symlink to `/private/tmp` and `$TMPDIR` is `/var/folders/…` → `/private/var/folders/…`, so the
/// worker's own temp dir was being denied (`internal/ISOLATION.md` §11 BUG 1). Resolve every
/// subpath before it goes into the profile.
///
/// A path that cannot be resolved (it does not exist yet) is emitted verbatim rather than dropped:
/// granting a path that resolves to nothing is harmless, silently narrowing the writable set is not.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Assemble the Seatbelt profile: deny-default, read-all, write only the whitelisted subpaths (plus
/// the `/dev` nodes), network open. `tmp` (the resolved `$TMPDIR`) is always in the writable set.
/// Taken as a param — not read from env here — so tests need not mutate the process-wide `TMPDIR`
/// (which would race every other test that writes to a temp dir).
///
/// `/tmp` is writable too, and that is not laziness: a real `claude` worker's shell tool creates
/// `/private/tmp/claude-<uid>/…` for every command it runs and cannot execute ANYTHING without it,
/// and `/usr/bin/diff` hardcodes `/tmp/diff.XXXXXXXX` and ignores `$TMPDIR`. It costs no real blast
/// radius — `/tmp` is world-writable to begin with — and it is exactly what Codex's own
/// `workspace-write` policy grants.
fn profile(cwd: &Path, tmp: &Path, writable: &[PathBuf]) -> Result<String> {
    let mut subpaths = vec![canonical(cwd), canonical(tmp), canonical(Path::new("/tmp"))];
    subpaths.extend(writable.iter().map(|p| canonical(p)));

    // A writable path that resolves to `/` grants the entire filesystem — confinement would be
    // silently gone while still reporting `isolation: sandbox`. Loud, not silent (§7).
    if let Some(p) = subpaths.iter().find(|p| p.as_os_str() == "/") {
        anyhow::bail!(
            "refusing to sandbox: writable path {} resolves to `/`, which would grant the whole \
             filesystem and silently disable the confinement you asked for",
            p.display()
        );
    }

    let mut writes: Vec<String> = subpaths
        .iter()
        // An empty path is a HARD `sandbox-exec` parse failure ("empty subpath pattern", exit 65).
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("(subpath \"{}\")", escape(&p.to_string_lossy())))
        .collect();

    // A config FILE is rewritten ATOMICALLY — write `<file>.tmp.<pid>.<hash>`, then rename — and
    // `(subpath …)` is COMPONENT-aware, so it grants the file but not its temp sibling. Measured:
    // with only `(subpath "~/.claude.json")`, claude's own config write is `Operation not
    // permitted`. A prefix regex covers the file, its `.backup`, and every temp sibling in one
    // clause. Files only: a directory subpath already covers everything beneath it, and a prefix
    // regex on a directory would also match unrelated siblings sharing its name prefix.
    writes.extend(
        subpaths
            .iter()
            .filter(|p| p.is_file())
            .map(|p| format!("(regex #\"^{}\")", regex_escape(&p.to_string_lossy()))),
    );
    let writes = writes.join(" ");

    Ok(format!(
        "(version 1)(deny default)\
         (allow process*)\
         (allow signal (target same-sandbox))\
         (allow file-read*)\
         (allow file-write* {writes})\
         {DEV_WRITES}\
         (allow network*)(allow mach*)(allow sysctl-read)(allow system-sched)"
    ))
}

/// Escape a path for a double-quoted Seatbelt string literal: backslash and double-quote.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a path for use INSIDE a Seatbelt `(regex #"…")`, where it must be matched literally.
/// A `.` in an unescaped path is a regex wildcard, so `~/.claude.json` would also match
/// `~/Xclaude!json` — narrow it back down to the one path we mean.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if r#"\^$.|?*+()[]{}""#.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_confines_writes_to_cwd_and_tmp_and_state() {
        // tmp is passed explicitly — no global env mutation, so this can't race parallel tests.
        // These paths do not exist, which also pins the `canonical` fallback: an unresolvable path
        // is emitted VERBATIM, never dropped (dropping would silently narrow the writable set).
        let p = profile(Path::new("/repo/proj"), Path::new("/var/folders/xy"), &[PathBuf::from("/home/u/.claude")]).unwrap();
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow file-read*)"), "reads everything");
        assert!(p.contains("(subpath \"/repo/proj\")"), "cwd is writable");
        assert!(p.contains("(subpath \"/var/folders/xy\")"), "TMPDIR is writable");
        assert!(p.contains("(subpath \"/home/u/.claude\")"), "agent state dir is writable");
        assert!(p.contains("(subpath \"/private/tmp\")"), "/tmp is writable — claude's shell tool cannot run without it");
        assert!(p.contains("(allow network*)"), "network is open");
        // `process*` does NOT imply `signal`; without this the agent cannot kill its own children.
        assert!(p.contains("(allow signal (target same-sandbox))"), "must signal its own subtree: {p}");
        // …and only its own: unfiltered `signal` would let a confined worker kill host processes.
        assert!(!p.contains("(allow signal)"), "never grant unfiltered signal: {p}");
    }

    /// BUG 2, and the trap inside it. The positive half is that `/dev/null` is writable at all; the
    /// NEGATIVE half is the half that matters — `(literal "/dev/stdout")`/`"/dev/stderr"` are dead
    /// clauses (symlinks to `/dev/fd/N`, and Seatbelt matches canonically), and `(subpath "/dev")`
    /// would hand the worker the user's other terminals. Both are exactly what a well-meaning
    /// future edit reaches for, so both are pinned shut here.
    #[test]
    fn profile_grants_the_measured_dev_writes_and_nothing_wider() {
        let p = profile(Path::new("/repo/proj"), Path::new("/private/tmp/x"), &[]).unwrap();
        assert!(p.contains("(allow file-write-data (literal \"/dev/null\") (subpath \"/dev/fd\"))"), "{p}");
        assert!(!p.contains("(subpath \"/dev\")"), "all of /dev grants /dev/ttys* → terminal injection: {p}");
        assert!(!p.contains("/dev/stdout"), "dead clause — /dev/stdout is a symlink to /dev/fd/N: {p}");
        assert!(!p.contains("/dev/stderr"), "dead clause — /dev/stderr is a symlink to /dev/fd/N: {p}");
    }

    /// A writable path that resolves to `/` would grant the whole filesystem while agg still
    /// reported `isolation: sandbox` — the one failure mode this feature must never have. Refuse.
    #[test]
    fn profile_refuses_a_writable_path_that_resolves_to_root() {
        let err = profile(Path::new("/"), Path::new("/private/tmp/x"), &[]).unwrap_err().to_string();
        assert!(err.contains("resolves to `/`"), "must refuse loudly: {err}");
    }

    /// BUG 1 (`internal/ISOLATION.md` §11): Seatbelt matches the CANONICAL path, so a symlinked
    /// writable dir — which on macOS is the NORMAL case, `/tmp` → `/private/tmp` and `$TMPDIR` →
    /// `/private/var/folders/…` — must be resolved before it is emitted, or the kernel denies the
    /// very directory we meant to grant.
    #[test]
    fn profile_canonicalizes_symlinked_subpaths() {
        // `/tmp` really is a symlink to `/private/tmp` on every macOS host.
        let p = profile(Path::new("/tmp"), Path::new("/tmp"), &[]).unwrap();
        assert!(p.contains("(subpath \"/private/tmp\")"), "symlinked cwd/tmp resolved: {p}");
        assert!(!p.contains("(subpath \"/tmp\")"), "the unresolved path must not be what we emit: {p}");
    }

    /// An agent's config file is rewritten atomically (`<file>.tmp.<pid>.<hash>` then rename), and
    /// `(subpath …)` is component-aware so it does NOT cover that sibling — measured on a real host
    /// as `Operation not permitted` on claude's own `~/.claude.json`. Files get a prefix regex too;
    /// directories must NOT (a prefix regex there would match unrelated same-prefix siblings).
    #[test]
    fn a_writable_file_also_grants_its_atomic_write_temp_siblings() {
        let dir = std::env::temp_dir().join(format!("agg-iso-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".agent.json");
        std::fs::write(&file, "{}").unwrap();

        let p = profile(&dir, Path::new("/private/tmp/x"), &[file.clone(), dir.clone()]).unwrap();
        let canon = std::fs::canonicalize(&file).unwrap();
        let esc = regex_escape(&canon.to_string_lossy());
        assert!(p.contains(&format!("(regex #\"^{esc}\")")), "the FILE gets a prefix regex: {p}");
        let dir_canon = std::fs::canonicalize(&dir).unwrap();
        assert!(
            !p.contains(&format!("(regex #\"^{}\")", regex_escape(&dir_canon.to_string_lossy()))),
            "a DIRECTORY must not get one — it would match same-prefix siblings: {p}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn regex_escape_neutralizes_the_dots_that_would_widen_the_match() {
        assert_eq!(regex_escape("/Users/u/.claude.json"), r"/Users/u/\.claude\.json");
        assert_eq!(regex_escape("/a+b(c)"), r"/a\+b\(c\)");
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        assert_eq!(escape(r#"/a b/"x""#), r#"/a b/\"x\""#);
        assert_eq!(escape(r"/a\b"), r"/a\\b");
    }
}
