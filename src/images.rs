//! The reference container images (`docker/<agent>/`): `rlt images build [--rlt <linux binary>] [<agent>…]` builds them, each with
//! the agent's CLI and the `rlt` binary (so a lane can run in it: `rlt lane --spec`), tagged `rlt-<agent>:<version>` and `:latest`;
//! `rlt images list` and `rlt doctor --containers` say which exist.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The images: the agent's name and the files of its build folder.
const IMAGES: &[(&str, &[(&str, &str)])] = &[
    ("claude", &[("Dockerfile", include_str!("../docker/claude/Dockerfile"))]),
    ("codex", &[("Dockerfile", include_str!("../docker/codex/Dockerfile"))]),
    ("swe-metrics", &[
        ("Dockerfile", include_str!("../docker/swe-metrics/Dockerfile")),
        ("build.sh", include_str!("../docker/swe-metrics/build.sh")),
        ("userinfo.cjs", include_str!("../docker/swe-metrics/userinfo.cjs")),
    ]),
];

/// The tag of `agent`'s image for this version of `rlt`.
pub fn tag(agent: &str) -> String {
    format!("rlt-{agent}:{}", env!("CARGO_PKG_VERSION"))
}

/// Writes the build folder of `agent` into `dir`: its files, the `rlt` binary, and the lines that put it on the image's PATH.
pub fn stage(agent: &str, rlt: &Path, dir: &Path) -> Result<(), String> {
    let (_, files) = IMAGES.iter().find(|(a, _)| *a == agent).ok_or_else(|| format!("no reference image `{agent}`: {}", names().join(", ")))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in *files {
        let text = if *name == "Dockerfile" { format!("{}\n# the engine itself, so a lane can run in the image: `rlt lane --spec`\nCOPY rlt /usr/local/bin/rlt\n", text.trim_end()) } else { text.to_string() };
        std::fs::write(dir.join(name), text).map_err(|e| format!("{name}: {e}"))?;
    }
    std::fs::copy(rlt, dir.join("rlt")).map_err(|e| format!("the rlt binary {}: {e}", rlt.display()))?;
    Ok(())
}

/// The names of the reference images.
pub fn names() -> Vec<&'static str> {
    IMAGES.iter().map(|(a, _)| *a).collect()
}

/// Whether `engine` has `image`.
fn present(engine: &str, image: &str) -> bool {
    Command::new(engine).args(["image", "inspect", image]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// `rlt images build [--rlt <linux rlt binary>] [<agent>…]`: the images of the agents named, all by default. The binary is
/// this one on Linux; elsewhere (a Mac builds Linux images) a Linux build of the same version is given.
pub fn build(args: &[String]) -> ExitCode {
    match try_build(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rlt images build: {e}");
            ExitCode::from(1)
        }
    }
}

fn try_build(args: &[String]) -> Result<(), String> {
    let engine = crate::isolation::container_engine().ok_or("no container engine (docker or podman) on this machine")?;
    let (mut rlt, mut agents) = (None::<PathBuf>, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rlt" => rlt = Some(PathBuf::from(it.next().ok_or("--rlt needs a path")?)),
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => agents.push(other.to_string()),
        }
    }
    let rlt = match rlt {
        Some(p) => p,
        None if cfg!(target_os = "linux") => std::env::current_exe().map_err(|e| e.to_string())?,
        None => return Err("the images hold a Linux build of rlt: give it with `--rlt <path>` (cargo build --release --target x86_64-unknown-linux-musl)".into()),
    };
    if agents.is_empty() {
        agents = names().into_iter().map(String::from).collect();
    }
    for agent in agents {
        let dir = std::env::temp_dir().join(format!("rlt-image-{}-{agent}", std::process::id()));
        stage(&agent, &rlt, &dir)?;
        let tags = [tag(&agent), format!("rlt-{agent}:latest")];
        println!("building {} …", tags[0]);
        let status = Command::new(engine).args(["build", "-t", &tags[0], "-t", &tags[1]]).arg(&dir).status().map_err(|e| format!("{engine}: {e}"))?;
        let _ = std::fs::remove_dir_all(&dir);
        if !status.success() {
            return Err(format!("`{engine} build` of {agent} failed"));
        }
    }
    Ok(())
}

