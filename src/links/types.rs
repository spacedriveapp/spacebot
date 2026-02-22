//! Types for the agent communication graph.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A directed edge in the agent communication graph.
///
/// Represents a persistent, policy-governed communication channel between two agents.
/// When agent A has a link to agent B, agent A can send messages to agent B.
/// The link carries direction and relationship flags that define the nature
/// of the communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLink {
    pub id: String,
    pub from_agent_id: String,
    pub to_agent_id: String,
    pub direction: LinkDirection,
    pub relationship: LinkRelationship,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Direction policy for an agent link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkDirection {
    /// from_agent can message to_agent, but not vice versa.
    OneWay,
    /// Both agents can message each other through this link.
    TwoWay,
}

impl LinkDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkDirection::OneWay => "one_way",
            LinkDirection::TwoWay => "two_way",
        }
    }
}

impl std::fmt::Display for LinkDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LinkDirection {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "one_way" => Ok(LinkDirection::OneWay),
            "two_way" => Ok(LinkDirection::TwoWay),
            other => Err(format!(
                "invalid link direction: '{other}', expected 'one_way' or 'two_way'"
            )),
        }
    }
}

/// Relationship semantics for an agent link.
///
/// Affects the receiving agent's system prompt context. A superior can delegate tasks,
/// a subordinate reports status and escalates. Peers communicate collaboratively.
/// The relationship doesn't restrict message delivery — it frames context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRelationship {
    /// Equal peers — neither agent has authority over the other.
    Peer,
    /// from_agent is superior to to_agent. Can delegate tasks,
    /// request status, and override decisions.
    Superior,
    /// from_agent is subordinate to to_agent. Reports status,
    /// escalates issues, requests approval.
    Subordinate,
}

impl LinkRelationship {
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkRelationship::Peer => "peer",
            LinkRelationship::Superior => "superior",
            LinkRelationship::Subordinate => "subordinate",
        }
    }

    /// Get the inverse relationship from the other agent's perspective.
    pub fn inverse(&self) -> Self {
        match self {
            LinkRelationship::Peer => LinkRelationship::Peer,
            LinkRelationship::Superior => LinkRelationship::Subordinate,
            LinkRelationship::Subordinate => LinkRelationship::Superior,
        }
    }
}

impl std::fmt::Display for LinkRelationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LinkRelationship {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "peer" => Ok(LinkRelationship::Peer),
            "superior" => Ok(LinkRelationship::Superior),
            "subordinate" => Ok(LinkRelationship::Subordinate),
            other => Err(format!(
                "invalid link relationship: '{other}', expected 'peer', 'superior', or 'subordinate'"
            )),
        }
    }
}
