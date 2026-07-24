//! ACCEPTANCE TEST for the `isolation: container` tier — this file IS the Definition of Done.
//!
//! It is written by the operator (agg), NOT the worker. The loop's judges require these tests to
//! PASS and to still EXIST. DO NOT weaken, skip, or delete an assertion — that is the one way to
//! "pass" that does not count. Implement the tier until these are green.
//!
//! Contract the worker must satisfy (public API on the `agg` lib crate):
//!   * `agg::isolation::Isolation::Container` — a new tier, deserialized from the name `container`.
//!   * `agg::isolation::containerize(cmd, cwd, writable, image) -> anyhow::Result<Command>` —
//!     reshape a built worker `Command` into a `docker run …` (or `podman run …`) that BIND-MOUNTS
//!     `cwd` read-write, sets it as the workdir, mounts each `writable` path, leaves the network
//!     open, and runs the original program+args INSIDE `image`. Analogous to `wrap()` for sandbox,
//!     except the command runs in a container rather than a jailed host process.
//!   * `agg::isolation::container_available() -> bool` — is a container engine (docker/podman) runnable.
//!
//! SCOPE: this proves the CONFINEMENT MECHANISM + wiring against a base image. Running the actual
//! agent CLI *inside* the container with its auth (the "container problem", ISOLATION.md §2/§4) is a
//! documented follow-up, NOT part of this DoD.

use std::path::Path;
use std::process::Command;

#[test]
fn container_deserializes_from_the_tier_name() {
    let iso: agg::isolation::Isolation = serde_yaml::from_str("container").expect("`container` parses");
    assert_eq!(iso, agg::isolation::Isolation::Container);
    // and garbage still rejects
    assert!(serde_yaml::from_str::<agg::isolation::Isolation>("dockerish").is_err());
}

#[test]
fn containerize_builds_a_docker_run_that_bind_mounts_cwd_and_runs_the_inner_command() {
    let mut inner = Command::new("claude");
    inner.arg("-p").arg("do the thing");
    let cwd = Path::new("/private/tmp/agg-container-argv");
    std::fs::create_dir_all(cwd).ok();

    let cmd = agg::isolation::containerize(inner, cwd, &[], "alpine:3.20").expect("build the container command");
    let prog = cmd.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

    assert!(prog.ends_with("docker") || prog.ends_with("podman"), "runs a container engine, got: {prog}");
    assert!(args.iter().any(|a| a == "run"), "it is a `run`: {args:?}");
    assert!(args.iter().any(|a| a == "--rm"), "ephemeral (--rm): {args:?}");
    // cwd is bind-mounted (`-v <cwd>:<cwd>`) AND is the working directory (`-w <cwd>`)
    assert!(
        args.windows(2).any(|w| w[0] == "-v" && w[1].contains("agg-container-argv") && w[1].contains(':')),
        "cwd bind-mounted rw: {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w[0] == "-w" && w[1].contains("agg-container-argv")),
        "cwd is the workdir: {args:?}"
    );
    // the original program + args survive, in order, AFTER the image name
    let img = args.iter().position(|a| a == "alpine:3.20").expect("image present in argv");
    assert_eq!(
        &args[img + 1..],
        &["claude".to_string(), "-p".to_string(), "do the thing".to_string()],
        "inner program + args run inside the image, in order: {args:?}"
    );
}

#[test]
fn container_available_returns_a_bool() {
    let _: bool = agg::isolation::container_available();
}

/// REAL container confinement — needs a running Docker/Podman engine. `#[ignore]`d so the plain
/// `cargo test` suite stays hermetic; the loop's `container_confines` judge runs it with
/// `--ignored`. Proves the tier ACTUALLY confines: a write inside the bind-mounted cwd reaches the
/// host, and a write OUTSIDE the mount does not.
///
/// macOS + colima note: only host dirs colima mounts into its VM (default: `$HOME`) can bind-mount
/// rw, so the jail lives under `$HOME`; the escape target is a sibling path that is NOT bind-mounted.
#[test]
#[ignore = "needs a real container engine; the loop's container_confines judge runs it with --ignored"]
fn container_confines_writes() {
    assert!(agg::isolation::container_available(), "no container engine on this host — cannot prove confinement");
    let home = std::env::var("HOME").expect("HOME");
    let pid = std::process::id();
    let jail = Path::new(&home).join(format!("agg-container-jail-{pid}"));
    let escape = Path::new(&home).join(format!("agg-container-escape-{pid}.txt")); // NOT bind-mounted
    std::fs::create_dir_all(&jail).unwrap();
    let _ = std::fs::remove_file(&escape);

    let mut inner = Command::new("sh");
    inner.arg("-c").arg(format!(
        "echo in > inside.txt && echo INSIDE_OK; echo out > '{}' 2>/dev/null || true",
        escape.display()
    ));
    let out = agg::isolation::containerize(inner, &jail, &[], "alpine:3.20")
        .expect("build the container command")
        .output()
        .expect("run the container");

    let wrote_inside = jail.join("inside.txt").exists();
    let leaked = escape.exists();
    let _ = std::fs::remove_file(&escape);
    let _ = std::fs::remove_dir_all(&jail);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(wrote_inside, "inside-cwd write must reach the host via the bind-mount\nstderr: {stderr}");
    assert!(!leaked, "a write OUTSIDE the bind-mount leaked to the host — the container did NOT confine");
}
