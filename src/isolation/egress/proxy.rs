//! The egress PROXY: a small threaded HTTP proxy in the agg process that every confined process of
//! an allow-listed run must go through — the only network destination its jail leaves open.
//!
//! It speaks the two forms clients use with `HTTPS_PROXY`/`HTTP_PROXY`: `CONNECT host:port` (TLS,
//! tunnelled opaquely) and an absolute-URI request (`GET http://host/path`, forwarded). Each is
//! checked against the allow-list BY THE NAME THE CLIENT ASKED FOR, before anything is resolved:
//!
//! - a name not on the list is refused (`403`) without a DNS lookup — so a lookup cannot leak it;
//! - an IP literal is allowed only by an IP entry; a name entry never admits an address;
//! - an allowed NAME is resolved HERE, and an address it resolves to that is loopback, link-local
//!   (cloud metadata), unspecified or multicast is refused — a DNS-rebinding answer cannot turn an
//!   allowed name into a door to the host or the metadata service. The proxy connects to the
//!   address it resolved itself, so a later re-resolution elsewhere changes nothing.
//!
//! Sync and std-only by design (agg has no async runtime): one accept thread, two threads per
//! tunnel. Bounded where it matters — a 16 KiB request head read within 30 s, a 10 s connect.

use super::allow::AllowList;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HEAD_MAX: usize = 16 * 1024;
/// The name [`super::bridge`]'s readiness probe asks for — on no allow-list, so always a `403`.
pub(super) const SELFTEST_HOST: &str = "agg-egress-selftest.invalid";
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A running proxy listener. Dropping it stops accepting (tunnels already open run to their end).
pub struct Proxy {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Proxy {
    /// Listen on `bind` (port 0 = any free port) and serve `allow` until dropped.
    pub fn start(bind: SocketAddr, allow: Arc<AllowList>) -> io::Result<Proxy> {
        let listener = TcpListener::bind(bind)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new().name("agg-egress".into()).spawn(move || {
            for conn in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(client) = conn else { continue };
                let allow = allow.clone();
                let _ = std::thread::Builder::new().name("agg-egress-conn".into()).spawn(move || serve(client, &allow));
            }
        })?;
        Ok(Proxy { addr, stop })
    }

    /// Where it listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // unblock the accept loop so it sees the flag
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

/// One client connection: read the request head, decide, then tunnel or refuse.
fn serve(mut client: TcpStream, allow: &AllowList) {
    let _ = client.set_read_timeout(Some(HEAD_TIMEOUT));
    let Ok((head, rest)) = read_head(&mut client) else { return };
    let req = match Request::parse(&head) {
        Ok(r) => r,
        Err(why) => return refuse(&mut client, 400, &why),
    };
    let upstream = match dial(allow, &req.host, req.port) {
        Ok(u) => u,
        Err(why) => {
            // the relay's readiness probe asks for this name on purpose; it is not an event
            if req.host != SELFTEST_HOST {
                eprintln!("  [egress] DENIED {} {}:{} — {why}", req.method, req.host, req.port);
            }
            return refuse(&mut client, 403, &format!("agg egress: {}:{} — {why}", req.host, req.port));
        }
    };
    // the audit trail: every destination a confined process actually reached
    eprintln!("  [egress] allowed {} {}:{}", req.method, req.host, req.port);
    let _ = client.set_read_timeout(None);
    let mut upstream = upstream;
    let sent = if req.method == "CONNECT" {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
    } else {
        upstream.write_all(&req.forward_head(&head))
    };
    if sent.is_err() || (!rest.is_empty() && upstream.write_all(&rest).is_err()) {
        return;
    }
    splice(client, upstream);
}

/// The parts of a request line the proxy decides on.
struct Request {
    method: String,
    host: String,
    port: u16,
    /// for a forwarded request: the origin-form target (`/path?q`).
    path: String,
}

impl Request {
    fn parse(head: &[u8]) -> Result<Request, String> {
        let text = String::from_utf8_lossy(head);
        let line = text.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let (method, target) = match (parts.next(), parts.next()) {
            (Some(m), Some(t)) => (m.to_string(), t.to_string()),
            _ => return Err("not an HTTP request".into()),
        };
        if method == "CONNECT" {
            let (host, port) = host_port(&target, 443)?;
            return Ok(Request { method, host, port, path: String::new() });
        }
        let rest = target.strip_prefix("http://").ok_or("only CONNECT and absolute http:// requests are proxied")?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (host, port) = host_port(authority, 80)?;
        Ok(Request { method, host, port, path })
    }

