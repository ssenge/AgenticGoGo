//! USAGE — the tokens and dollars every agent call reported, kept per session and summed per run.
//!
//! agg runs the agents itself, so it is the only party that sees what they spent. This module is the
//! record of it: [`Usage`] is what ONE call reported (the backend parses it off the wire), the
//! [`Ledger`] files each call under its session, its role and the model that actually ran, and
//! [`Summary`] is the run's totals. The run report's `session_log` and `usage`, the live status's
//! `usage` and `agg history --json` are all views of it.
//!
//! # Three rules
//!
//! - **"Not reported" is not zero.** Every count is an `Option`: Copilot reports output tokens only,
//!   Codex has no cache-write figure and no price, and a `0` in either place would be a lie that a
//!   consumer summing costs could not detect. A sum is `null` only when NOTHING in it reported that
//!   kind; `cost_source` says whether the price covers every call (`reported`), some (`partial`) or
//!   none (`unknown`).
//! - **The model is the one the agent reported**, never the configured one. They differ — an alias
//!   resolves, an `auto` mode picks — and the record is about what ran.
//! - **Worker and ruler are kept apart.** The worker is the session's agent; the ruler runs the LLM
//!   judges and the summarizer. A run whose judges cost more than its work is a finding, and it is
//!   invisible in a single total.
//!
//! Recording usage never caps anything: the ceilings (`limits.tokens`, `limits.cost`) read the
//! run's own counters, which only exist when a config sets them.

use serde::{Deserialize, Serialize};

/// Tokens one call (or a sum of calls) reported, split the way providers bill them. A field is
/// `None` when the agent does not report that kind.
///
/// The four kinds do not overlap: `input` is the UNCACHED prompt, `cache_write` the prompt written to
/// the cache, `cache_read` the prompt served from it — so their sum is the whole prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_write: Option<u64>,
    pub cache_read: Option<u64>,
}

impl Tokens {
    /// Add `other` field by field. A kind stays `None` only if neither side reported it.
    pub fn add(&mut self, other: &Tokens) {
        self.input = add_opt(self.input, other.input);
        self.output = add_opt(self.output, other.output);
        self.cache_write = add_opt(self.cache_write, other.cache_write);
        self.cache_read = add_opt(self.cache_read, other.cache_read);
    }

    /// Output tokens, `0` when unreported — what the token ceiling (`limits.tokens`) counts.
    pub fn output_or_zero(&self) -> u64 {
        self.output.unwrap_or(0)
    }
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

fn add_cost(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

/// What one agent call reported: the model that ran, its tokens, its dollar price.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// as the agent named it on the wire; `None` if it never said.
    pub model: Option<String>,
    pub tokens: Tokens,
    /// `None` = the agent cannot price itself (only Claude reports dollars).
    pub cost_usd: Option<f64>,
}

/// Who made the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// the session's agent, doing the work.
    Worker,
    /// the agent behind the LLM judges and the summarizer.
    Ruler,
}

/// How much of a dollar figure the agents actually reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// every call in it reported a price.
    Reported,
    /// some did: `cost_usd` is the sum of those, a LOWER BOUND.
    Partial,
    /// none did (or there were no calls): `cost_usd` is `null`.
    #[default]
    Unknown,
}

impl CostSource {
    fn of(cost: Option<f64>) -> CostSource {
        if cost.is_some() { CostSource::Reported } else { CostSource::Unknown }
    }

    fn combine(self, other: CostSource) -> CostSource {
        if self == other { self } else { CostSource::Partial }
    }
}

/// A sum of calls: tokens, price, and how complete the price is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub tokens: Tokens,
    pub cost_usd: Option<f64>,
    pub cost_source: CostSource,
    /// calls summed so far — an empty sum must not decide `cost_source` for the first real one.
    #[serde(skip)]
    calls: u32,
}

impl Totals {
    /// Add one call.
    pub fn add_call(&mut self, u: &Usage) {
        self.merge(&Totals { tokens: u.tokens, cost_usd: u.cost_usd, cost_source: CostSource::of(u.cost_usd), calls: 1 });
    }

    /// Add another sum.
    pub fn merge(&mut self, other: &Totals) {
        if other.calls == 0 {
            return;
        }
        self.tokens.add(&other.tokens);
        self.cost_usd = add_cost(self.cost_usd, other.cost_usd);
        self.cost_source = if self.calls == 0 { other.cost_source } else { self.cost_source.combine(other.cost_source) };
        self.calls += other.calls;
    }
}

/// One line of a session's usage: what one role spent on one (agent, model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub role: Role,
    pub agent: String,
    pub model: Option<String>,
    #[serde(flatten)]
    pub totals: Totals,
}

/// One session's usage. Session `0` is the baseline pass, before any work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session: u32,
    /// the step the session ran; empty for the baseline pass.
    pub step: String,
    pub usage: Vec<Entry>,
}

/// The run's totals: all of it, and the worker and ruler parts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub total: Totals,
    pub worker: Totals,
    pub ruler: Totals,
}

/// Every call this run made, filed by session. Lives in the run's `ext` store.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    sessions: Vec<SessionUsage>,
}

