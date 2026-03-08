//! Domain types for native and external knowledge retrieval.

use serde::{Deserialize, Serialize};

/// A normalized retrieval query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeQuery {
    pub query: String,
    pub max_results: usize,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

/// A normalized retrieval hit returned to branches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub content_type: String,
    pub score: f32,
    pub provenance: KnowledgeProvenance,
}

/// Mandatory provenance attached to every knowledge hit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeProvenance {
    pub source_id: String,
    pub source_kind: KnowledgeSourceKind,
    pub source_label: String,
    pub canonical_locator: String,
}

/// Source types supported by the retrieval plane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceKind {
    NativeMemory,
    Qmd,
    GoogleWorkspace,
}

impl std::fmt::Display for KnowledgeSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativeMemory => f.write_str("native_memory"),
            Self::Qmd => f.write_str("qmd"),
            Self::GoogleWorkspace => f.write_str("google_workspace"),
        }
    }
}

/// Capabilities a source can advertise in the registry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceCapability {
    HybridSearch,
    TypedMemoryRecall,
    DocumentSearch,
    EmailSearch,
    CalendarSearch,
}

impl std::fmt::Display for KnowledgeSourceCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HybridSearch => f.write_str("hybrid_search"),
            Self::TypedMemoryRecall => f.write_str("typed_memory_recall"),
            Self::DocumentSearch => f.write_str("document_search"),
            Self::EmailSearch => f.write_str("email_search"),
            Self::CalendarSearch => f.write_str("calendar_search"),
        }
    }
}

/// Whether a source is usable, configured, or intentionally reserved.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceEnablementState {
    Enabled,
    Configured,
    Placeholder,
    Disabled,
}

/// Enablement information for a source descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeSourceEnablement {
    pub state: KnowledgeSourceEnablementState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl KnowledgeSourceEnablement {
    pub fn enabled() -> Self {
        Self {
            state: KnowledgeSourceEnablementState::Enabled,
            reason: None,
        }
    }

    pub fn configured(reason: impl Into<String>) -> Self {
        Self {
            state: KnowledgeSourceEnablementState::Configured,
            reason: Some(reason.into()),
        }
    }

    pub fn placeholder(reason: impl Into<String>) -> Self {
        Self {
            state: KnowledgeSourceEnablementState::Placeholder,
            reason: Some(reason.into()),
        }
    }

    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            state: KnowledgeSourceEnablementState::Disabled,
            reason: Some(reason.into()),
        }
    }
}

/// Registry entry describing a retrieval source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeSourceDescriptor {
    pub id: String,
    pub label: String,
    pub kind: KnowledgeSourceKind,
    pub capabilities: Vec<KnowledgeSourceCapability>,
    pub enablement: KnowledgeSourceEnablement,
}

/// Source-level outcome from a retrieval call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceStatusState {
    Used,
    Unavailable,
}

/// Structured source status returned alongside retrieval hits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeSourceStatus {
    pub source_id: String,
    pub state: KnowledgeSourceStatusState,
    pub hit_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Structured output from the retrieval service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeRetrievalResult {
    pub hits: Vec<KnowledgeHit>,
    pub source_statuses: Vec<KnowledgeSourceStatus>,
}

#[cfg(test)]
mod tests {
    use super::{
        KnowledgeHit, KnowledgeProvenance, KnowledgeSourceCapability, KnowledgeSourceKind,
    };

    #[test]
    fn knowledge_hit_deserialization_requires_provenance() {
        let result = serde_json::from_value::<KnowledgeHit>(serde_json::json!({
            "id": "memory-1",
            "title": "Found memory",
            "snippet": "Stored note",
            "content_type": "memory/fact",
            "score": 0.9
        }));
        assert!(result.is_err());
    }

    #[test]
    fn knowledge_hit_serialization_preserves_provenance() {
        let hit = KnowledgeHit {
            id: "memory-1".to_string(),
            title: "Found memory".to_string(),
            snippet: "Stored note".to_string(),
            content_type: "memory/fact".to_string(),
            score: 0.9,
            provenance: KnowledgeProvenance {
                source_id: "native_memory".to_string(),
                source_kind: KnowledgeSourceKind::NativeMemory,
                source_label: "Native memory".to_string(),
                canonical_locator: "memory://memory-1".to_string(),
            },
        };

        let value = serde_json::to_value(hit).expect("serialize knowledge hit");
        assert_eq!(value["provenance"]["source_id"], "native_memory");
        assert_eq!(
            value["provenance"]["canonical_locator"],
            "memory://memory-1"
        );
        assert_eq!(
            serde_json::from_value::<KnowledgeSourceCapability>(serde_json::json!(
                "document_search"
            ))
            .expect("capability enum"),
            KnowledgeSourceCapability::DocumentSearch
        );
    }
}
