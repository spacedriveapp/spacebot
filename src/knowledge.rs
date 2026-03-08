//! Knowledge retrieval contracts, source registry, and service layer.

mod fusion;
mod google_workspace;
mod qmd;
mod registry;
mod service;
mod types;

#[cfg(test)]
pub(crate) use qmd::{QmdAdapter, QmdToolResponse};

pub use google_workspace::{
    GoogleWorkspaceAuthStatusSummary, GoogleWorkspaceBridgeError, GoogleWorkspaceDryRunSummary,
    GoogleWorkspaceFamily, GoogleWorkspaceNormalizedSourceStatus, GoogleWorkspaceSchemaSummary,
    normalize_auth_status_output, normalize_dry_run_output, normalize_schema_output,
    normalized_source_statuses_from_auth, status_message_for_source,
};
pub use registry::KnowledgeSourceRegistry;
pub use service::{KnowledgeRetrievalService, KnowledgeServiceError};
pub use types::{
    KnowledgeHit, KnowledgeProvenance, KnowledgeQuery, KnowledgeRetrievalResult,
    KnowledgeSourceCapability, KnowledgeSourceDescriptor, KnowledgeSourceEnablement,
    KnowledgeSourceEnablementState, KnowledgeSourceKind, KnowledgeSourceStatus,
    KnowledgeSourceStatusState,
};
