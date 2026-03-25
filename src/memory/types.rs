//! Memory types and graph structures.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Memory structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub importance: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed_at: chrono::DateTime<chrono::Utc>,
    pub access_count: i64,
    pub source: Option<String>,
    pub channel_id: Option<String>,
    /// Soft-delete flag. Forgotten memories are excluded from search and recall
    /// but remain in the database.
    pub forgotten: bool,
}

impl Memory {
    /// Create a new memory with default values.
    pub fn new(content: impl Into<String>, memory_type: MemoryType) -> Self {
        let now = chrono::Utc::now();
        let id = Uuid::new_v4().to_string();
        let importance = memory_type.default_importance();

        Self {
            id,
            content: content.into(),
            memory_type,
            importance,
            created_at: now,
            updated_at: now,
            last_accessed_at: now,
            access_count: 0,
            source: None,
            channel_id: None,
            forgotten: false,
        }
    }

    /// Set the importance explicitly.
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Set the source.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set the channel ID.
    pub fn with_channel_id(mut self, channel_id: crate::ChannelId) -> Self {
        self.channel_id = Some(channel_id.to_string());
        self
    }

    /// Identity memories have maximum importance and don't decay.
    pub const fn identity_importance() -> f32 {
        1.0
    }

    /// Default importance for new memories.
    pub const fn default_importance() -> f32 {
        0.5
    }
}

impl MemoryType {
    /// Get the default importance for this memory type.
    pub fn default_importance(&self) -> f32 {
        match self {
            MemoryType::Identity => 1.0,
            MemoryType::Decision => 0.8,
            MemoryType::Preference => 0.7,
            MemoryType::Fact => 0.6,
            MemoryType::Event => 0.4,
            MemoryType::Observation => 0.3,
            MemoryType::Goal => 0.9,
            MemoryType::Todo => 0.8,
        }
    }
}

/// Memory types.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Something that is true.
    Fact,
    /// Something the user likes or dislikes.
    Preference,
    /// A choice that was made.
    Decision,
    /// Core information about who the user is or who the agent is.
    Identity,
    /// Something that happened.
    Event,
    /// Something the system noticed.
    Observation,
    /// Something the user or agent wants to achieve.
    Goal,
    /// An actionable task or reminder.
    Todo,
}

impl MemoryType {
    /// All variants in definition order.
    pub const ALL: &[MemoryType] = &[
        MemoryType::Fact,
        MemoryType::Preference,
        MemoryType::Decision,
        MemoryType::Identity,
        MemoryType::Event,
        MemoryType::Observation,
        MemoryType::Goal,
        MemoryType::Todo,
    ];
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::Fact => write!(f, "fact"),
            MemoryType::Preference => write!(f, "preference"),
            MemoryType::Decision => write!(f, "decision"),
            MemoryType::Identity => write!(f, "identity"),
            MemoryType::Event => write!(f, "event"),
            MemoryType::Observation => write!(f, "observation"),
            MemoryType::Goal => write!(f, "goal"),
            MemoryType::Todo => write!(f, "todo"),
        }
    }
}

/// Association between memories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
pub struct Association {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation_type: RelationType,
    pub weight: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Association {
    /// Create a new association.
    pub fn new(
        source_id: impl Into<String>,
        target_id: impl Into<String>,
        relation_type: RelationType,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            source_id: source_id.into(),
            target_id: target_id.into(),
            relation_type,
            weight: 0.5,
            created_at: chrono::Utc::now(),
        }
    }

    /// Set the weight.
    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight.clamp(0.0, 1.0);
        self
    }
}

/// Relation types for memory associations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// General semantic connection.
    RelatedTo,
    /// Newer version of the same information.
    Updates,
    /// Conflicting information.
    Contradicts,
    /// Causal relationship.
    CausedBy,
    /// Result relationship.
    ResultOf,
    /// Hierarchical relationship.
    PartOf,
}

impl std::fmt::Display for RelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelationType::RelatedTo => write!(f, "related_to"),
            RelationType::Updates => write!(f, "updates"),
            RelationType::Contradicts => write!(f, "contradicts"),
            RelationType::CausedBy => write!(f, "caused_by"),
            RelationType::ResultOf => write!(f, "result_of"),
            RelationType::PartOf => write!(f, "part_of"),
        }
    }
}

/// Search result combining memory with relevance score.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MemorySearchResult {
    pub memory: Memory,
    pub score: f32,
    pub rank: usize,
}

/// Input for memory creation.
#[derive(Debug, Clone)]
pub struct CreateMemoryInput {
    pub content: String,
    pub memory_type: MemoryType,
    pub importance: Option<f32>,
    pub source: Option<String>,
    pub channel_id: Option<crate::ChannelId>,
    pub embedding: Option<Vec<f32>>,
    pub associations: Vec<CreateAssociationInput>,
}

/// Input for association creation.
#[derive(Debug, Clone)]
pub struct CreateAssociationInput {
    pub target_id: String,
    pub relation_type: RelationType,
    pub weight: f32,
}