/// What `image` needs when `engine` doesn't have it, if it is one of ours: the command that builds it.
pub fn missing_hint(engine: &str, image: &str) -> Option<String> {
    let (name, version) = image.split_once(':').unwrap_or((image, "latest"));
    let agent = name.strip_prefix("rlt-").filter(|a| names().contains(a))?;
    if present(engine, image) {
        return None;
    }
    let _ = version;
    Some(format!("the image `{image}` is missing: build it with `rlt images build --rlt <a Linux rlt of this version> {agent}`"))
}

/// The engine and each reference image, as a table: what `rlt doctor --containers` and `rlt images list` print.
pub fn report() -> String {
    let Some(engine) = crate::isolation::container_engine() else {
        return "container engine   none (docker or podman: neither is on PATH)\n".into();
    };
    let version = Command::new(engine).args(["version", "--format", "{{.Server.Version}}"]).output().ok().filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "?".into());
    let mut out = format!("container engine   {engine} {version}\n");
    for agent in names() {
        let image = tag(agent);
        out.push_str(&format!("image {image:<28} {}\n", if present(engine, &image) { "built" } else { "missing: rlt images build" }));
    }
    out
}

/// `rlt install-remote <host> [ssh options…]`: runs the install script on `host` over ssh (the script of this binary, sent on
/// stdin: nothing is fetched to run it), so a lane target there has an `rlt`.
pub fn install_remote(args: &[String]) -> ExitCode {
    let Some(host) = args.first() else {
        eprintln!("rlt install-remote <host> [ssh options…]");
        return ExitCode::from(2);
    };
    let script = include_str!("../scripts/install.sh");
    let child = Command::new("ssh").args(["-o", "BatchMode=yes"]).args(&args[1..]).arg(host).args(["sh", "-s"]).stdin(Stdio::piped()).spawn();
    let Ok(mut child) = child else {
        eprintln!("rlt install-remote: ssh could not run");
        return ExitCode::from(1);
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, script.as_bytes());
    }
    match child.wait() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build folder holds the agent's files, the binary and the line that copies it; an unknown agent is refused naming the known.
    #[test]
    fn a_build_folder_holds_the_files_the_binary_and_its_copy_line() {
        let (dir, bin) = (tempfile::tempdir().unwrap(), tempfile::NamedTempFile::new().unwrap());
        std::fs::write(bin.path(), "ELF").unwrap();
        stage("swe-metrics", bin.path(), dir.path()).unwrap();
        for f in ["Dockerfile", "build.sh", "userinfo.cjs", "rlt"] {
            assert!(dir.path().join(f).is_file(), "{f}");
        }
        let docker = std::fs::read_to_string(dir.path().join("Dockerfile")).unwrap();
        assert!(docker.trim_end().ends_with("COPY rlt /usr/local/bin/rlt"), "{docker}");
        let err = stage("nope", bin.path(), dir.path()).unwrap_err();
        assert!(err.contains("claude, codex, swe-metrics"), "{err}");
        assert_eq!(tag("claude"), format!("rlt-claude:{}", env!("CARGO_PKG_VERSION")));
    }

    /// A missing image of ours says the command that builds it; an image of another's says nothing.
    #[test]
    fn a_missing_image_names_the_command_that_builds_it() {
        let hint = missing_hint("false", "rlt-claude:0.5.0").unwrap();
        assert!(hint.contains("rlt images build --rlt <a Linux rlt of this version> claude"), "{hint}");
        assert!(missing_hint("false", "alpine:3.20").is_none() && missing_hint("false", "rlt-unknown:1").is_none());
        assert!(missing_hint("true", "rlt-claude:0.5.0").is_none(), "present: nothing to say");
    }
}
