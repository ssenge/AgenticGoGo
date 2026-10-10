//! `write_deny` end to end: the real `agg` binary, a FAKE agent that tries one way of changing a
//! protected path per run, on `isolation: none` and on this host's kernel sandbox.
//!
//! The project is built so that, WITHOUT protection, every attack would MERGE: the fake worker also
//! makes the honest change the one judge wants, so `done_if` is met after session 1. With
//! protection, each attack must end one of two ways, and never a third:
//!
//! * **blocked** — the kernel refused the write (the attack's own shell reports failure), nothing
//!   protected changed, and the honest work merges (exit 0);
//! * **caught** — the gate check saw the change, put the path back, recorded a
//!   `protected_path_violation` row, and rolled the session back (base untouched, exit 4).
//!
//! Under `none` every attack must be CAUGHT (there is no kernel layer). Under the macOS sandbox
//! every attack must be BLOCKED (Seatbelt denies them all, measured). Elsewhere, either is fine.
//! On EVERY tier the protected files are byte-identical afterwards, tracked or gitignored.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The fake agent: makes the honest change, then runs `$AGG_TEST_ATTACK` and logs whether the
/// attack's shell succeeded plus its stderr (so an `EPERM` is visible to the test).
const FAKE_CLAUDE: &str = r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
echo work > src/lib.txt
if [ -n "$AGG_TEST_ATTACK" ]; then
  if sh -c "$AGG_TEST_ATTACK" 2>attack.err; then echo ATTACK_OK > attack.log; else echo ATTACK_FAILED > attack.log; fi
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#;

/// The judge needs the honest change AND reads the protected oracle — so a met verdict also proves
/// a judge can still READ a write-denied path on this tier.
const JUDGE: &str = r#"#!/bin/sh
if grep -q work src/lib.txt && grep -q orig tests/oracles/x.json; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"worked, oracle readable"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
fi
"#;

/// A git project with a two-version tracked oracle (so `git checkout HEAD~1 --` has somewhere to
/// go), a gitignored oracle beside it, and `write_deny: ["tests/oracles/**"]` under `tier`.
fn project(tier: &str, top_extra: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "user.email", "t@t"]);
    git(d, &["config", "user.name", "t"]);
    write(d, "tests/oracles/x.json", "v1\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "oracle v1"]);

    write(d, "tests/oracles/x.json", "orig\n");
    write(d, "tests/oracles/cache.gen", "generated\n");
    write(d, "src/lib.txt", "base\n");
    write(d, ".gitignore", "agg/state/\nagg/private/\nbin/\n.home/\n*.gen\nattack.log\nattack.err\n");
    write(
        d,
        "agg/agg.yaml",
        &format!(
            "project: \"protect\"\n\
             defaults: {{ model: \"fake\", isolation: \"{tier}\", write_deny: [\"tests/oracles/**\"] }}\n\
             steps:\n  worker: {{}}\n\
             sequence:\n  steps: [\"worker\"]\n  done_if: \"worked\"\n\
             summary: {{ enabled: false }}\n{top_extra}"
        ),
    );
    write(d, "agg/judges/worked.sh", JUDGE);
    chmod_x(&d.join("agg/judges/worked.sh"));
    write(d, "agg/state/STATE.md", "do work\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "oracle + config"]);

    write(d, "bin/claude", FAKE_CLAUDE);
    chmod_x(&d.join("bin/claude"));
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    (tmp, path)
}

/// `agg run --max-sessions <n>` with `attack` in the fake agent's environment.
fn run(dir: &Path, path: &str, attack: &str, sessions: &str) -> (Option<i32>, String) {
    let out = Command::cargo_bin("agg")
        .unwrap()
        .args(["run", "--max-sessions", sessions])
        .current_dir(dir)
        .env("PATH", path)
        .env("HOME", dir.join(".home"))
        .env("AGG_TEST_ATTACK", attack)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code(), text)
}

/// Can this process apply a REAL (restricting) kernel sandbox? A nested one (CI containers, an
/// agent's own jail) refuses, and then the sandbox half is skipped LOUDLY rather than failed.
fn kernel_sandbox_usable() -> bool {
    let probe = if cfg!(target_os = "macos") {
        Command::new("sandbox-exec")
            .args(["-p", "(version 1)(deny default)(allow process*)(allow file-read*)(allow sysctl-read)(allow mach*)", "/usr/bin/true"])
            .output()
    } else {
        Command::new("bwrap").args(["--ro-bind", "/", "/", "--dev", "/dev", "--", "/bin/true"]).output()
    };
    probe.map(|o| o.status.success()).unwrap_or(false)
}

