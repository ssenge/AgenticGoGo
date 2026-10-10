//! The egress ALLOW-LIST: `host[:port]` entries, parsed once and matched by the proxy.
//!
//! | entry | matches |
//! |---|---|
//! | `api.example.com` | that host name, any port (case-insensitive, a trailing `.` ignored) |
//! | `api.example.com:443` | that host name on port 443 only |
//! | `*.example.com` | every SUBdomain of `example.com` (not `example.com` itself), any port |
//! | `10.1.2.3[:p]` / `[2001:db8::1][:p]` | that literal IP address |
//! | `localhost[:p]` | the worker's own loopback — what a sandboxed worker may reach DIRECTLY |
//!
//! No CIDR ranges, no URLs, no paths: an entry that is not one of the shapes above is refused at
//! startup rather than read as something narrower or wider than it says.

use std::net::IpAddr;

/// One parsed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub host: HostPat,
    /// `None` = any port.
    pub port: Option<u16>,
}

/// What an entry's host part matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPat {
    /// a DNS name, lowercased, without a trailing dot.
    Name(String),
    /// `*.suffix` — any name ending in `.suffix`.
    Suffix(String),
    /// a literal address.
    Ip(IpAddr),
    /// `localhost` — the worker's own loopback, never reached through the proxy.
    Localhost,
}

/// The parsed allow-list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowList {
    pub rules: Vec<Rule>,
}

/// Which loopback ports a SANDBOXED worker may reach directly — only what `localhost[:p]` entries
/// grant (the proxy's own port is always added by the profile).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loopback {
    None,
    Ports(Vec<u16>),
    Any,
}

impl AllowList {
    /// Parse every entry; `Err` lists `(index, why)` for each one that is not an entry.
    pub fn parse(entries: &[String]) -> Result<AllowList, Vec<(usize, String)>> {
        let mut rules = Vec::new();
        let mut bad = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            match parse_rule(e.trim()) {
                Ok(r) => rules.push(r),
                Err(why) => bad.push((i, why)),
            }
        }
        if bad.is_empty() {
            Ok(AllowList { rules })
        } else {
            Err(bad)
        }
    }

    /// May the proxy connect to `host:port`? `host` is a name or an IP literal, as a client asked.
    /// A `localhost` entry never matches here: the proxy never connects to loopback for a name.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let target = Target::of(host);
        self.rules.iter().any(|r| r.port.is_none_or(|p| p == port) && r.host.matches(&target))
    }

    /// Is `host` (a name, any port) allowed — the startup check that an agent's endpoint is listed.
    pub fn allows_host(&self, host: &str) -> bool {
        let target = Target::of(host);
        self.rules.iter().any(|r| r.host.matches(&target))
    }

    /// The direct loopback access a sandboxed worker gets from `localhost` entries.
    pub fn loopback(&self) -> Loopback {
        let mut ports = Vec::new();
        for r in self.rules.iter().filter(|r| r.host == HostPat::Localhost) {
            match r.port {
                None => return Loopback::Any,
                Some(p) if !ports.contains(&p) => ports.push(p),
                Some(_) => {}
            }
        }
        if ports.is_empty() {
            Loopback::None
        } else {
            Loopback::Ports(ports)
        }
    }
}

/// A client's requested host, normalised once.
enum Target {
    Name(String),
    Ip(IpAddr),
}

impl Target {
    fn of(host: &str) -> Target {
        let h = host.trim().trim_start_matches('[').trim_end_matches(']');
        match h.parse::<IpAddr>() {
            Ok(ip) => Target::Ip(ip),
            Err(_) => Target::Name(h.trim_end_matches('.').to_ascii_lowercase()),
        }
    }
}

impl HostPat {
    fn matches(&self, t: &Target) -> bool {
        match (self, t) {
            (HostPat::Name(n), Target::Name(h)) => n == h,
            (HostPat::Suffix(s), Target::Name(h)) => h.len() > s.len() + 1 && h.ends_with(s.as_str()) && h.as_bytes()[h.len() - s.len() - 1] == b'.',
            (HostPat::Ip(a), Target::Ip(b)) => a == b,
            _ => false,
        }
    }
}

/// One entry → a rule, or why it is not one.
fn parse_rule(e: &str) -> Result<Rule, String> {
    if e.is_empty() {
        return Err("an empty entry".into());
    }
    if e.contains("://") || e.contains('/') {
        return Err(format!("`{e}` is not `host[:port]` — no scheme, path or CIDR range; name the host"));
    }
    let (host, port) = split_port(e)?;
    let pat = if host.eq_ignore_ascii_case("localhost") {
        HostPat::Localhost
    } else if let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        HostPat::Ip(ip)
    } else if let Some(suffix) = host.strip_prefix("*.") {
        valid_name(suffix).map_err(|w| format!("`{e}`: {w}"))?;
        HostPat::Suffix(suffix.trim_end_matches('.').to_ascii_lowercase())
    } else {
        valid_name(host).map_err(|w| format!("`{e}`: {w}"))?;
        HostPat::Name(host.trim_end_matches('.').to_ascii_lowercase())
    };
    Ok(Rule { host: pat, port })
}

