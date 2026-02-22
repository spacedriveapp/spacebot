//! Agent communication graph: links between agents with direction and relationship policies.

pub mod store;
pub mod types;

pub use store::LinkStore;
pub use types::{AgentLink, LinkDirection, LinkRelationship};
