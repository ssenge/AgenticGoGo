//! The container tier's egress path: an `--internal` network and a one-port RELAY.
//!
//! ```text
//!  worker container ──(agg-egress-<run>: --internal, no route out)──▶ relay :3128
//!                                                                      │ (default bridge)
//!                                                                      ▼
//!                                                  agg's proxy on the host (the allow-list)
//! ```
//!
//! A container on a Docker `--internal` network has NO default route: a direct connection to any
//! address — an IP, a DoH resolver, a raw socket — is "network unreachable", and the engine's DNS
//! does not resolve outside names there (measured: SERVFAIL). The one thing it can reach is the
//! relay, a container on BOTH networks running busybox `nc` (`-lk -e`: one relay per connection),
//! which pipes bytes to agg's proxy on the host and decides nothing — the allow-list is enforced in
//! one place, the proxy. The relay image is the base image the tier already uses (`alpine`).
//!
//! The relay reaches the host as `host.docker.internal` where the engine provides it (Docker
//! Desktop, colima/lima, OrbStack — it maps to the host's loopback, where the proxy listens), and
//! on a Linux engine at the default bridge's gateway, where agg binds a second listener.
//!
//! Every network and container is labelled `agg.egress=<project key>`; a run removes its own when it
//! ends, and removes any a crashed run left behind before it starts.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The port the relay listens on inside the internal network.
pub const RELAY_PORT: u16 = 3128;

/// What the relay runs on: the base image (busybox `nc` with `-e`).
const RELAY_IMAGE: &str = super::super::DEFAULT_IMAGE;

/// One run's internal network + relay. Dropping it removes both.
#[derive(Debug)]
pub struct Bridge {
    engine: &'static str,
    /// the internal network a confined container joins.
    pub network: String,
    /// the relay's name — its address on the internal network.
    pub relay: String,
}

impl Bridge {
    /// Create the network and the relay to `target:port` (agg's proxy as the relay reaches it).
    /// `key` names the project; leftovers labelled with it are removed first.
    pub fn start(target: &str, port: u16, key: &str) -> Result<Bridge, String> {
        let engine = super::super::container_engine().ok_or("no container engine answers")?;
        sweep(engine, key);
        let id = format!("{key}-{}", std::process::id());
        let b = Bridge { engine, network: format!("agg-egress-{id}"), relay: format!("agg-egress-relay-{id}") };
        let label = format!("agg.egress={key}");
        run(engine, &["network", "create", "--internal", "--label", &label, &b.network])?;
        let mut args: Vec<String> = ["run", "-d", "--rm", "--name", &b.relay, "--label", &label, "--network", "bridge"].map(String::from).to_vec();
        if target == "host.docker.internal" && cfg!(target_os = "linux") {
            args.extend(["--add-host".into(), "host.docker.internal:host-gateway".into()]);
        }
        let port_s = port.to_string();
        let relay_port = RELAY_PORT.to_string();
        args.extend([RELAY_IMAGE, "nc", "-lk", "-p", &relay_port, "-e", "nc", target, &port_s].map(String::from));
        run(engine, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        run(engine, &["network", "connect", &b.network, &b.relay])?;
        b.wait_ready(target, port)?;
        Ok(b)
    }

    /// The `HTTPS_PROXY` a container on [`Self::network`] uses.
    pub fn proxy_url(&self) -> String {
        format!("http://{}:{RELAY_PORT}", self.relay)
    }

    /// Wait until the relay answers AND its far end is agg's proxy: a request for a host no
    /// allow-list holds must come back `403` from the proxy. Anything else in 10 s means the relay
    /// cannot reach the host, and the run is refused rather than started with a dead network.
    fn wait_ready(&self, target: &str, port: u16) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let probe = format!(
            "printf 'CONNECT {}:1 HTTP/1.1\\r\\n\\r\\n' | nc -w 3 127.0.0.1 {RELAY_PORT}",
            super::proxy::SELFTEST_HOST
        );
        let mut last = String::new();
        while Instant::now() < deadline {
            match output(self.engine, &["exec", &self.relay, "sh", "-c", &probe]) {
                Ok(out) if out.contains(" 403 ") => return Ok(()),
                Ok(out) => last = out,
                Err(e) => last = e,
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!(
            "the egress relay cannot reach agg's proxy at {target}:{port} from inside the engine ({}) — \
             the network allow-list cannot be enforced, so the run does not start",
            last.trim()
        ))
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = output(self.engine, &["rm", "-f", &self.relay]);
        let _ = output(self.engine, &["network", "rm", &self.network]);
    }
}

/// Remove every relay and network an earlier run of this project left behind.
fn sweep(engine: &str, key: &str) {
    let label = format!("label=agg.egress={key}");
    if let Ok(ids) = output(engine, &["ps", "-aq", "--filter", &label]) {
        for id in ids.split_whitespace() {
            let _ = output(engine, &["rm", "-f", id]);
        }
    }
    if let Ok(ids) = output(engine, &["network", "ls", "-q", "--filter", &label]) {
        for id in ids.split_whitespace() {
            let _ = output(engine, &["network", "rm", id]);
        }
    }
}

fn run(engine: &str, args: &[&str]) -> Result<(), String> {
    output(engine, args).map(|_| ())
}

/// `<engine> args…` → stdout, or its stderr as the error.
fn output(engine: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(engine).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{engine}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("`{engine} {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}