fn case_insensitive(dir: &Path) -> bool {
    agg::protect::case_insensitive(dir)
}

/// The attacks — the ten cases of the requirement plus the ones a determined agent reaches for
/// next (an ancestor moved away, the protected directory swapped out, a case variant).
const ATTACKS: &[(&str, &str)] = &[
    ("direct_write", "echo evil > tests/oracles/x.json"),
    ("delete_and_recreate", "rm tests/oracles/x.json && echo evil > tests/oracles/x.json"),
    ("rename_over", "echo evil > t.json && mv t.json tests/oracles/x.json"),
    ("symlink_then_write", "ln -s tests/oracles/x.json link && echo evil > link"),
    ("hardlink_then_write", "ln tests/oracles/x.json hl && echo evil > hl"),
    ("chmod", "chmod 777 tests/oracles/x.json"),
    ("git_checkout_old_version", "git checkout -q HEAD~1 -- tests/oracles/x.json"),
    ("child_process", "sh -c 'echo evil > tests/oracles/x.json' & wait $!"),
    ("new_untracked_file", "echo n > tests/oracles/new.json"),
    ("gitignored_file", "echo evil > tests/oracles/cache.gen"),
    ("case_variant", "echo evil > TESTS/Oracles/X.JSON"),
    ("ancestor_moved_away", "mv tests tests_moved"),
    ("directory_swapped", "mv tests/oracles o2 && mkdir tests/oracles && echo evil > tests/oracles/x.json"),
];

#[derive(Debug, PartialEq)]
enum End {
    Blocked,
    Caught,
}

/// Run one attack and assert the invariants every tier owes; return how it ended, and whether the
/// project sat on a case-insensitive volume.
fn attack(tier: &str, name: &str, cmd: &str) -> (End, bool) {
    let (tmp, path) = project(tier, "");
    let d = tmp.path();
    let ci = case_insensitive(d);
    let (code, text) = run(d, &path, cmd, "1");
    let ctx = format!("[{tier}/{name}] exit {code:?}\n{text}");

    // ⛔ the invariant: whatever happened, the protected files are exactly as they were.
    assert_eq!(fs::read_to_string(d.join("tests/oracles/x.json")).unwrap_or_default(), "orig\n", "{ctx}");
    assert_eq!(fs::read_to_string(d.join("tests/oracles/cache.gen")).unwrap_or_default(), "generated\n", "{ctx}");
    assert!(!d.join("tests/oracles/new.json").exists(), "{ctx}");
    assert_eq!(git(d, &["show", "main:tests/oracles/x.json"]), "orig\n", "{ctx}");

    let rows = agg::protect::read(d);
    let log = fs::read_to_string(d.join("attack.log")).unwrap_or_default();
    // no log ⇒ the attack never ran (the env did not reach the jailed agent), and "blocked" would
    // pass vacuously.
    assert!(log.contains("ATTACK_"), "the attack did not run: {ctx}");
    let attack_ok = log.contains("ATTACK_OK");
    if rows.is_empty() {
        // BLOCKED: the kernel refused, nothing protected changed, the honest work merged.
        assert!(!attack_ok, "the attack SUCCEEDED and nothing caught it: {ctx}");
        assert_eq!(code, Some(0), "{ctx}");
        assert_eq!(git(d, &["show", "main:src/lib.txt"]), "work\n", "honest work merges: {ctx}");
        (End::Blocked, ci)
    } else {
        // CAUGHT: recorded with the paths, rolled back, base untouched.
        let r = &rows[0];
        assert_eq!(r.reason, "protected_path_violation", "{ctx}");
        assert_eq!((r.session, r.window.as_str(), r.restored), (1, "session", true), "{ctx}");
        assert!(r.paths.iter().all(|p| p.starts_with("tests/oracles")), "{:?} {ctx}", r.paths);
        assert!(text.contains("protected_path_violation"), "said out loud: {ctx}");
        assert_eq!(code, Some(4), "rolled back ⇒ done_if never held ⇒ max-sessions: {ctx}");
        assert_eq!(git(d, &["show", "main:src/lib.txt"]), "base\n", "the session was rolled back: {ctx}");
        (End::Caught, ci)
    }
}

