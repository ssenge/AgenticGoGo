//! NETWORK EGRESS (`isolation_policy.network: { allow: [...] }`) against the REAL mechanisms — the
//! adversarial cases of the requirement, on each tier this host can run:
//!
//! | attack | macOS `sandbox` | `container` |
//! |---|---|---|
//! | direct connection to an IP not on the list | Seatbelt: EPERM | no route |
//! | DNS-over-HTTPS to a resolver not on the list | the proxy: 403 · direct: EPERM | the proxy: 403 · direct: no route |
//! | plain DNS | no `mDNSResponder` | the engine's DNS answers no outside name |
//! | a raw socket / UDP from a child process | EPERM | NET_RAW dropped, no route |
//! | a proxy the WORKER names (its own `HTTPS_PROXY`) | EPERM | no route |
//! | a name on no list, through agg's proxy | 403 | 403 |
//!
//! …while the one allowed destination works THROUGH the proxy. Every destination used is either a
//! local test server or refused before a packet leaves, so this needs no internet. Skipped LOUDLY
//! where the mechanism is missing (no usable kernel sandbox; no engine). The startup refusals run
//! everywhere.

#![cfg(unix)]

use agg::isolation::egress::{allow::AllowList, Egress};
use agg::isolation::policy::{self, RunPolicy};
use assert_cmd::cargo::CommandCargoExt;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

/// A loopback HTTP server answering every request with `EGRESS_ALLOWED_OK`. Returns its port.
fn server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let mut r = std::io::BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut c = c;
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\nEGRESS_ALLOWED_OK");
        }
    });
    port
}

fn register(dir: &Path, entries: &[String]) -> policy::PolicyGuard {
    let allow = AllowList::parse(entries).unwrap();
    policy::register(dir, RunPolicy { egress: Some(Egress::new(allow, dir)), ..RunPolicy::default() })
}

