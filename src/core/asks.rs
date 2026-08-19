//! The human-ask ledger — one question, one answer, correlated by id.
//!
//! `internal/HUMAN_LOOP.md` §4.2. An **ask** is a durable request for one value from a human; an
//! **answer** is a bus command carrying that value. This module owns both halves of the record and
//! nothing else: no delivery (that is `notify.cmd`), no waiting (that is the driver), no policy.
//!
//! # The trust asymmetry IS the moat
//!
//! | | channel | trust |
//! |---|---|---|
//! | request | a driver call site, or the worker via `agg hil` → `agg/state/asks/` | **untrusted** — a worker-authored question is text, exactly like `BLOCKED.md`'s rationale |
//! | answer | `agg send answer` → `agg/private/bus/in/` | **trusted** — the bus is carved out of the worker's writable set under `sandbox`/`container` |
//!
//! That is why the ledger lives in `private/` while worker requests land in `state/`: a worker must
//! not be able to answer its own question, or "a human approved the prod deploy" means nothing.
//! Under `isolation: none` this is a protocol boundary rather than a kernel one — the same honest
//! caveat that applies to `verdicts.jsonl`.
//!
//! # Shape
//!
//! Append-only JSONL, like `verdicts.jsonl`: one row per state transition, and the live view is a
//! fold over the file. Never rewritten in place, so a crash mid-answer can lose at most the row it
//! was writing, and an auditor can see the whole history of who was asked what.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Which `hil_*` call opened the ask. **Derived from the function that was called** — never chosen
/// by a caller and never inferred by agg. It exists so a reader knows whether to render buttons or a
/// text field, and so an answer can be validated against a closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AskCase {
    /// `hil_bool` — sugar for a two-option `Choose`.
    Bool,
    /// `hil_choose` — a CLOSED answer set. The options are recorded, so `agg send answer` can refuse
    /// anything not on the list and the caller cannot be handed a value it did not offer.
    Choose,
    /// `hil_input` — an OPEN set. Pair it with a judge; agg cannot validate free text.
    Input,
}

/// Who opened the ask. Decides nothing about trust (the *answer's* channel does that) but a reader
/// wants to know, and a worker that pages a human every session is visible in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Driver,
    Worker,
}

/// One row. `Open` rows carry the question; `Answered` rows carry the answer. Both are appended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Row {
    Open {
        id: String,
        case: AskCase,
        question: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<Vec<String>>,
        origin: Origin,
        session: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
        ts: u64,
    },
    Answered {
        id: String,
        answer: String,
        by: String,
        ts: u64,
    },
}

impl Row {
    pub fn id(&self) -> &str {
        match self {
            Row::Open { id, .. } | Row::Answered { id, .. } => id,
        }
    }
}

/// The folded view of one ask — what a reader and a driver see.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ask {
    pub id: String,
    pub case: AskCase,
    pub question: String,
    pub options: Option<Vec<String>>,
    pub origin: Origin,
    pub session: u32,
    pub step: Option<String>,
    pub opened_at_epoch: u64,
    /// `None` while open.
    pub answer: Option<String>,
    pub answered_at_epoch: Option<u64>,
    pub by: Option<String>,
}

impl Ask {
    pub fn is_open(&self) -> bool {
        self.answer.is_none()
    }
    /// How long this ask has been outstanding. `0` once answered — an answered ask is not waiting.
    pub fn age_secs(&self, now_epoch: u64) -> u64 {
        match self.answered_at_epoch {
            Some(_) => 0,
            None => now_epoch.saturating_sub(self.opened_at_epoch),
        }
    }
}

/// Append one row. Errors propagate: an ask that was not recorded must not be waited on, because
/// nothing could ever answer it.
pub fn append(dir: &Path, row: &Row) -> std::io::Result<()> {
    use std::io::Write;
    let path = crate::paths::asks_jsonl(dir);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let line = serde_json::to_string(row)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")
}