/// Every attack on `tier`, each in its own project, in parallel (they share nothing).
fn all_attacks(tier: &'static str) -> Vec<(&'static str, End, bool)> {
    std::thread::scope(|sc| {
        let hs: Vec<_> = ATTACKS
            .iter()
            .map(|(name, cmd)| sc.spawn(move || {
                let (end, ci) = attack(tier, name, cmd);
                (*name, end, ci)
            }))
            .collect();
        hs.into_iter().map(|h| h.join().expect("an attack assertion failed (see above)")).collect()
    })
}

/// `isolation: none`: no kernel layer, so the gate must CATCH every attack that changes anything.
#[test]
fn under_none_the_gate_catches_every_attack() {
    for (name, end, ci) in all_attacks("none") {
        // on a case-SENSITIVE volume `TESTS/…` is a different, unprotected path — not an attack.
        if name == "case_variant" && !ci {
            continue;
        }
        assert_eq!(end, End::Caught, "under none, `{name}` must be caught by the gate");
    }
}

/// The host's kernel sandbox: every attack is blocked or caught; on macOS, Seatbelt BLOCKS them all.
#[test]
fn under_the_sandbox_every_attack_is_blocked_or_caught() {
    if !kernel_sandbox_usable() {
        eprintln!("SKIPPED: no usable kernel sandbox here (nested sandbox or missing wrapper)");
        return;
    }
    for (name, end, _) in all_attacks("sandbox") {
        if cfg!(target_os = "macos") {
            assert_eq!(end, End::Blocked, "Seatbelt must deny `{name}` outright");
        }
    }
}

/// The acceptance case, spelled out: the write fails with EPERM under the sandbox…
#[test]
fn a_direct_write_gets_eperm_under_the_sandbox() {
    if !kernel_sandbox_usable() {
        eprintln!("SKIPPED: no usable kernel sandbox here");
        return;
    }
    let (tmp, path) = project("sandbox", "");
    let (code, text) = run(tmp.path(), &path, "echo evil > tests/oracles/x.json", "1");
    let err = fs::read_to_string(tmp.path().join("attack.err")).unwrap_or_default();
    assert!(
        err.contains("Operation not permitted") || err.contains("Read-only file system"),
        "expected EPERM/EROFS, got `{err}`\n{text}"
    );
    assert_eq!(code, Some(0), "blocked, so the honest work still merges:\n{text}");
}

/// …and with no attack at all, a protected project runs exactly as before — the judge READS the
/// protected oracle and the work merges, on both tiers.
#[test]
fn an_honest_session_is_untouched_and_judges_still_read_protected_paths() {
    let mut tiers = vec!["none"];
    if kernel_sandbox_usable() {
        tiers.push("sandbox");
    }
    for tier in tiers {
        let (tmp, path) = project(tier, "");
        let (code, text) = run(tmp.path(), &path, "", "1");
        assert_eq!(code, Some(0), "[{tier}]\n{text}");
        assert!(agg::protect::read(tmp.path()).is_empty(), "[{tier}] no violation:\n{text}");
        assert!(text.contains("worked, oracle readable"), "[{tier}] the judge read the oracle:\n{text}");
        if tier == "none" {
            assert!(text.contains("only the GATE CHECK applies"), "none warns at startup:\n{text}");
        }
    }
}

/// A change made AFTER the session's check — here by an `on_session_end` hook, standing in for a
/// process the worker left running — is caught when the NEXT session starts, and put back.
#[test]
fn a_change_between_sessions_is_caught_at_the_next_session() {
    let (tmp, path) = project("none", "hooks: { on_session_end: [\"echo late > tests/oracles/x.json\"] }\n");
    let d = tmp.path();
    // the honest change is made in session 1 and merges; keep going with a done_if it cannot meet
    // so session 2 starts and runs its check.
    let yaml = fs::read_to_string(d.join("agg/agg.yaml")).unwrap().replace("done_if: \"worked\"", "done_if: \"never\"");
    write(d, "agg/agg.yaml", &yaml);
    write(d, "agg/judges/never.sh", "#!/bin/sh\necho '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"never\"}'\n");
    chmod_x(&d.join("agg/judges/never.sh"));
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "never"]);

    let (_code, text) = run(d, &path, "", "2");
    let rows = agg::protect::read(d);
    let between: Vec<_> = rows.iter().filter(|r| r.window == "between_sessions").collect();
    assert!(!between.is_empty(), "the late write must be caught:\n{text}\n{rows:?}");
    assert_eq!(between[0].paths, ["tests/oracles/x.json"]);
    assert_eq!(between[0].session, 1, "attributed to the session before the gap");
    // the hook runs again after session 2, so the file is only guaranteed restored at session
    // start — which is when the next worker and its judges would read it.
    assert!(text.contains("BETWEEN sessions"), "{text}");
}

