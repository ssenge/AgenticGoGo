//! `agg doctor --json` — the same preflight as the text doctor, as a stable, versioned document
//! an orchestrator can act on before it spends anything: is the config valid, are the agents it
//! names installed (path, version, what each can do), which isolation tiers this host offers.
//!
//! The config half IS `agg check` ([`crate::check::validate`]), so doctor and startup cannot
//! disagree about what is wrong. Doctor adds the host facts and the one check startup does not
//! make (the forward state file exists).

use crate::backend::{for_name, Capabilities, KNOWN};
use crate::check::{self, Validation};
use crate::isolation::Isolation;
use crate::problem::Problem;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The whole `agg doctor --json` document. `schema_version` is shared with `agg check --json`.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub agg_version: &'static str,
    /// no problems: the config is valid, every agent it names is installed, every tier it asks
    /// for is available, and the state file exists. Warnings do not count.
    pub ok: bool,
    pub config: ConfigFacts,
    /// EVERY backend agg knows, whether or not the config names it (`used`).
    pub backends: Vec<BackendFacts>,
    pub isolation: Tiers,
    /// `docker` or `podman` when one answers, else null.
    pub container_engine: Option<&'static str>,
    /// every image an `isolation: container` run of this config starts — the steps' and the
    /// judges' — with the agent CLIs it needs and whether it has them (`clis.<bin>`: the version
    /// line, or null when missing), plus any mounted path the engine cannot see. Empty without a
    /// container tier or an engine.
    pub container_images: Vec<crate::isolation::probe::ImageFacts>,
    /// the network egress policy and how each tier would enforce it on this host.
    pub network: NetworkFacts,
    pub problems: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