/// Every ask in the ledger, oldest first, folded from its rows.
///
/// A malformed line is SKIPPED rather than fatal: the ledger is append-only and a torn final line
/// (a crash mid-write) must not make every earlier ask unreadable.
pub fn all(dir: &Path) -> Vec<Ask> {
    let text = match std::fs::read_to_string(crate::paths::asks_jsonl(dir)) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<Ask> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Row>(line) {
            Ok(Row::Open { id, case, question, options, origin, session, step, ts }) => {
                // A duplicate `Open` for a known id is ignored — ids are agg-minted and a repeat
                // would mean a replay, not a new question.
                if !out.iter().any(|a| a.id == id) {
                    out.push(Ask {
                        id,
                        case,
                        question,
                        options,
                        origin,
                        session,
                        step,
                        opened_at_epoch: ts,
                        answer: None,
                        answered_at_epoch: None,
                        by: None,
                    });
                }
            }
            Ok(Row::Answered { id, answer, by, ts }) => {
                if let Some(a) = out.iter_mut().find(|a| a.id == id) {
                    // FIRST answer wins. A second one is not an amendment: the driver may already
                    // have acted on the first, so silently changing it would rewrite history the run
                    // has already consumed.
                    if a.answer.is_none() {
                        a.answer = Some(answer);
                        a.answered_at_epoch = Some(ts);
                        a.by = Some(by);
                    }
                }
            }
            Err(_) => continue,
        }
    }
    out
}

/// The asks still waiting on a human, oldest first.
pub fn open(dir: &Path) -> Vec<Ask> {
    all(dir).into_iter().filter(Ask::is_open).collect()
}

/// One ask by id.
pub fn get(dir: &Path, id: &str) -> Option<Ask> {
    all(dir).into_iter().find(|a| a.id == id)
}

/// Record an answer. Returns the ask as it now stands.
///
/// Validation lives at the CLI (it can print the options and refuse), not here: this is the write
/// path both the CLI and the loop share, and a second validator that disagreed with the first would
/// be worse than none.
pub fn answer(dir: &Path, id: &str, answer: &str, by: &str, now_epoch: u64) -> std::io::Result<()> {
    append(
        dir,
        &Row::Answered { id: id.to_string(), answer: answer.to_string(), by: by.to_string(), ts: now_epoch },
    )
}

/// A request the WORKER wrote, before agg has promoted it into the ledger.
///
/// Deliberately not an [`Ask`]: it has no id, no session and no state, because those are agg's to
/// assign. A worker that could mint its own ask id could collide with a real one and answer it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub case: AskCase,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub ts: u64,
}

