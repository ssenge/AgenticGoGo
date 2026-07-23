//! The outer loop's stage enum, `Phase`, and its cross-version wire (de)serialization.
//!
//! Split out of the dashboard state module: `Phase` is a self-contained value type with its own
//! hand-written serde impls (so an unknown stage from another `agg` build round-trips verbatim
//! rather than crashing the reader). It carries no dependency on `DashboardState`.

/// The outer loop's current stage. The four deterministic stages (INJECT → RUN → VERIFY → GATE)
/// plus the three off-cycle ones.
///
/// Was a bare `String` assigned from literals at ~10 sites in loop_.rs and re-matched by literal
/// in the dashboard and status renderers — a typo at either end was a silent mis-render, and
/// adding a stage meant remembering to touch two `match`es with `_` arms that would happily
/// swallow it.
///
/// # state.json compatibility (REQUIRED, both directions)
/// This serializes to and from exactly the lowercase strings it always did, because `state.json`
/// is a cross-version contract: `agg dashboard` / `agg status` attach to a loop that may be
/// running a DIFFERENT `agg` build than they are.
///
/// That is also why [`Phase::Other`] exists rather than a hard parse error. An older agg wrote
/// `"phase":"judging"` — a stage this build has no variant for. Rejecting it would crash the
/// dashboard against a running loop; mapping it to a catch-all `Unknown` would lie about what the
/// loop is doing. `Other` keeps the text verbatim, so it round-trips and still renders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Starting,
    Inject,
    Run,
    Verify,
    Gate,
    Backoff,
    /// a `skip_judges` step whose work is staged onto the span (§7.4).
    Staging,
    Done,
    /// A stage name this build doesn't know — from a state.json written by another agg version.
    /// Held verbatim so it survives a read/write round-trip instead of being flattened.
    Other(String),
}

impl Phase {
    /// The wire form — the exact lowercase string that has always been in state.json.
    pub fn as_str(&self) -> &str {
        match self {
            Phase::Starting => "starting",
            Phase::Inject => "inject",
            Phase::Run => "run",
            Phase::Verify => "verify",
            Phase::Gate => "gate",
            Phase::Backoff => "backoff",
            Phase::Staging => "staging",
            Phase::Done => "done",
            Phase::Other(s) => s,
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Phase {
    fn from(s: &str) -> Self {
        match s {
            "starting" => Phase::Starting,
            "inject" => Phase::Inject,
            "run" => Phase::Run,
            "verify" => Phase::Verify,
            "gate" => Phase::Gate,
            "backoff" => Phase::Backoff,
            "staging" => Phase::Staging,
            "done" => Phase::Done,
            other => Phase::Other(other.to_string()),
        }
    }
}

// Hand-written rather than `#[derive(Serialize, Deserialize)]`: a derived fieldless enum would
// reject any unknown tag, and serde's `#[serde(other)]` escape hatch is only available on
// internally/adjacently-tagged enums — neither applies to a plain JSON string field. So we go
// through `String` and let `From<&str>` absorb the unknown case.
impl serde::Serialize for Phase {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Phase {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Phase::from(String::deserialize(d)?.as_str()))
    }
}
