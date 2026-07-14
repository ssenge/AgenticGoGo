//! `agg serve` — a thin JSON HTTP API over a project's `agg/state/` state, for the standalone web UI.
//!
//! This is deliberately UI-free: no HTML, no embedded assets. It exposes exactly the read/control
//! surface `agg dashboard` + `agg send` already use (state.json, project.json, run.pid liveness,
//! the bus), so a separate web tool (the SvelteKit app in `web/`) can monitor and steer a running
//! loop over HTTP — locally now, remotely later. The loop keeps running whether or not this is up.
//!
//! Endpoints:
//!   GET  /api/state    → the live DashboardState (204 when no snapshot exists yet)
//!   GET  /api/history  → the Project run-history ledger
//!   GET  /api/health   → { running: bool, pid: number|null }
//!   POST /api/send     → { cmd: "pause"|"resume"|"stop"|"inject"|"budget", ... } → the bus
//!                        (409 if no loop is running here)
//!
//! Sync server (tiny_http) — a JSON API for one local reader doesn't need async. CORS is locked to
//! the configured web origin (not `*`). An `auth` seam is present but a no-op for local use; it
//! becomes a bearer-token check when the tool is exposed remotely.

use crate::bus::{self, Command};
use crate::state::DashboardState;
use anyhow::Result;
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Response, Server};

/// Config for a serve session.
pub struct ServeConfig {
    pub dir: PathBuf,
    pub port: u16,
    /// Allowed CORS origin for the web tool (the SvelteKit dev server / deployed origin). When
    /// empty, defaults to the SvelteKit dev default so `npm run dev` just works.
    pub cors_origin: String,
    /// Optional bearer token. When set, every /api request must send `Authorization: Bearer <t>`.
    /// Empty = no auth (the local default). This is the remote-ready seam.
    pub token: String,
}