#[derive(Debug, Serialize)]
pub struct ConfigFacts {
    pub path: String,
    pub found: bool,
    /// parses AND assembles AND the agents can do what it asks.
    pub valid: bool,
    /// the run-set size, when it assembled.
    pub judges: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct BackendFacts {
    pub name: &'static str,
    /// the CLI agg launches, resolved via PATH.
    pub bin: &'static str,
    /// named by the config (as a worker, the ruler, or a step's agent).
    pub used: bool,
    /// `<bin> --version` succeeds — the same probe `agg run` makes.
    pub installed: bool,
    /// where `bin` resolves on PATH, if anywhere.
    pub path: Option<PathBuf>,
    /// the first line `<bin> --version` printed.
    pub version: Option<String>,
    /// the agent can additionally confine itself inside agg's jail (reporting only).
    pub self_sandboxes: bool,
    pub capabilities: Capabilities,
}

/// `isolation_policy.network` as this host would enforce it.
#[derive(Debug, Serialize)]
pub struct NetworkFacts {
    /// `open` or `allow`.
    pub mode: &'static str,
    /// the allow-list as written (empty when open).
    pub allow: Vec<String>,
    /// per tier: the mechanism that enforces the list here, or why that tier refuses it.
    pub sandbox: &'static str,
    pub container: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Tiers {
    pub none: Tier,
    pub sandbox: Tier,
    pub container: Tier,
}

#[derive(Debug, Serialize)]
pub struct Tier {
    pub available: bool,
    /// what provides it here: `sandbox-exec` / `bwrap`, `docker` / `podman`; null for `none`.
    pub mechanism: Option<&'static str>,
    /// some step of the config resolves to this tier.
    pub requested: bool,
}

/// Build the report for the project at `config_base` / `config`.
pub fn report(config_base: &Path, config: &Path) -> DoctorReport {
    let v = check::validate(config_base, config);
    let mut problems = v.problems.clone();
    if let Some(c) = &v.cfg {
        if !config_base.join(&c.defaults.state).exists() {
            problems.push(Problem::new(
                "defaults.state",
                format!("state file `{}` does not exist — create it (or run `agg init`)", c.defaults.state),
            ));
        }
    }
    let used = v.cfg.as_ref().map(|c| c.agent_names()).unwrap_or_default();
    let backends = KNOWN.iter().filter_map(|n| for_name(n).ok()).map(|b| backend_facts(b, used.iter().any(|u| u == b.name()))).collect();
    let container_engine = crate::isolation::container_engine();
    DoctorReport {
        schema_version: check::SCHEMA_VERSION,
        agg_version: env!("CARGO_PKG_VERSION"),
        ok: problems.is_empty(),
        config: ConfigFacts {
            path: config.display().to_string(),
            found: config.exists(),
            valid: v.problems.is_empty(),
            judges: v.assembly.as_ref().map(|a| a.engine.judges.len()),
        },
        backends,
        isolation: tiers(&v, container_engine),
        container_engine,
        container_images: v.images.clone(),
        network: network_facts(v.cfg.as_ref(), container_engine),
        problems,
        warnings: v.warnings,
    }
}

fn backend_facts(b: &dyn crate::backend::AgentBackend, used: bool) -> BackendFacts {
    let version = version_of(b.bin());
    BackendFacts {
        name: b.name(),
        bin: b.bin(),
        used,
        installed: version.is_some(),
        path: on_path(b.bin()),
        version: version.filter(|v| !v.is_empty()),
        self_sandboxes: b.self_sandboxes(),
        capabilities: b.capabilities(),
    }
}

fn tiers(v: &Validation, engine: Option<&'static str>) -> Tiers {
    let requested = |tier: Isolation| {
        v.cfg.as_ref().is_some_and(|c| c.steps.keys().filter_map(|n| c.resolve_step(n).ok()).any(|s| s.isolation == tier))
    };
    let wrapper = crate::isolation::available();
    Tiers {
        none: Tier { available: true, mechanism: None, requested: requested(Isolation::None) },
        sandbox: Tier { available: wrapper, mechanism: wrapper.then_some(SANDBOX_WRAPPER).flatten(), requested: requested(Isolation::Sandbox) },
        container: Tier { available: engine.is_some(), mechanism: engine, requested: requested(Isolation::Container) },
    }
}

fn network_facts(cfg: Option<&crate::core::config::AggConfig>, engine: Option<&'static str>) -> NetworkFacts {
    let allow = match cfg.map(|c| &c.isolation_policy.network) {
        Some(crate::isolation::egress::Network::Allow(a)) => a.clone(),
        _ => return NetworkFacts { mode: "open", allow: vec![], sandbox: "open", container: "open" },
    };
    NetworkFacts {
        mode: "allow",
        allow,
        sandbox: if cfg!(target_os = "macos") { "seatbelt: outbound only to agg's proxy" } else { "refused: bwrap cannot narrow the network" },
        container: match engine {
            Some("docker") => "internal network + relay to agg's proxy",
            Some(_) => "refused: needs docker's --internal networks",
            None => "unavailable: no engine",
        },
    }
}

/// The OS wrapper behind `isolation: sandbox` on this platform.
const SANDBOX_WRAPPER: Option<&str> = if cfg!(target_os = "macos") {
    Some("sandbox-exec")
} else if cfg!(target_os = "linux") {
    Some("bwrap")
} else {
    None
};

/// `<bin> --version`: `Some(first line)` when it exits 0 (possibly empty), `None` otherwise.
fn version_of(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout).lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string()
    })
}

/// The first executable `bin` on PATH (what `Command::new(bin)` will run).
fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| executable(p))
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Windows has no exec bit; `Command::new(bin)` also finds `<bin>.exe`.
#[cfg(not(unix))]
fn executable(p: &Path) -> bool {
    p.is_file() || p.with_extension("exe").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_binary_has_no_version_and_no_path() {
        assert_eq!(version_of("agg-no-such-binary-xyz"), None);
        assert_eq!(on_path("agg-no-such-binary-xyz"), None);
    }

    #[test]
    fn a_present_binary_is_found_on_path() {
        assert!(on_path("sh").is_some(), "`sh` is on every unix PATH");
    }

    /// The JSON field names are the interface: pin the shape, not the host-dependent values.
    #[test]
    fn the_report_shape_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let r = serde_json::to_value(report(tmp.path(), &tmp.path().join("agg.yaml"))).unwrap();
        for key in ["schema_version", "agg_version", "ok", "config", "backends", "isolation", "container_engine", "container_images", "network", "problems", "warnings"] {
            assert!(r.get(key).is_some(), "missing `{key}`");
        }
        assert_eq!(r["ok"], false, "no config is not ok");
        assert_eq!(r["config"]["found"], false);
        let names: Vec<&str> = r["backends"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
        assert_eq!(names, KNOWN);
        assert!(r["backends"][0]["capabilities"]["supports_one_shot"].is_boolean());
        assert_eq!(r["isolation"]["none"]["available"], true);
        assert_eq!(r["network"]["mode"], "open");
    }
}