    /// The head to send upstream for a forwarded request: origin-form request line, no `Proxy-*`.
    fn forward_head(&self, head: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(head);
        let mut lines = text.split("\r\n");
        let first = lines.next().unwrap_or("");
        let version = first.split_whitespace().nth(2).unwrap_or("HTTP/1.1");
        let mut out = format!("{} {} {version}\r\n", self.method, self.path);
        for l in lines.filter(|l| !l.to_ascii_lowercase().starts_with("proxy-")) {
            out.push_str(l);
            out.push_str("\r\n");
        }
        // `split` left the head's own terminator as trailing empty items; normalise to exactly one
        while out.ends_with("\r\n\r\n\r\n") {
            out.truncate(out.len() - 2);
        }
        out.into_bytes()
    }
}

/// `host[:port]` / `[v6][:port]` → (host, port).
fn host_port(s: &str, default: u16) -> Result<(String, u16), String> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').ok_or("bad IPv6 authority")?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("bad port `{p}`"))?,
            None => default,
        };
        return Ok((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Ok((h.to_string(), p.parse().map_err(|_| format!("bad port `{p}`"))?)),
        None => Ok((s.to_string(), default)),
    }
}

/// Connect to `host:port` if — and only if — the allow-list says so (see the module doc).
pub(super) fn dial(allow: &AllowList, host: &str, port: u16) -> Result<TcpStream, String> {
    if !allow.allows(host, port) {
        return Err("not in the allow-list".into());
    }
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        // an IP entry admitted it explicitly — loopback included, because the operator wrote it
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => {
            let all: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|e| format!("cannot resolve: {e}"))?.collect();
            let ok: Vec<SocketAddr> = all.iter().copied().filter(|a| !forbidden(a.ip())).collect();
            if ok.is_empty() && !all.is_empty() {
                return Err(format!("resolves only to loopback/link-local addresses ({}) — refused", all[0].ip()));
            }
            ok
        }
    };
    let mut last = String::from("no address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(format!("cannot connect ({last})"))
}

/// An address an allowed NAME must never resolve to: the host itself, link-local (incl. the cloud
/// metadata service at 169.254.169.254), nothing, or a group.
fn forbidden(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_multicast() || a.is_broadcast(),
        IpAddr::V6(a) => a.is_loopback() || a.is_unspecified() || a.is_multicast() || (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Read up to and including the blank line ending the request head. Returns (head, bytes after it).
fn read_head(s: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return Ok((buf, rest));
        }
        if buf.len() > HEAD_MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
    }
}

fn refuse(client: &mut TcpStream, code: u16, why: &str) {
    let reason = if code == 403 { "Forbidden" } else { "Bad Request" };
    let body = format!("{why}\n");
    let _ = write!(
        client,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\nX-Agg-Egress: denied\r\n\r\n{body}",
        body.len()
    );
}

