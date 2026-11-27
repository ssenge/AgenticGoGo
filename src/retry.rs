//! Retries: what a failed worker session is, which failures are tried again, and how long to wait between tries.
//!
//! A retry is not a work session. It stays inside one session of the loop: one ledger record (with `attempts`), no new
//! session branch, nothing counted toward `stalled`, the repair budget or the dud streak. Its tokens and cost are
//! charged. It retries only what is not the work's fault (an overloaded API, a dropped connection, a crash, a timeout);
//! what a verifier sees (repairs, ladders, the task force) is a different mechanism.

use std::time::Duration;

use serde::Deserialize;

/// What a failed session was, as far as retrying is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// a quota or limit with a stated reset: waits for the reset (the existing rate-limit wait), is not a retry
    RateLimit,
    /// overloaded (529), a 5xx, a dropped or refused connection, a stream that ended without its terminal event
    Transient,
    /// a rejected key or login: never retried, the same call fails the same way
    Auth,
    /// the step's `timeout` fired, or the stall watchdog killed a hung session
    Timeout,
    /// a non-zero exit with nothing to classify it by
    Crash,
    /// a clean exit that changed nothing (opt-in: `on: [empty]`)
    Empty,
    /// anything else: not retried
    Unknown,
}

impl FailureClass {
    /// The word for logs and records.
    pub fn word(self) -> &'static str {
        match self {
            FailureClass::RateLimit => "rate_limit",
            FailureClass::Transient => "transient",
            FailureClass::Auth => "auth",
            FailureClass::Timeout => "timeout",
            FailureClass::Crash => "crash",
            FailureClass::Empty => "empty",
            FailureClass::Unknown => "unknown",
        }
    }
}

/// The class of a failed session from what it reported. A backend with a better idea overrides
/// [`crate::backend::AgentBackend::classify`]; this is the shared default.
///
/// `error` is the text of the terminal failure event (never tool output: a command that prints "401" is no auth
/// failure); `rate_limited` and `reset` are the backend's own detection.
pub fn classify(error: Option<&str>, exit_code: Option<i32>, timed_out: bool, killed: bool, rate_limited: bool) -> FailureClass {
    let text = error.unwrap_or_default().to_ascii_lowercase();
    let has = |pats: &[&str]| pats.iter().any(|p| text.contains(p));
    if timed_out || killed {
        return FailureClass::Timeout;
    }
    if has(&["invalid api key", "invalid_api_key", "authentication", "unauthorized", "401", "not logged in", "please run /login"]) {
        return FailureClass::Auth;
    }
    // an overload or a server error is a try-again, also where it is worded like a limit ("overloaded_error")
    if has(&["overloaded", "529", "500 ", "502", "503", "504", "internal server error", "bad gateway", "service unavailable",
             "temporarily unavailable", "econnreset", "connection reset", "connection refused", "etimedout", "timed out",
             "socket hang up", "stream ended", "terminal event"]) && !has(&["resets "]) {
        return FailureClass::Transient;
    }
    if rate_limited {
        return FailureClass::RateLimit;
    }
    match exit_code {
        Some(0) => FailureClass::Unknown,
        _ => FailureClass::Crash,
    }
}

/// A span of time written as a number of seconds or with a unit (`90`, `15s`, `5m`, `2h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dur(pub u64);

impl Dur {
    /// `90`, `15s`, `5m`, `2h` (seconds, minutes, hours).
    pub fn parse(text: &str) -> Result<Dur, String> {
        let t = text.trim().to_ascii_lowercase();
        let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
        let (number, unit) = t.split_at(split);
        let factor = match unit {
            "" | "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            other => return Err(format!("`{text}`: unknown unit `{other}` (s, m, h)")),
        };
        match number.trim().parse::<f64>() {
            Ok(v) if v >= 0.0 => Ok(Dur((v * factor) as u64)),
            _ => Err(format!("`{text}`: not a duration like 15s, 5m or 2h")),
        }
    }

    /// As a std duration.
    pub fn as_duration(self) -> Duration {
        Duration::from_secs(self.0)
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Secs(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Secs(s) => Ok(Dur(s)),
            Raw::Text(t) => Dur::parse(&t).map_err(serde::de::Error::custom),
        }
    }
}