/// `host`, `host:port`, `[v6]`, `[v6]:port`.
fn split_port(e: &str) -> Result<(&str, Option<u16>), String> {
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("`{e}`: `{p}` is not a port (1-65535)"));
    if let Some(rest) = e.strip_prefix('[') {
        let (addr, tail) = rest.split_once(']').ok_or_else(|| format!("`{e}`: an unclosed `[`"))?;
        return match tail.strip_prefix(':') {
            Some(p) => Ok((&e[..addr.len() + 2], Some(port(p)?))),
            None if tail.is_empty() => Ok((e, None)),
            None => Err(format!("`{e}`: unexpected text after `]`")),
        };
    }
    match e.matches(':').count() {
        0 => Ok((e, None)),
        1 => {
            let (h, p) = e.split_once(':').expect("one colon");
            Ok((h, Some(port(p)?)))
        }
        // a bare IPv6 literal has no room for a port; it must be bracketed to carry one
        _ if e.parse::<IpAddr>().is_ok() => Ok((e, None)),
        _ => Err(format!("`{e}` is not `host[:port]` (an IPv6 address with a port is `[addr]:port`)")),
    }
}

/// A DNS name: labels of letters, digits and `-`, separated by dots. `*` only as `*.` up front.
fn valid_name(n: &str) -> Result<(), String> {
    let n = n.trim_end_matches('.');
    let ok = !n.is_empty()
        && n.len() <= 253
        && n.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !l.starts_with('-'));
    if ok {
        Ok(())
    } else {
        Err("not a host name (a wildcard is only `*.` at the front)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> AllowList {
        AllowList::parse(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>()).expect("valid")
    }

    #[test]
    fn a_name_matches_itself_on_any_port_or_only_its_own() {
        let l = list(&["API.Anthropic.com", "mirror.example:8443"]);
        assert!(l.allows("api.anthropic.com", 443) && l.allows("api.anthropic.com.", 80));
        assert!(l.allows("mirror.example", 8443));
        assert!(!l.allows("mirror.example", 443), "the port is part of the entry");
        assert!(!l.allows("evil-api.anthropic.com", 443) && !l.allows("anthropic.com", 443));
    }

    /// `*.x` is SUBdomains only — and not a suffix match on the string (`evilx` is not `.x`).
    #[test]
    fn a_wildcard_matches_subdomains_only() {
        let l = list(&["*.npmjs.org"]);
        assert!(l.allows("registry.npmjs.org", 443) && l.allows("a.b.npmjs.org", 443));
        assert!(!l.allows("npmjs.org", 443), "not the apex");
        assert!(!l.allows("evilnpmjs.org", 443), "a label boundary, not a string suffix");
    }

    /// An IP is matched only by an IP entry — a name entry never lets a client in by address.
    #[test]
    fn an_ip_is_matched_only_by_an_ip_entry() {
        let l = list(&["10.0.0.5", "[2001:db8::1]:443", "2001:db8::2", "api.example.com"]);
        assert!(l.allows("10.0.0.5", 22));
        assert!(l.allows("[2001:db8::1]", 443) && !l.allows("2001:db8::1", 80));
        assert!(l.allows("2001:db8::2", 1));
        assert!(!l.allows("93.184.215.14", 443), "an address the list does not name");
    }

    #[test]
    fn localhost_is_a_direct_grant_never_a_proxy_destination() {
        let l = list(&["localhost:5432", "localhost:8080"]);
        assert_eq!(l.loopback(), Loopback::Ports(vec![5432, 8080]));
        assert!(!l.allows("localhost", 5432), "the proxy never connects to loopback for a name");
        assert_eq!(list(&["localhost"]).loopback(), Loopback::Any);
        assert_eq!(list(&["api.x.com"]).loopback(), Loopback::None);
    }

    #[test]
    fn every_malformed_entry_is_refused_with_its_index() {
        let bad = ["", "https://api.example.com", "10.0.0.0/8", "host:0", "host:99999", "ho st", "a.*.com", "[::1", "x:1:2"];
        let entries: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
        let errs = AllowList::parse(&entries).unwrap_err();
        assert_eq!(errs.iter().map(|(i, _)| *i).collect::<Vec<_>>(), (0..bad.len()).collect::<Vec<_>>(), "{errs:#?}");
    }

    #[test]
    fn allows_host_ignores_ports() {
        let l = list(&["api.openai.com:443"]);
        assert!(l.allows_host("api.openai.com") && !l.allows_host("chatgpt.com"));
    }
}
