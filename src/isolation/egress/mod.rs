//! NETWORK EGRESS — `isolation_policy.network: open | { allow: [host[:port], …] }`.
//!
//! `open` (the default) is the network every tier has always had. `allow` confines every process
//! the run confines — worker, judges, the LLM judge's one-shot, and on `sandbox` the hooks — to the
//! listed destinations, through ONE enforcement point: agg's own proxy ([`proxy`]), which checks the
//! name the client asked for and resolves it itself. Each tier's job is to make the proxy the only
//! way out:
//!
//! | tier | how the proxy becomes the only way out |
//! |---|---|
//! | `container` (the reference) | an `--internal` network with no route out; a relay to the proxy is the one reachable address ([`bridge`]) |
//! | `sandbox` on macOS | Seatbelt denies every outbound connection but TCP to the proxy's loopback port (and the `localhost` entries); DNS is denied too — the proxy resolves |
//! | `sandbox` on Linux | **refused** at startup: `bwrap` cannot narrow the host network to one port; use `container` |
//! | `none` | **refused** at startup: nothing would stop a direct connection |
//!
//! Every confined process gets `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` (both cases) pointing at the
//! proxy, `NO_PROXY` for its own loopback, and `NODE_USE_ENV_PROXY=1` (Node's built-in fetch honours
//! the variables only with it). A process that ignores or rewrites them gains nothing: the jail
//! still allows only the proxy.

pub mod allow;
pub mod bridge;
pub mod proxy;

use allow::AllowList;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The `isolation_policy.network` setting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Network {
    /// no egress policy — the network is fully open (the default, and every tier's behaviour
    /// before the key existed).
    #[default]
    Open,
    /// only these `host[:port]` destinations, through agg's proxy.
    Allow(Vec<String>),
}

impl<'de> serde::Deserialize<'de> for Network {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AllowBody {
            allow: Vec<String>,
        }
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Network;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("`open`, or `{ allow: [host[:port], …] }`")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Network, E> {
                match v {
                    "open" => Ok(Network::Open),
                    other => Err(E::invalid_value(serde::de::Unexpected::Str(other), &self)),
                }
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Network, A::Error> {
                let b = <AllowBody as serde::Deserialize>::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Network::Allow(b.allow))
            }
        }
        d.deserialize_any(V)
    }
}

impl schemars::JsonSchema for Network {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Network".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "`open` (no egress policy, the default), or `{ allow: [host[:port], …] }` — only those destinations, through agg's proxy.",
            "anyOf": [
                { "const": "open" },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["allow"],
                    "properties": { "allow": { "type": "array", "items": { "type": "string" } } }
                }
            ]
        })
    }
}

/// One run's egress enforcement: the allow-list, and — started on first use — the proxy and, for
/// a container run, the bridge. Dropped with the run's policy, which stops and removes both.
pub struct Egress {
    pub allow: Arc<AllowList>,
    key: String,
    local: OnceLock<Result<proxy::Proxy, String>>,
    container: OnceLock<Result<ContainerSide, String>>,
}

/// What a container needs: the network to join and the proxy URL there.
struct ContainerSide {
    /// a second proxy listener, on the engine's bridge gateway (Linux engines only).
    _gateway: Option<proxy::Proxy>,
    bridge: bridge::Bridge,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress").field("allow", &self.allow).field("key", &self.key).finish_non_exhaustive()
    }
}

impl Egress {
    /// The egress state for the project in `dir`, nothing started yet.
    pub fn new(allow: AllowList, dir: &Path) -> Egress {
        Egress { allow: Arc::new(allow), key: project_key(dir), local: OnceLock::new(), container: OnceLock::new() }
    }

    /// The loopback port of the proxy a SANDBOXED process is allowed to reach (started now if
    /// this is the first confined spawn of the run).
    pub fn sandbox_port(&self) -> Result<u16, String> {
        self.local
            .get_or_init(|| {
                proxy::Proxy::start("127.0.0.1:0".parse().expect("addr"), self.allow.clone())
                    .map_err(|e| format!("cannot start agg's egress proxy: {e}"))
            })
            .as_ref()
            .map(|p| p.addr().port())
            .map_err(Clone::clone)
    }

    /// The internal network a CONTAINER joins and the proxy URL it uses there.
    pub fn container(&self) -> Result<(String, String), String> {
        self.container
            .get_or_init(|| self.start_container_side())
            .as_ref()
            .map(|c| (c.bridge.network.clone(), c.bridge.proxy_url()))
            .map_err(Clone::clone)
    }

    fn start_container_side(&self) -> Result<ContainerSide, String> {
        if cfg!(target_os = "linux") {
            // a Linux engine runs on THIS kernel: the relay reaches the host at the default
            // bridge's gateway, so the proxy listens there too. (Docker Desktop for Linux runs a VM
            // instead; its `host.docker.internal` path is taken when the gateway cannot be bound.)
            if let Some(gw) = bridge_gateway() {
                if let Ok(p) = proxy::Proxy::start(std::net::SocketAddr::new(gw, 0), self.allow.clone()) {
                    let port = p.addr().port();
                    let bridge = bridge::Bridge::start(&gw.to_string(), port, &self.key)?;
                    return Ok(ContainerSide { _gateway: Some(p), bridge });
                }
            }
        }
        let port = self.sandbox_port()?;
        let bridge = bridge::Bridge::start("host.docker.internal", port, &self.key)?;
        Ok(ContainerSide { _gateway: None, bridge })
    }
}

