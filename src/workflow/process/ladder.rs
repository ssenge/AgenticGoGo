//! A step's ladder (A-37): its tiers, first to last, each working on what the one
//! before left and judged by the step's own checks and critic. The first tier that meets them ends it; an unmet
//! one hands over to the next; the last one goes to the task force as a step always did. A re-entry with a note
//! starts one tier above the one the step reached.

use serde::Deserialize;

/// One tier as the config writes it.
#[derive(Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TierSpec {
    /// `agent`: from here, a decider picks the next tiers.
    Decide(String),
    /// `{ tool: <capability or bound tool> }`: a tool changes the step's work (`ladder.tool_run`), then its checks.
    Tool { tool: String },
    /// An agent session in a mode, with its model, effort and repair rounds.
    Agent(AgentTier),
}

/// An agent tier: how the step's worker runs on it.
#[derive(Deserialize, Clone, Debug, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentTier {
    #[serde(default)]
    pub mode: Mode,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub agent: Option<String>,
    pub repairs: Option<usize>,
    pub max: Option<usize>,
}

/// How an agent tier is judged and repaired; a step's checks run in every mode.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Default, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// the checks alone, `repairs` rounds
    Once,
    /// the checks and the critic, `repairs` rounds (a step without checks: the critic alone)
    Dcr,
    #[default]
    Dccr,
    /// the checks and the critic, until met or stalled, at most `max` rounds
    Converge,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config's three kinds of tier, and the defaults of an agent tier.
    #[test]
    fn a_ladder_reads_its_three_kinds_of_tier() {
        let tiers: Vec<TierSpec> = serde_yaml::from_str(
            "- { tool: transform.recipes }\n- { mode: converge, model: large, max: 4 }\n- agent\n- {}\n").unwrap();
        assert_eq!(tiers[0], TierSpec::Tool { tool: "transform.recipes".into() });
        assert_eq!(tiers[1], TierSpec::Agent(AgentTier { mode: Mode::Converge, model: Some("large".into()),
                                                         max: Some(4), ..AgentTier::default() }));
        assert_eq!(tiers[2], TierSpec::Decide("agent".into()));
        assert_eq!(tiers[3], TierSpec::Agent(AgentTier::default()), "an empty tier is dccr with the step's own settings");
        assert!(serde_yaml::from_str::<Vec<TierSpec>>("- { mode: once, bogus: 1 }\n").is_err());
    }
}