/// Refuse rather than degrade: a pattern the tier cannot enforce stops the run at startup.
#[test]
fn an_unenforceable_pattern_is_refused_at_startup() {
    let (tmp, path) = project("none", "");
    let d = tmp.path();
    let yaml = fs::read_to_string(d.join("agg/agg.yaml")).unwrap().replace("tests/oracles/**", "/etc/passwd");
    write(d, "agg/agg.yaml", &yaml);
    git(d, &["commit", "-qam", "bad pattern"]);
    let (code, text) = run(d, &path, "", "1");
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("absolute"), "names the problem:\n{text}");
}

// ---- the container tier: protected paths reach the engine as `:ro` mounts ----

/// The container tier gets the protected DIRECTORY as a read-only mount, beside the derived
/// `agg/private/` carve-out.
#[test]
fn the_container_tier_mounts_protected_paths_read_only() {
    let (tmp, _) = project("container", "");
    let d = tmp.path();
    let denies = agg::protect::bind_paths(d, &["tests/oracles/**".to_string()]);
    let cmd = agg::isolation::containerize(Command::new("true"), d, &[], &denies, "alpine:3.20").unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let canon = fs::canonicalize(d.join("tests/oracles")).unwrap();
    let want = format!("{p}:{p}:ro", p = canon.display());
    assert!(args.contains(&want), "{want} in {args:?}");
}

/// REAL ENGINE: the attacks run inside `alpine` against the `:ro` mount. The kernel blocks every
/// write to the mounted tree; what a mount cannot see (an ancestor renamed on the host side of a
/// bind, a case variant on a case-insensitive host volume) is left to the gate check, which the
/// `none` suite proves catches each of them.
///
/// `#[ignore]`d: it needs a running engine, the image, and a project dir the engine can mount (it
/// uses `target/`, under the repo, since a VM-backed engine often cannot see the host's tmp).
/// `cargo test --test protected_paths -- --ignored container --nocapture`
#[test]
#[ignore = "needs a running container engine with alpine:3.20; run by hand"]
fn real_container_denies_writes_to_a_protected_directory() {
    assert!(agg::isolation::container_available(), "no container engine");
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/agg-protect-ctr-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    write(&d, "tests/oracles/x.json", "orig\n");
    let denies = agg::protect::bind_paths(&d, &["tests/oracles/**".to_string()]);
    let script = "t() { if sh -c \"$2\" 2>/dev/null; then echo \"$1 OK\"; else echo \"$1 DENIED\"; fi; }; \
        t direct 'echo evil > tests/oracles/x.json'; \
        t delete 'rm tests/oracles/x.json'; \
        t chmod 'chmod 777 tests/oracles/x.json'; \
        t rename_over 'echo e > t && mv t tests/oracles/x.json'; \
        t new_file 'echo n > tests/oracles/new.json'; \
        t hardlink 'ln tests/oracles/x.json hl'; \
        t symlink 'ln -s tests/oracles/x.json l && echo evil > l'; \
        t outside 'echo ok > src.txt'";
    let mut inner = Command::new("/bin/sh");
    inner.arg("-c").arg(script);
    let out = agg::isolation::containerize(inner, &d, &[], &denies, "alpine:3.20").unwrap().output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let after = fs::read_to_string(d.join("tests/oracles/x.json")).unwrap_or_default();
    let _ = fs::remove_dir_all(&d);
    eprintln!("{text}{}", String::from_utf8_lossy(&out.stderr));
    for a in ["direct", "delete", "chmod", "rename_over", "new_file", "hardlink", "symlink"] {
        assert!(text.contains(&format!("{a} DENIED")), "`{a}` must be denied by the :ro mount:\n{text}");
    }
    assert!(text.contains("outside OK"), "the rest of the project stays writable:\n{text}");
    assert_eq!(after, "orig\n");
}