impl schemars::JsonSchema for Dur {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Duration".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"description": "seconds, or a number with s, m or h (`15s`, `5m`, `2h`)", "anyOf": [{"type": "integer"}, {"type": "string"}]})
    }
}

/// How long to wait between tries.
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backoff {
    /// the wait before the second try (default 15s)
    #[serde(default = "Backoff::initial")]
    pub initial: Dur,
    /// each wait is this many times the one before (default 2)
    #[serde(default = "Backoff::factor")]
    pub factor: f64,
    /// no wait is longer (default 5m)
    #[serde(default = "Backoff::max")]
    pub max: Dur,
    /// spread each wait by up to a quarter either way, so tries of several runs don't line up (default true)
    #[serde(default = "Backoff::jitter")]
    pub jitter: bool,
}

impl Backoff {
    fn initial() -> Dur {
        Dur(15)
    }
    fn factor() -> f64 {
        2.0
    }
    fn max() -> Dur {
        Dur(300)
    }
    fn jitter() -> bool {
        true
    }

    /// The wait before try number `next` (2 for the first retry), without jitter.
    pub fn base_wait(&self, next: u32) -> Duration {
        let secs = self.initial.0 as f64 * self.factor.powi(next.saturating_sub(2) as i32);
        Duration::from_secs_f64(secs.min(self.max.0 as f64).max(0.0))
    }

    /// `base_wait` spread by `r` in 0..1 (0.5 = no spread): between 75 % and 125 % of it, when jitter is on.
    pub fn wait(&self, next: u32, r: f64) -> Duration {
        let base = self.base_wait(next);
        if self.jitter {
            base.mul_f64(0.75 + 0.5 * r.clamp(0.0, 1.0))
        } else {
            base
        }
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { initial: Backoff::initial(), factor: Backoff::factor(), max: Backoff::max(), jitter: true }
    }
}

/// The retry policy of a step.
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// tries in all, the first included; 1 = no retry (default 3)
    #[serde(default = "Retry::attempts")]
    pub attempts: u32,
    #[serde(default)]
    pub backoff: Backoff,
    /// the classes that are tried again (default transient, crash, timeout)
    #[serde(default = "Retry::on")]
    pub on: Vec<FailureClass>,
}

impl Retry {
    fn attempts() -> u32 {
        3
    }
    fn on() -> Vec<FailureClass> {
        vec![FailureClass::Transient, FailureClass::Crash, FailureClass::Timeout]
    }

    /// Whether a session that ended as `class` after `made` tries is tried again.
    pub fn again(&self, class: FailureClass, made: u32) -> bool {
        made < self.attempts && self.on.contains(&class)
    }
}

impl Default for Retry {
    fn default() -> Self {
        Retry { attempts: Retry::attempts(), backoff: Backoff::default(), on: Retry::on() }
    }
}

/// `retry:` as written: a policy, or `false` for none (`true` is the default policy).
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RetrySetting {
    Flag(bool),
    Policy(Retry),
}

impl RetrySetting {
    /// The policy it stands for, `None` for `false`.
    pub fn policy(&self) -> Option<Retry> {
        match self {
            RetrySetting::Flag(false) => None,
            RetrySetting::Flag(true) => Some(Retry::default()),
            RetrySetting::Policy(p) => Some(p.clone()),
        }
    }
}