impl Ledger {
    /// File one call under `session`. Calls of the same role on the same (agent, model) in one
    /// session share a line; the first step name recorded for a session sticks.
    pub fn record(&mut self, session: u32, step: &str, role: Role, agent: &str, u: &Usage) {
        let idx = match self.sessions.iter().position(|s| s.session == session) {
            Some(i) => i,
            None => {
                self.sessions.push(SessionUsage { session, step: step.to_string(), usage: Vec::new() });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[idx];
        if s.step.is_empty() {
            s.step = step.to_string();
        }
        let line = s.usage.iter().position(|e| e.role == role && e.agent == agent && e.model == u.model);
        let line = match line {
            Some(i) => &mut s.usage[i],
            None => {
                s.usage.push(Entry { role, agent: agent.to_string(), model: u.model.clone(), totals: Totals::default() });
                s.usage.last_mut().expect("just pushed")
            }
        };
        line.totals.add_call(u);
    }

    /// Every session that made a call, in the order they were first charged.
    pub fn sessions(&self) -> &[SessionUsage] {
        &self.sessions
    }

    /// The run's totals.
    pub fn summary(&self) -> Summary {
        let mut sum = Summary::default();
        for e in self.sessions.iter().flat_map(|s| &s.usage) {
            sum.total.merge(&e.totals);
            match e.role {
                Role::Worker => sum.worker.merge(&e.totals),
                Role::Ruler => sum.ruler.merge(&e.totals),
            }
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(model: &str, input: Option<u64>, output: u64, cost: Option<f64>) -> Usage {
        Usage {
            model: Some(model.into()),
            tokens: Tokens { input, output: Some(output), cache_write: None, cache_read: Some(10) },
            cost_usd: cost,
        }
    }

    /// A kind no call reported stays null in the sum; one reported anywhere counts the others as 0.
    #[test]
    fn tokens_add_without_inventing_zeroes() {
        let mut t = Tokens::default();
        t.add(&Tokens { output: Some(5), ..Default::default() });
        t.add(&Tokens { output: Some(7), input: Some(3), ..Default::default() });
        assert_eq!(t, Tokens { input: Some(3), output: Some(12), cache_write: None, cache_read: None });
        assert_eq!(t.output_or_zero(), 12);
        assert_eq!(Tokens::default().output_or_zero(), 0);
    }

    /// `reported` only when every call priced itself, `unknown` when none did, `partial` between.
    #[test]
    fn cost_source_says_how_complete_the_price_is() {
        let mut t = Totals::default();
        assert_eq!((t.cost_source, t.cost_usd), (CostSource::Unknown, None), "an empty sum knows nothing");
        t.add_call(&call("m", None, 1, Some(0.5)));
        assert_eq!((t.cost_source, t.cost_usd), (CostSource::Reported, Some(0.5)));
        t.add_call(&call("m", None, 1, None));
        assert_eq!((t.cost_source, t.cost_usd), (CostSource::Partial, Some(0.5)));

        let mut u = Totals::default();
        u.add_call(&call("m", None, 1, None));
        assert_eq!((u.cost_source, u.cost_usd), (CostSource::Unknown, None));
        u.merge(&Totals::default());
        assert_eq!(u.cost_source, CostSource::Unknown, "merging an empty sum changes nothing");
    }

    /// Lines are per (session, role, agent, model); the summary splits worker from ruler.
    #[test]
    fn the_ledger_files_calls_by_session_role_and_model() {
        let mut l = Ledger::default();
        l.record(0, "", Role::Ruler, "claude", &call("haiku", Some(1), 2, Some(0.01)));
        l.record(1, "work", Role::Worker, "claude", &call("opus", Some(100), 50, Some(1.0)));
        l.record(1, "", Role::Ruler, "claude", &call("haiku", Some(1), 2, Some(0.01)));
        l.record(1, "", Role::Ruler, "claude", &call("haiku", Some(1), 3, Some(0.02)));
        l.record(1, "", Role::Ruler, "claude", &call("sonnet", Some(1), 4, None));

        let s = l.sessions();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].session, s[0].step.as_str()), (0, ""));
        assert_eq!((s[1].session, s[1].step.as_str()), (1, "work"), "the first step name sticks");
        assert_eq!(s[1].usage.len(), 3, "worker/opus, ruler/haiku (two calls), ruler/sonnet");
        let haiku = &s[1].usage[1];
        assert_eq!((haiku.role, haiku.model.as_deref(), haiku.totals.tokens.output), (Role::Ruler, Some("haiku"), Some(5)));

        let sum = l.summary();
        assert_eq!(sum.worker.tokens.output, Some(50));
        assert_eq!(sum.ruler.tokens.output, Some(11));
        assert_eq!(sum.total.tokens.output, Some(61));
        assert_eq!(sum.ruler.cost_source, CostSource::Partial, "the sonnet call reported no price");
        assert_eq!(sum.worker.cost_source, CostSource::Reported);
        assert_eq!(sum.total.cost_usd, Some(1.04));
    }

    /// The wire shape of a line: flat, with every token kind present and `null` where unreported.
    #[test]
    fn an_entry_serializes_flat_with_explicit_nulls() {
        let mut l = Ledger::default();
        l.record(2, "w", Role::Worker, "codex", &Usage { tokens: Tokens { output: Some(3), ..Default::default() }, ..Default::default() });
        let v = serde_json::to_value(&l.sessions()[0].usage[0]).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "role": "worker", "agent": "codex", "model": null,
                "tokens": {"input": null, "output": 3, "cache_write": null, "cache_read": null},
                "cost_usd": null, "cost_source": "unknown"
            })
        );
    }
}
