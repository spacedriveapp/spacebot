//! Agent communication graph: directed links between agents with relationship policies.
//!
//! Links are defined in config via `[[links]]` sections and stored as a shared
//! `ArcSwap<Vec<AgentLink>>` that's hot-reloadable when config changes.

pub mod types;

pub use types::{AgentLink, LinkDirection, LinkKind};

/// Find the link between two agents (checking both directions).
pub fn find_link_between<'a>(
    links: &'a [AgentLink],
    agent_a: &str,
    agent_b: &str,
) -> Option<&'a AgentLink> {
    links.iter().find(|link| {
        (link.from_agent_id == agent_a && link.to_agent_id == agent_b)
            || (link.from_agent_id == agent_b && link.to_agent_id == agent_a)
    })
}

/// Get all links involving a specific agent.
pub fn links_for_agent<'a>(links: &'a [AgentLink], agent_id: &str) -> Vec<&'a AgentLink> {
    links
        .iter()
        .filter(|link| link.from_agent_id == agent_id || link.to_agent_id == agent_id)
        .collect()
}