/// The policy a step runs with: the one it names (`false` = none), else the default.
pub fn resolved(setting: Option<&RetrySetting>) -> Option<Retry> {
    setting.map_or_else(|| Some(Retry::default()), RetrySetting::policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class from the text a session ended with, and the order of the rules.
    #[test]
    fn a_failure_is_classified_from_what_the_session_reported() {
        let c = |e: &str, code, to, kill, rl| classify(Some(e), code, to, kill, rl);
        assert_eq!(c("", Some(1), true, false, false), FailureClass::Timeout);
        assert_eq!(c("", Some(1), false, true, false), FailureClass::Timeout, "a hung session the watchdog killed");
        assert_eq!(c("Invalid API key · Please run /login", Some(1), false, false, false), FailureClass::Auth);
        assert_eq!(c("API Error: 401 authentication_error", Some(1), false, false, false), FailureClass::Auth);
        assert_eq!(c("overloaded_error: Overloaded", Some(1), false, false, true), FailureClass::Transient, "worded like a limit");
        assert_eq!(c("API Error: 529", Some(1), false, false, false), FailureClass::Transient);
        assert_eq!(c("503 Service Unavailable", Some(1), false, false, false), FailureClass::Transient);
        assert_eq!(c("read ECONNRESET", Some(1), false, false, false), FailureClass::Transient);
        assert_eq!(c("stream ended without a terminal event", Some(1), false, false, false), FailureClass::Transient);
        assert_eq!(c("You've hit your session limit · resets 12:50pm", Some(1), false, false, true), FailureClass::RateLimit);
        assert_eq!(c("overloaded, resets 12:50pm", Some(1), false, false, true), FailureClass::RateLimit, "a stated reset wins");
        assert_eq!(c("", Some(137), false, false, false), FailureClass::Crash);
        assert_eq!(c("", Some(0), false, false, false), FailureClass::Unknown);
        assert_eq!(classify(None, None, false, false, false), FailureClass::Crash, "no exit code at all");
    }

    /// 90, 15s, 5m, 2h, and what is refused.
    #[test]
    fn durations_take_a_unit() {
        assert_eq!(Dur::parse("90"), Ok(Dur(90)));
        assert_eq!(Dur::parse("15s"), Ok(Dur(15)));
        assert_eq!(Dur::parse("5m"), Ok(Dur(300)));
        assert_eq!(Dur::parse("2h"), Ok(Dur(7200)));
        assert_eq!(Dur::parse("1.5m"), Ok(Dur(90)));
        assert!(Dur::parse("5d").is_err() && Dur::parse("soon").is_err() && Dur::parse("-5s").is_err());
        assert_eq!(serde_yaml::from_str::<Dur>("45m").unwrap(), Dur(2700));
        assert_eq!(serde_yaml::from_str::<Dur>("30").unwrap(), Dur(30));
    }

    /// 15 s, 30 s, 60 s … up to the cap; jitter stays within a quarter either way; off means exact.
    #[test]
    fn the_backoff_doubles_up_to_its_cap_and_jitter_is_bounded() {
        let b = Backoff::default();
        let secs = |n| b.base_wait(n).as_secs();
        assert_eq!((secs(2), secs(3), secs(4), secs(5), secs(6), secs(7), secs(20)), (15, 30, 60, 120, 240, 300, 300));
        assert_eq!(b.wait(2, 0.0), Duration::from_secs_f64(15.0 * 0.75));
        assert_eq!(b.wait(2, 1.0), Duration::from_secs_f64(15.0 * 1.25));
        assert_eq!(b.wait(2, 0.5), Duration::from_secs(15));
        let exact = Backoff { jitter: false, ..Backoff::default() };
        assert_eq!(exact.wait(3, 0.0), Duration::from_secs(30));
    }

    /// The default policy retries transient, crash and timeout, three tries in all; an attempt count stops it.
    #[test]
    fn the_default_policy_retries_what_is_not_the_works_fault() {
        let p = Retry::default();
        assert!(p.again(FailureClass::Transient, 1) && p.again(FailureClass::Crash, 2) && p.again(FailureClass::Timeout, 1));
        assert!(!p.again(FailureClass::Transient, 3), "three tries are made");
        for never in [FailureClass::Auth, FailureClass::RateLimit, FailureClass::Unknown, FailureClass::Empty] {
            assert!(!p.again(never, 1), "{never:?}");
        }
    }

    /// YAML: a partial policy, `false`, `true`, and an unknown key refused.
    #[test]
    fn yaml_reads_a_policy_false_and_true() {
        let s = |t: &str| serde_yaml::from_str::<RetrySetting>(t);
        assert_eq!(s("false").unwrap().policy(), None);
        assert_eq!(s("true").unwrap().policy(), Some(Retry::default()));
        let p = s("{ attempts: 5, backoff: { initial: 1s, factor: 3 }, on: [transient, empty] }").unwrap().policy().unwrap();
        assert_eq!((p.attempts, p.backoff.initial, p.backoff.max, p.on), (5, Dur(1), Dur(300), vec![FailureClass::Transient, FailureClass::Empty]));
        assert!(s("{ attemps: 5 }").is_err());
        assert_eq!(FailureClass::Auth.word(), "auth");
    }
}