/// Why a step on `tier`, run by `agent`, cannot honour `allow` — empty when it can. The ONE rule
/// both startup (`capability::assess`, per YAML step) and the driver (`Agg::step`, per code-built
/// step) apply. `(on_tier, on_endpoint)`: the first belong at the step's `isolation` key, the
/// second at the allow-list.
pub fn step_problems(tier: super::Isolation, agent: &dyn crate::backend::AgentBackend, allow: &AllowList) -> (Vec<String>, Vec<String>) {
    let mut on_tier = Vec::new();
    match tier {
        super::Isolation::None => on_tier.push(
            "runs under `isolation: none`, but the run has a network allow-list — nothing would stop a \
             direct connection. Confine it: `isolation: container` (or `sandbox` on macOS)"
                .to_string(),
        ),
        super::Isolation::Sandbox if !cfg!(target_os = "macos") => on_tier.push(
            "runs under `isolation: sandbox`, which cannot enforce a network allow-list on this OS \
             (bwrap cannot narrow the host network to one port) — use `isolation: container`"
                .to_string(),
        ),
        super::Isolation::Container if super::container_engine() == Some("podman") => on_tier.push(
            "runs under `isolation: container` on podman, but the allow-list's container network is \
             built on Docker's `--internal` networks — use Docker (or colima), or `network: open`"
                .to_string(),
        ),
        _ => {}
    }
    (on_tier, endpoint_problem(agent, allow).into_iter().collect())
}

/// Why `agent` could not work behind `allow`, or `None`.
pub fn endpoint_problem(agent: &dyn crate::backend::AgentBackend, allow: &AllowList) -> Option<String> {
    let hosts = agent.api_hosts();
    if hosts.is_empty() {
        return Some(format!(
            "agent `{}` declares no API endpoint, so agg cannot verify the allow-list lets it reach its \
             model — refused rather than started blind",
            agent.name()
        ));
    }
    let missing: Vec<&str> = hosts.iter().map(String::as_str).filter(|h| !allow.allows_host(h)).collect();
    (!missing.is_empty()).then(|| {
        format!(
            "agent `{}` must reach {} — add {} to `isolation_policy.network.allow`, or it could never \
             run behind it",
            agent.name(),
            missing.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(" and "),
            if missing.len() == 1 { "it" } else { "them" }
        )
    })
}

/// The env every confined process of an allow-listed run gets, pointing at `proxy_url`.
pub fn proxy_env(proxy_url: &str) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = Vec::new();
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        v.push((k, proxy_url.to_string()));
    }
    for k in ["NO_PROXY", "no_proxy"] {
        v.push((k, "localhost,127.0.0.1,::1".to_string()));
    }
    v.push(("NODE_USE_ENV_PROXY", "1".to_string()));
    v
}

/// The engine's default bridge gateway (`docker network inspect bridge`), if it reports one.
fn bridge_gateway() -> Option<std::net::IpAddr> {
    let engine = super::container_engine()?;
    let out = std::process::Command::new(engine)
        .args(["network", "inspect", "bridge", "--format", "{{range .IPAM.Config}}{{.Gateway}} {{end}}"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().find_map(|s| s.parse::<std::net::Ipv4Addr>().ok().map(std::net::IpAddr::V4))
}

/// A short, stable, engine-name-safe key for a project dir.
fn project_key(dir: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_parses_open_or_an_allow_list_and_nothing_else() {
        assert_eq!(serde_yaml::from_str::<Network>("open").unwrap(), Network::Open);
        assert_eq!(serde_yaml::from_str::<Network>("{ allow: [\"a.com\"] }").unwrap(), Network::Allow(vec!["a.com".into()]));
        assert!(serde_yaml::from_str::<Network>("closed").is_err());
        assert!(serde_yaml::from_str::<Network>("{ allow: [], deny: [] }").is_err(), "deny_unknown_fields");
        assert!(serde_yaml::from_str::<Network>("{ alow: [] }").is_err());
        assert_eq!(Network::default(), Network::Open);
    }

    #[test]
    fn every_proxy_variable_points_at_the_proxy_and_loopback_bypasses_it() {
        let env = proxy_env("http://127.0.0.1:9");
        for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            assert!(env.contains(&(k, "http://127.0.0.1:9".to_string())), "{k}");
        }
        assert!(env.contains(&("NO_PROXY", "localhost,127.0.0.1,::1".to_string())));
    }

    /// The sandbox side starts ONE proxy, lazily, and reuses it.
    #[test]
    fn the_local_proxy_starts_once_on_first_use() {
        let tmp = tempfile::tempdir().unwrap();
        let e = Egress::new(AllowList::default(), tmp.path());
        let p = e.sandbox_port().unwrap();
        assert_eq!(e.sandbox_port().unwrap(), p);
        assert!(std::net::TcpStream::connect(("127.0.0.1", p)).is_ok());
    }
}
