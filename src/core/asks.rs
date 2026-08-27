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
    /// The answer has been handed to the worker in a session brief.
    ///
    /// Exists so an answer is delivered EXACTLY ONCE. Without it every answer ever given is
    /// re-injected into every future brief for the life of the project — the brief grows without
    /// bound and a worker re-reads decisions it acted on twenty sessions ago. A row rather than a
    /// mutation, because the ledger is append-only and "when was the worker told" is worth auditing.
    Delivered { id: String, ts: u64 },
}

impl Row {
    pub fn id(&self) -> &str {
        match self {
            Row::Open { id, .. } | Row::Answered { id, .. } | Row::Delivered { id, .. } => id,
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
    /// the worker has been shown this answer in a brief
    #[serde(default)]
    pub delivered: bool,
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
                        delivered: false,
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
            Ok(Row::Delivered { id, .. }) => {
                if let Some(a) = out.iter_mut().find(|a| a.id == id) {
                    a.delivered = true;
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

/// The most worker-authored asks that may be open at once.
///
/// A worker cannot stop the loop, so the worst it can do with this channel is NAG — and a worker that
/// asks on every session pages a human on every session, because a driver-level ping does not go
/// through `notify.cooldown_sessions`. This is the cap on that. It is deliberately generous: it
/// exists to bound a runaway, not to ration a legitimate question.
const MAX_OPEN_WORKER_ASKS: usize = 5;

/// Promote every pending worker request into the ledger, oldest first. Returns the new asks.
///
/// Called at the session boundary. Each request is CONSUMED (the file is removed) once its `Open`
/// row is durable, so a request cannot be promoted twice; a request that fails to promote is left
/// in place for the next boundary rather than dropped.
///
/// Two things a worker's own channel is guarded against, both because a worker re-reads the same
/// unresolved situation every session and will ask about it again:
///
/// - **A repeat of a question already open is dropped.** Without this, "which instance is prod?"
///   becomes a new ask and a new page every single session until somebody answers.
/// - **At most [`MAX_OPEN_WORKER_ASKS`] worker asks are open at once.** Different wording every
///   session would slip past the dedupe; this bounds it anyway.
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
        // Recomputed per file: two requests in one batch must not both slip past the cap or the
        // dedupe by reading a snapshot taken before either landed.
        let existing = all(dir);
        let open_worker = existing
            .iter()
            .filter(|a| a.is_open() && matches!(a.origin, Origin::Worker))
            .count();
        if open_worker >= MAX_OPEN_WORKER_ASKS {
            eprintln!(
                "  ⚠ {MAX_OPEN_WORKER_ASKS} worker asks are already open — dropping further requests \
                 until some are answered. The worker is asking faster than anyone can answer."
            );
            let _ = std::fs::remove_file(&f);
            continue;
        }
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
        // Already asked and still unanswered? Drop the repeat rather than page again.
        if existing
            .iter()
            .any(|a| a.is_open() && matches!(a.origin, Origin::Worker) && a.question == req.question)
        {
            eprintln!("  [ask] the worker re-asked an open question; not paging again");
            let _ = std::fs::remove_file(&f);
            continue;
        }
        let id = (0..u64::MAX)
            .map(|seq| mint_id(seq, now_epoch))
            .find(|id| !existing.iter().any(|a| &a.id == id))
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

/// The answered worker asks this session must be told about: answered, and not yet delivered.
///
/// Only WORKER-origin asks. A driver's ask was answered to the driver's own code — putting it in the
/// worker's brief would tell the worker about a decision that was never addressed to it.
pub fn answers_for_worker(dir: &Path) -> Vec<Ask> {
    all(dir)
        .into_iter()
        .filter(|a| matches!(a.origin, Origin::Worker))
        .filter(|a| a.answer.is_some() && !a.delivered)
        .collect()
}

/// Mark answers as handed to the worker. Called once the brief carrying them is composed.
pub fn mark_delivered(dir: &Path, asks: &[Ask], now_epoch: u64) {
    for a in asks {
        if let Err(e) = append(dir, &Row::Delivered { id: a.id.clone(), ts: now_epoch }) {
            // Worst case on failure is telling the worker the same answer twice, which is annoying
            // rather than wrong — never a reason to kill the run.
            eprintln!("  ⚠ could not mark ask {} delivered: {e}", a.id);
        }
    }
}

/// Validate an answer against the ask it claims to answer, resolving it to a canonical value.
///
/// ONE validator, shared by every inbound channel (`agg send answer`, `POST /api/send`, and whatever
/// comes next), for the same reason `queue_command` is shared: two validators that disagreed about
/// what "postgres" means would be a bug nobody could see from either side.
///
/// A `choose`/`bool` ask has a CLOSED answer set, so anything off the list is refused here rather
/// than handed to a driver that never offered it. A 1-based number is accepted too — an operator
/// answering from a phone should not have to retype an option exactly.
pub fn validate_answer(dir: &Path, id: &str, value: &str) -> Result<String, String> {
    let Some(ask) = get(dir, id) else {
        let open = open(dir);
        return Err(if open.is_empty() {
            format!("no ask `{id}`, and nothing is waiting for an answer")
        } else {
            format!(
                "no ask `{id}`. Open right now: {}",
                open.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ")
            )
        });
    };
    if let Some(prev) = &ask.answer {
        // The run may already have acted on the first answer, so a second is refused rather than
        // silently rewriting a decision.
        return Err(format!("ask `{id}` was already answered {prev:?} — the first answer wins"));
    }
    match (&ask.case, &ask.options) {
        (AskCase::Input, _) => Ok(value.to_string()),
        (_, Some(opts)) => value
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1 && *n <= opts.len())
            .map(|n| opts[n - 1].clone())
            .or_else(|| opts.iter().find(|o| o.eq_ignore_ascii_case(value)).cloned())
            .ok_or_else(|| {
                format!(
                    "{value:?} is not one of the options for ask `{id}`: {}. Answer with the text or \
                     its number; the ask stays open.",
                    opts.iter()
                        .enumerate()
                        .map(|(i, o)| format!("{}. {o}", i + 1))
                        .collect::<Vec<_>>()
                        .join("  ")
                )
            }),
        (_, None) => Err(format!("ask `{id}` is a choice but records no options — ledger is corrupt")),
    }
}

/// Put a newly-opened ask on the OPERATOR'S OUTBOUND QUEUE (`agg/private/bus/out/`).
///
/// A question is a message to the operator, which is what [`crate::bus::Bus::emit`] was built for —
/// the in/out pair is symmetric: the ask goes out on `bus/out/`, the answer comes back on `bus/in/`
/// via `agg send answer`. Readers (the TUI, `agg status`, the web UI) consume it through the
/// `asks` field agg publishes to `state.json`; a push transport can be layered on later without
/// changing anything here.
///
/// `notify.cmd` is an OPTIONAL adapter on top of this, not the mechanism: a run with no notify
/// configured still queues every question, and nothing is lost if no adapter is wired.
///
/// Best-effort: a queue write that fails must not kill the run, because the ask is already durable
/// in `asks.jsonl` and every reader works off that.
pub fn emit_to_operator(dir: &Path, ask: &Ask) {
    let Ok(bus) = crate::bus::Bus::open(dir) else { return };
    let body = serde_json::to_string_pretty(ask).unwrap_or_else(|_| ask.question.clone());
    let stamp = format!("{:013}", ask.opened_at_epoch);
    if let Err(e) = bus.emit("ask", &body, &stamp) {
        eprintln!("  ⚠ could not queue ask {} for the operator: {e}", ask.id);
    }
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

    /// A worker re-reads the same unresolved situation every session and asks again. Without the
    /// dedupe that is a new ask AND a new page every session — the worker cannot stop the loop, but
    /// it can make the channel unusable by nagging.
    #[test]
    fn a_worker_re_asking_an_open_question_does_not_page_again() {
        let d = tempfile::tempdir().unwrap();
        let ask_it = || {
            write_worker_request(
                d.path(),
                &WorkerRequest {
                    case: AskCase::Input,
                    question: "which instance is prod?".into(),
                    options: None,
                    ts: 1,
                },
            )
            .unwrap();
        };

        ask_it();
        assert_eq!(promote_worker_requests(d.path(), 1, 100).len(), 1);
        ask_it();
        assert!(promote_worker_requests(d.path(), 2, 200).is_empty(), "the repeat is dropped");
        assert_eq!(open(d.path()).len(), 1, "still exactly one open ask");

        // Once answered, the same question CAN be asked again — the situation may have recurred, and
        // refusing forever would silently break the channel.
        answer(d.path(), &open(d.path())[0].id.clone(), "db-1", "operator", 300).unwrap();
        ask_it();
        assert_eq!(promote_worker_requests(d.path(), 3, 400).len(), 1, "an answered question may recur");
    }

    /// Different wording every session slips past the dedupe, so the count is bounded too.
    #[test]
    fn open_worker_asks_are_capped() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..MAX_OPEN_WORKER_ASKS + 3 {
            write_worker_request(
                d.path(),
                &WorkerRequest { case: AskCase::Bool, question: format!("q{i}?"), options: None, ts: i as u64 },
            )
            .unwrap();
            promote_worker_requests(d.path(), 1, 100 + i as u64);
        }
        assert_eq!(
            open(d.path()).len(),
            MAX_OPEN_WORKER_ASKS,
            "a runaway worker cannot open unbounded asks"
        );
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