pub fn run(cfg: ServeConfig) -> Result<()> {
    let origin = if cfg.cors_origin.is_empty() {
        "http://localhost:5173".to_string() // SvelteKit `npm run dev` default
    } else {
        cfg.cors_origin.clone()
    };
    let server = Server::http(("127.0.0.1", cfg.port))
        .map_err(|e| anyhow::anyhow!("could not bind agg serve on 127.0.0.1:{}: {e}", cfg.port))?;
    eprintln!(
        "agg serve → http://127.0.0.1:{}  (project: {})\n  \
         endpoints: /api/state  /api/history  /api/health  POST /api/send\n  \
         CORS origin: {}{}\n  Ctrl-C to stop.",
        cfg.port,
        cfg.dir.display(),
        origin,
        if cfg.token.is_empty() { "  (no auth — local mode)" } else { "  (bearer-token auth ON)" },
    );

    for mut req in server.incoming_requests() {
        let method = req.method().clone();
        let url = req.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();

        // CORS preflight — answer before auth so browsers can probe.
        if method == Method::Options {
            let _ = req.respond(cors_preflight(&origin));
            continue;
        }

        // Auth seam: no-op when token is empty; a bearer check otherwise.
        if !cfg.token.is_empty() && !authorized(&req, &cfg.token) {
            let _ = req.respond(json_resp(401, r#"{"error":"unauthorized"}"#, &origin));
            continue;
        }

        let resp = match (&method, path.as_str()) {
            (Method::Get, "/api/state") => handle_state(&cfg.dir, &origin),
            (Method::Get, "/api/history") => handle_history(&cfg.dir, &origin),
            (Method::Get, "/api/health") => handle_health(&cfg.dir, &origin),
            (Method::Post, "/api/send") => handle_send(&mut req, &cfg.dir, &origin),
            _ => json_resp(404, r#"{"error":"not found"}"#, &origin),
        };
        let _ = req.respond(resp);
    }
    Ok(())
}

// ---------------- handlers ----------------

fn handle_state(dir: &Path, origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    match DashboardState::read(dir) {
        Some(s) => match serde_json::to_string(&s) {
            Ok(body) => json_resp(200, &body, origin),
            Err(e) => json_resp(500, &err_json(&e.to_string()), origin),
        },
        // no snapshot yet (loop hasn't published) — 204 so the UI can show a "waiting" state.
        None => json_resp(204, r#"{"waiting":true}"#, origin),
    }
}

fn handle_history(dir: &Path, origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let proj = crate::project::Project::load(dir);
    match serde_json::to_string(&proj) {
        Ok(body) => json_resp(200, &body, origin),
        Err(e) => json_resp(500, &err_json(&e.to_string()), origin),
    }
}

fn handle_health(dir: &Path, origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let pid = crate::os::detach::live_pid(dir);
    let body = match pid {
        Some(p) => format!(r#"{{"running":true,"pid":{p}}}"#),
        None => r#"{"running":false,"pid":null}"#.to_string(),
    };
    json_resp(200, &body, origin)
}

fn handle_send(
    req: &mut tiny_http::Request,
    dir: &Path,
    origin: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut body = String::new();
    if std::io::Read::read_to_string(req.as_reader(), &mut body).is_err() {
        return json_resp(400, &err_json("could not read request body"), origin);
    }
    let cmd = match parse_send(&body) {
        Ok(c) => c,
        Err(e) => return json_resp(400, &err_json(&e), origin),
    };
    // Liveness guard: refuse (409) when no loop is running, so the UI never silently queues a
    // control action to a dead loop (a stop would then fire at the next run's startup).
    if crate::os::detach::live_pid(dir).is_none() {
        return json_resp(409, &err_json("no loop is running in this project"), origin);
    }
    match bus::queue_command(dir, &cmd) {
        Ok(_) => json_resp(200, r#"{"ok":true}"#, origin),
        Err(e) => json_resp(500, &err_json(&e.to_string()), origin),
    }
}

/// Map the web tool's `{cmd, ...}` body to a bus Command. Kept strict so a bad body is a clear 400.
fn parse_send(body: &str) -> std::result::Result<Command, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let cmd = v.get("cmd").and_then(|c| c.as_str()).ok_or("missing `cmd`")?;
    match cmd {
        "pause" => Ok(Command::Pause),
        "resume" => Ok(Command::Resume),
        "stop" => {
            let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("web").to_string();
            Ok(Command::Stop { reason })
        }
        "inject" => {
            let text = v
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("`inject` requires `text`")?
                .to_string();
            if text.trim().is_empty() {
                return Err("`inject` text must not be empty".into());
            }
            Ok(Command::InjectInstruction { text })
        }
        "budget" => {
            // total: number | null (null = unlimited)
            let total = match v.get("total") {
                None => return Err("`budget` requires `total` (number or null)".into()),
                Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Number(n)) => {
                    Some(n.as_u64().ok_or("`total` must be a non-negative integer")?)
                }
                Some(_) => return Err("`total` must be a number or null".into()),
            };
            Ok(Command::SetBudget { total })
        }
        "note" => {
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
            Ok(Command::Note { text })
        }
        other => Err(format!("unknown cmd `{other}` (expected pause/resume/stop/inject/budget/note)")),
    }
}

// ---------------- http helpers ----------------

fn authorized(req: &tiny_http::Request, token: &str) -> bool {
    let want = format!("Bearer {token}");
    req.headers()
        .iter()
        .any(|h| h.field.equiv("Authorization") && h.value.as_str() == want)
}

fn cors_headers(origin: &str) -> Vec<Header> {
    vec![
        Header::from_bytes(&b"Access-Control-Allow-Origin"[..], origin.as_bytes()).unwrap(),
        Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap(),
        Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type, Authorization"[..]).unwrap(),
        Header::from_bytes(&b"Vary"[..], &b"Origin"[..]).unwrap(),
    ]
}

fn cors_preflight(origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string("").with_status_code(204);
    for h in cors_headers(origin) {
        r.add_header(h);
    }
    r
}

fn json_resp(status: u16, body: &str, origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body).with_status_code(status);
    r.add_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    for h in cors_headers(origin) {
        r.add_header(h);
    }
    r
}

fn err_json(msg: &str) -> String {
    format!(r#"{{"error":{}}}"#, serde_json::to_string(msg).unwrap_or_else(|_| "\"error\"".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_send_maps_all_verbs() {
        assert!(matches!(parse_send(r#"{"cmd":"pause"}"#).unwrap(), Command::Pause));
        assert!(matches!(parse_send(r#"{"cmd":"resume"}"#).unwrap(), Command::Resume));
        assert!(matches!(parse_send(r#"{"cmd":"stop","reason":"done"}"#).unwrap(),
            Command::Stop { reason } if reason == "done"));
        assert!(matches!(parse_send(r#"{"cmd":"inject","text":"focus X"}"#).unwrap(),
            Command::InjectInstruction { text } if text == "focus X"));
        assert!(matches!(parse_send(r#"{"cmd":"budget","total":500000}"#).unwrap(),
            Command::SetBudget { total: Some(500000) }));
        assert!(matches!(parse_send(r#"{"cmd":"budget","total":null}"#).unwrap(),
            Command::SetBudget { total: None }));
    }

    #[test]
    fn parse_send_rejects_bad_input() {
        assert!(parse_send("not json").is_err());
        assert!(parse_send(r#"{"no_cmd":1}"#).is_err());
        assert!(parse_send(r#"{"cmd":"inject"}"#).is_err()); // missing text
        assert!(parse_send(r#"{"cmd":"inject","text":"  "}"#).is_err()); // empty text
        assert!(parse_send(r#"{"cmd":"budget"}"#).is_err()); // missing total
        assert!(parse_send(r#"{"cmd":"budget","total":"lots"}"#).is_err()); // wrong type
        assert!(parse_send(r#"{"cmd":"frobnicate"}"#).is_err()); // unknown
    }

    #[test]
    fn err_json_escapes() {
        let e = err_json("bad \"quote\" here");
        assert!(serde_json::from_str::<serde_json::Value>(&e).is_ok(), "err_json must be valid JSON: {e}");
    }
}