/// Write a worker request to `agg/state/asks/`. Returns the file written.
///
/// The filename carries the epoch so promotion happens in ask order. Written via tmp+rename so the
/// loop never promotes a half-written request.
pub fn write_worker_request(dir: &Path, req: &WorkerRequest) -> std::io::Result<std::path::PathBuf> {
    let d = crate::paths::worker_asks_dir(dir);
    std::fs::create_dir_all(&d)?;
    // A pid+nanos suffix so two asks in the same second cannot overwrite each other.
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = d.join(format!("{:013}-{}-{}.json", req.ts, std::process::id(), uniq));
    let body = serde_json::to_string_pretty(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::util::write_atomic(&path, &body)?;
    Ok(path)
}

/// Promote every pending worker request into the ledger, oldest first. Returns the new asks.
///
/// Called at the session boundary. Each request is CONSUMED (the file is removed) once its `Open`
/// row is durable, so a request cannot be promoted twice; a request that fails to promote is left
/// in place for the next boundary rather than dropped.
pub fn promote_worker_requests(dir: &Path, session: u32, now_epoch: u64) -> Vec<Ask> {
    let d = crate::paths::worker_asks_dir(dir);
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(&d) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();

    let mut minted: Vec<Ask> = Vec::new();
    for f in files {
        let Some(req) = std::fs::read_to_string(&f)
            .ok()
            .and_then(|t| serde_json::from_str::<WorkerRequest>(&t).ok())
        else {
            // Unparseable: the worker wrote garbage into its own directory. Drop it loudly rather
            // than retry it every session forever.
            eprintln!("  ⚠ discarding unreadable worker ask request {}", f.display());
            let _ = std::fs::remove_file(&f);
            continue;
        };
        let taken = all(dir);
        let id = (0..u64::MAX)
            .map(|seq| mint_id(seq, now_epoch))
            .find(|id| !taken.iter().any(|a| &a.id == id))
            .unwrap_or_else(|| format!("{now_epoch:x}"));
        let row = Row::Open {
            id: id.clone(),
            case: req.case,
            question: req.question.clone(),
            options: req.options.clone(),
            origin: Origin::Worker,
            session,
            step: None,
            ts: now_epoch,
        };
        match append(dir, &row) {
            Ok(()) => {
                let _ = std::fs::remove_file(&f);
                if let Some(a) = get(dir, &id) {
                    minted.push(a);
                }
            }
            // Left in place ON PURPOSE: an ask that is not in the ledger cannot be answered, so
            // losing the request would silently drop the worker's only channel to a human.
            Err(e) => eprintln!("  ⚠ could not promote worker ask {}: {e}", f.display()),
        }
    }
    minted
}

/// The answered worker asks a session has not yet been told about, and the marker to advance.
///
/// "Not yet told" is decided by `since_epoch` rather than by mutating the ledger: the ledger is
/// append-only, and a read-model that needs no write cannot corrupt it.
pub fn answers_for_worker(dir: &Path, since_epoch: u64) -> Vec<Ask> {
    all(dir)
        .into_iter()
        .filter(|a| matches!(a.origin, Origin::Worker))
        .filter(|a| a.answered_at_epoch.is_some_and(|t| t >= since_epoch))
        .collect()
}

/// Mint an id: short, sortable-ish, and unique per process even inside one second.
///
/// Not a UUID — an operator retypes these into `agg send answer <id>`, so four hex characters that a
/// human can read off a phone beats a canonical identifier nobody will ever type twice. Collisions
/// are checked against the ledger by the caller.
pub fn mint_id(seq: u64, now_epoch: u64) -> String {
    format!("{:x}{:x}", (now_epoch & 0xfff) as u16, (seq & 0xff) as u8)
}

/// The one-line summary a `{{reason}}` / notification carries. Normalised the same way
/// `notify.rs` normalises a worker-authored rationale: the question may be worker-authored, and
/// every sink it reaches (ntfy, syslog, a phone) is line-oriented.
pub fn one_line(question: &str, cap: usize) -> String {
    let flat: String = question
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    crate::util::truncate(&flat, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_row(id: &str, q: &str, ts: u64) -> Row {
        Row::Open {
            id: id.into(),
            case: AskCase::Bool,
            question: q.into(),
            options: None,
            origin: Origin::Driver,
            session: 1,
            step: None,
            ts,
        }
    }

    #[test]
    fn a_ledger_folds_open_then_answered_into_one_ask() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &open_row("a1", "Deploy to prod?", 100)).unwrap();
        assert_eq!(open(d.path()).len(), 1, "an unanswered ask is open");
        assert_eq!(open(d.path())[0].age_secs(160), 60);

        answer(d.path(), "a1", "yes", "operator", 150).unwrap();
        assert!(open(d.path()).is_empty(), "an answered ask is no longer open");
        let a = get(d.path(), "a1").unwrap();
        assert_eq!(a.answer.as_deref(), Some("yes"));
        assert_eq!(a.age_secs(9_999), 0, "an answered ask is not waiting");
    }

    /// The driver may already have acted on the first answer, so a later row must not rewrite it.
    #[test]
    fn the_first_answer_wins() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &open_row("b2", "Which store?", 10)).unwrap();
        answer(d.path(), "b2", "postgres", "operator", 20).unwrap();
        answer(d.path(), "b2", "sqlite", "someone-else", 30).unwrap();
        assert_eq!(get(d.path(), "b2").unwrap().answer.as_deref(), Some("postgres"));
    }

    /// A torn final line (crash mid-write) must not hide every earlier ask.
    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), &open_row("c3", "ok?", 1)).unwrap();
        let p = crate::paths::asks_jsonl(d.path());
        let mut s = std::fs::read_to_string(&p).unwrap();
        s.push_str("{\"state\":\"open\",\"id\":\"tor");
        std::fs::write(&p, s).unwrap();
        assert_eq!(all(d.path()).len(), 1, "the good row survives a torn tail");
    }

    /// An answer for an id that was never opened is inert — it cannot conjure an ask into being.
    #[test]
    fn an_answer_without_an_open_row_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        answer(d.path(), "ghost", "yes", "operator", 5).unwrap();
        assert!(all(d.path()).is_empty());
    }

    #[test]
    fn one_line_flattens_and_caps_a_worker_authored_question() {
        let q = "line one\nline\ttwo   with   spaces";
        assert_eq!(one_line(q, 400), "line one line two with spaces");
        // `util::truncate` appends `…` when it shortens, so the bound is cap + 1 char — the point
        // is that a 1000-char worker-authored question cannot repaint an operator's phone.
        assert_eq!(one_line(&"x".repeat(1000), 40).chars().count(), 41);
    }
}