/// Copy both ways until either side closes.
fn splice(client: TcpStream, upstream: TcpStream) {
    let (Ok(mut c_read), Ok(mut u_read)) = (client.try_clone(), upstream.try_clone()) else { return };
    let mut u_write = upstream;
    let mut c_write = client;
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut c_read, &mut u_write);
        let _ = u_write.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut u_read, &mut c_write);
    let _ = c_write.shutdown(Shutdown::Write);
    let _ = up.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    /// A one-shot HTTP server on loopback answering every request with `body`.
    fn server(body: &'static str) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let mut r = io::BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let mut c = c;
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        a
    }

    fn proxy(entries: &[String]) -> Proxy {
        let allow = AllowList::parse(entries).unwrap();
        Proxy::start("127.0.0.1:0".parse().unwrap(), Arc::new(allow)).unwrap()
    }

    fn ask(p: &Proxy, req: &str) -> String {
        let mut s = TcpStream::connect(p.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    /// CONNECT to an allowed address tunnels; the bytes after `200` are the origin's.
    #[test]
    fn an_allowed_connect_is_tunnelled() {
        let srv = server("hello-through-tunnel");
        let p = proxy(&[format!("127.0.0.1:{}", srv.port())]);
        let out = ask(&p, &format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: x\r\n\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n", srv.port()));
        assert!(out.starts_with("HTTP/1.1 200 Connection Established"), "{out}");
        assert!(out.contains("hello-through-tunnel"), "{out}");
    }

    /// Plain HTTP is forwarded in origin form, without the client's `Proxy-*` headers.
    #[test]
    fn an_allowed_plain_request_is_forwarded() {
        let srv = server("plain-ok");
        let p = proxy(&[format!("127.0.0.1:{}", srv.port())]);
        let out = ask(&p, &format!("GET http://127.0.0.1:{}/x HTTP/1.1\r\nHost: x\r\nProxy-Authorization: y\r\n\r\n", srv.port()));
        assert!(out.contains("plain-ok"), "{out}");
    }

    /// The adversarial cases the proxy itself decides: an address not listed (direct IP), a
    /// resolver not listed (DNS-over-HTTPS), a listed NAME that resolves to loopback (rebinding),
    /// and a request that is not a proxy request at all.
    #[test]
    fn everything_else_is_refused_before_a_connection_is_made() {
        let srv = server("must-not-be-seen");
        let p = proxy(&["api.example.com".to_string(), format!("localhost:{}", srv.port())]);
        for (req, why) in [
            (format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", srv.port()), "an address no IP entry names"),
            ("CONNECT 1.1.1.1:443 HTTP/1.1\r\n\r\n".to_string(), "a direct IP"),
            ("CONNECT dns.google:443 HTTP/1.1\r\n\r\n".to_string(), "a DoH resolver"),
            (format!("CONNECT localhost:{} HTTP/1.1\r\n\r\n", srv.port()), "a name that is loopback"),
        ] {
            let out = ask(&p, &req);
            assert!(out.starts_with("HTTP/1.1 403"), "{why}: {out}");
            assert!(!out.contains("must-not-be-seen"), "{why}: {out}");
        }
        assert!(ask(&p, "GET /relative HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn a_name_resolving_to_loopback_or_metadata_is_forbidden() {
        for ip in ["127.0.0.1", "169.254.169.254", "0.0.0.0", "::1", "fe80::1", "::ffff:127.0.0.1", "224.0.0.1"] {
            assert!(forbidden(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["10.0.0.1", "93.184.215.14", "2001:db8::1"] {
            assert!(!forbidden(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn the_request_line_is_parsed_for_both_forms() {
        let r = Request::parse(b"CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((r.host.as_str(), r.port), ("2001:db8::1", 8443));
        let r = Request::parse(b"GET http://h.example/a?b HTTP/1.0\r\n\r\n").unwrap();
        assert_eq!((r.host.as_str(), r.port, r.path.as_str()), ("h.example", 80, "/a?b"));
        let fwd = String::from_utf8(r.forward_head(b"GET http://h.example/a?b HTTP/1.0\r\nHost: h\r\nProxy-Connection: x\r\n\r\n")).unwrap();
        assert_eq!(fwd, "GET /a?b HTTP/1.0\r\nHost: h\r\n\r\n");
        assert!(Request::parse(b"GET https://h/ HTTP/1.1\r\n\r\n").is_err());
    }
}
