//! Knowledge retrieval contracts, source registry, and service layer.

mod registry;
mod service;
mod types;

pub use registry::KnowledgeSourceRegistry;
pub use service::{KnowledgeRetrievalService, KnowledgeServiceError};
pub use types::{
    KnowledgeHit, KnowledgeProvenance, KnowledgeQuery, KnowledgeRetrievalResult,
    KnowledgeSourceCapability, KnowledgeSourceDescriptor, KnowledgeSourceEnablement,
    KnowledgeSourceEnablementState, KnowledgeSourceKind, KnowledgeSourceStatus,
    KnowledgeSourceStatusState,
};