/// `name OK|BLOCKED` per probe, from inside the jail.
fn outcomes(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn assert_outcomes(text: &str, ok: &[&str], blocked: &[&str]) {
    for n in ok {
        assert!(text.contains(&format!("{n} OK")), "`{n}` must get through:\n{text}");
    }
    for n in blocked {
        assert!(text.contains(&format!("{n} BLOCKED")), "`{n}` must be blocked:\n{text}");
    }
}

const PROBE: &str = r#"t() { if sh -c "$2" >/dev/null 2>&1; then echo "$1 OK"; else echo "$1 BLOCKED"; fi; }"#;

fn kernel_sandbox_usable() -> bool {
    cfg!(target_os = "macos")
        && Command::new("sandbox-exec")
            .args(["-p", "(version 1)(deny default)(allow process*)(allow file-read*)(allow sysctl-read)(allow mach*)", "/usr/bin/true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// THE SANDBOX TIER (macOS Seatbelt): only TCP to agg's proxy leaves the jail.
#[test]
fn under_the_macos_sandbox_only_the_proxy_gets_out() {
    if !kernel_sandbox_usable() {
        eprintln!("SKIPPED: no usable macOS kernel sandbox here");
        return;
    }
    let srv = server();
    let tmp = tempfile::tempdir().unwrap();
    let _g = register(tmp.path(), &[format!("127.0.0.1:{srv}")]);
    let script = format!(
        r#"{PROBE}
t allowed_via_proxy "curl -sf -m 5 --noproxy '' -x \"$HTTPS_PROXY\" http://127.0.0.1:{srv}/ | grep -q EGRESS_ALLOWED_OK"
t direct_to_the_allowed_port "curl -sf -m 3 --noproxy '*' http://127.0.0.1:{srv}/"
t direct_ip "curl -s -m 3 --noproxy '*' http://1.1.1.1/"
t unlisted_name_via_proxy "curl -sf -m 5 https://example.com/"
t doh_via_proxy "curl -sf -m 5 --doh-url https://1.1.1.1/dns-query https://example.com/"
t doh_direct "curl -sf -m 5 --noproxy '*' https://1.1.1.1/dns-query?name=example.com"
t dns_lookup "python3 -c 'import socket; socket.getaddrinfo(\"example.com\", 443)'"
t udp "python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b\"x\", (\"8.8.8.8\", 53))'"
t raw_socket "python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_ICMP)'"
t child_process "sh -c 'curl -s -m 3 --noproxy \"*\" http://1.1.1.1/'"
t worker_set_proxy "HTTPS_PROXY=http://1.1.1.1:3128 https_proxy=http://1.1.1.1:3128 curl -sf -m 5 https://example.com/"
t localhost_resolves "python3 -c 'import socket; socket.getaddrinfo(\"localhost\", 80)'"
"#
    );
    let mut inner = Command::new("/bin/sh");
    inner.arg("-c").arg(&script);
    let out = agg::isolation::wrap(inner, tmp.path(), &[], &[]).unwrap().output().unwrap();
    let text = outcomes(&out);
    assert_outcomes(
        &text,
        &["allowed_via_proxy", "localhost_resolves"],
        &[
            "direct_to_the_allowed_port",
            "direct_ip",
            "unlisted_name_via_proxy",
            "doh_via_proxy",
            "doh_direct",
            "dns_lookup",
            "udp",
            "raw_socket",
            "child_process",
            "worker_set_proxy",
        ],
    );
}

/// A `localhost` entry grants the sandboxed worker its own loopback (a test server it starts);
/// without it, even its own server is out of reach.
#[test]
fn under_the_macos_sandbox_a_localhost_entry_grants_loopback() {
    if !kernel_sandbox_usable() {
        eprintln!("SKIPPED: no usable macOS kernel sandbox here");
        return;
    }
    let script = format!(
        r#"{PROBE}
t own_server "python3 -c 'import socket; s=socket.socket(); s.bind((\"127.0.0.1\", 0)); s.listen(); socket.create_connection(s.getsockname(), 3)'"
"#
    );
    for (entries, want) in [(vec!["api.example.com".to_string()], "own_server BLOCKED"), (vec!["localhost".to_string()], "own_server OK")] {
        let tmp = tempfile::tempdir().unwrap();
        let _g = register(tmp.path(), &entries);
        let mut inner = Command::new("/bin/sh");
        inner.arg("-c").arg(&script);
        let text = outcomes(&agg::isolation::wrap(inner, tmp.path(), &[], &[]).unwrap().output().unwrap());
        assert!(text.contains(want), "{entries:?} ⇒ {want}:\n{text}");
    }
}

/// THE CONTAINER TIER (the reference): an `--internal` network whose only reachable address is the
/// relay to agg's proxy.
#[test]
fn in_a_container_only_the_relay_to_the_proxy_gets_out() {
    if !agg::isolation::container_available() {
        eprintln!("SKIPPED: no container engine");
        return;
    }
    let srv = server();
    // under target/: a VM-backed engine cannot see $TMPDIR
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/agg-egress-test");
    std::fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(&base).unwrap();
    let g = register(tmp.path(), &[format!("127.0.0.1:{srv}")]);
    let script = format!(
        r#"{PROBE}
p="${{HTTPS_PROXY#http://}}"; ph="${{p%:*}}"; pp="${{p##*:}}"
t allowed_via_proxy "printf 'GET http://127.0.0.1:{srv}/ HTTP/1.0\r\n\r\n' | nc -w 5 $ph $pp | grep -q EGRESS_ALLOWED_OK"
t direct_ip "nc -w 3 1.1.1.1 443 </dev/null"
t host_directly "nc -w 3 host.docker.internal {srv} </dev/null"
t unlisted_name_via_proxy "printf 'CONNECT example.com:443 HTTP/1.1\r\n\r\n' | nc -w 5 $ph $pp | grep -q ' 200 '"
t doh_via_proxy "printf 'CONNECT 1.1.1.1:443 HTTP/1.1\r\n\r\n' | nc -w 5 $ph $pp | grep -q ' 200 '"
t doh_direct "wget -q -T 3 -O /dev/null https://1.1.1.1/dns-query"
t dns_lookup "nslookup example.com"
t raw_socket "ping -c 1 -W 2 1.1.1.1"
t child_process "sh -c 'nc -w 3 1.1.1.1 80 </dev/null'"
t worker_set_proxy "http_proxy=http://1.1.1.1:3128 wget -q -T 3 -O /dev/null http://example.com/"
"#
    );
    let mut inner = Command::new("/bin/sh");
    inner.arg("-c").arg(&script);
    let out = agg::isolation::containerize(inner, tmp.path(), &[], &[], agg::isolation::DEFAULT_IMAGE).unwrap().output().unwrap();
    let text = outcomes(&out);
    drop(g); // removes the relay and the internal network
    assert_outcomes(
        &text,
        &["allowed_via_proxy"],
        &["direct_ip", "host_directly", "unlisted_name_via_proxy", "doh_via_proxy", "doh_direct", "dns_lookup", "raw_socket", "child_process", "worker_set_proxy"],
    );
}

// ── startup refusals ────────────────────────────────────────────────────────────────────────────

fn check_problems(yaml: &str) -> Vec<(String, String)> {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::create_dir_all(d.join("agg/judges")).unwrap();
    std::fs::create_dir_all(d.join("agg/state")).unwrap();
    std::fs::write(d.join("agg/agg.yaml"), yaml).unwrap();
    std::fs::write(d.join("agg/judges/ok.sh"), "#!/bin/sh\necho '{\"met\":true}'\n").unwrap();
    let out = Command::cargo_bin("agg").unwrap().args(["check", "--json"]).current_dir(d).env("HOME", d).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["problems"].as_array().unwrap().iter().map(|p| (p["path"].as_str().unwrap().into(), p["message"].as_str().unwrap().into())).collect()
}

fn yaml(tier: &str, allow: &str) -> String {
    format!(
        "project: \"p\"\ndefaults: {{ isolation: \"{tier}\" }}\nsteps: {{ w: {{}} }}\nsequence: {{ steps: [\"w\"], done_if: \"ok\" }}\n\
         summary: {{ enabled: false }}\nisolation_policy: {{ network: {{ allow: [{allow}] }} }}\n"
    )
}

/// The agent's own endpoint must be on the list — otherwise the worker could never run.
#[test]
fn an_allow_list_without_the_agents_endpoint_is_refused() {
    let p = check_problems(&yaml("container", "\"registry.npmjs.org\""));
    assert!(
        p.iter().any(|(at, m)| at == "isolation_policy.network.allow" && m.contains("api.anthropic.com")),
        "{p:#?}"
    );
    let p = check_problems(&yaml("container", "\"api.anthropic.com\""));
    assert!(!p.iter().any(|(at, _)| at == "isolation_policy.network.allow"), "{p:#?}");
}

/// `none` cannot enforce it; a malformed entry is refused at its index.
#[test]
fn an_unenforceable_or_malformed_allow_list_is_refused() {
    let p = check_problems(&yaml("none", "\"api.anthropic.com\""));
    assert!(p.iter().any(|(at, m)| at == "defaults.isolation" && m.contains("allow-list")), "{p:#?}");
    let p = check_problems(&yaml("container", "\"api.anthropic.com\", \"https://x.example/\""));
    assert!(p.iter().any(|(at, _)| at == "isolation_policy.network.allow[1]"), "{p:#?}");
    if !cfg!(target_os = "macos") {
        let p = check_problems(&yaml("sandbox", "\"api.anthropic.com\""));
        assert!(p.iter().any(|(at, m)| at == "defaults.isolation" && m.contains("container")), "{p:#?}");
    }
}
