//! Retrieval service for normalized knowledge lookups.

use crate::config::RuntimeConfig;
use crate::knowledge::fusion::reciprocal_rank_fuse;
use crate::knowledge::qmd::{QmdAdapter, QmdRetrievalOutcome};
use crate::knowledge::registry::{NATIVE_MEMORY_SOURCE_ID, QMD_SOURCE_ID};
use crate::mcp::McpManager;
use crate::memory::MemorySearch;
use crate::memory::search::{SearchConfig, SearchMode, curate_results};

use std::sync::Arc;

use super::google_workspace::status_message_for_source;
use super::registry::KnowledgeSourceRegistry;
use super::types::{
    KnowledgeHit, KnowledgeProvenance, KnowledgeQuery, KnowledgeRetrievalResult,
    KnowledgeSourceEnablementState, KnowledgeSourceKind, KnowledgeSourceStatus,
    KnowledgeSourceStatusState,
};

/// Error type for the knowledge retrieval service.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeServiceError {
    #[error("knowledge retrieval failed: {0}")]
    Message(String),
}

impl KnowledgeServiceError {
    fn user_message(&self) -> String {
        match self {
            Self::Message(message) => message.clone(),
        }
    }
}

/// Service that mediates native memory retrieval and source registry state.
#[derive(Debug, Clone)]
pub struct KnowledgeRetrievalService {
    memory_search: Arc<MemorySearch>,
    runtime_config: Arc<RuntimeConfig>,
    qmd_adapter: QmdAdapter,
    #[cfg(test)]
    forced_memory_failure: Option<String>,
}

impl KnowledgeRetrievalService {
    pub fn new(
        memory_search: Arc<MemorySearch>,
        runtime_config: Arc<RuntimeConfig>,
        mcp_manager: Arc<McpManager>,
    ) -> Self {
        Self {
            memory_search,
            runtime_config,
            qmd_adapter: QmdAdapter::new(mcp_manager),
            #[cfg(test)]
            forced_memory_failure: None,
        }
    }

    #[cfg(test)]
    pub fn with_qmd_adapter(mut self, qmd_adapter: QmdAdapter) -> Self {
        self.qmd_adapter = qmd_adapter;
        self
    }

    #[cfg(test)]
    pub fn with_forced_memory_failure(mut self, message: impl Into<String>) -> Self {
        self.forced_memory_failure = Some(message.into());
        self
    }

    pub fn registry(&self) -> KnowledgeSourceRegistry {
        let mcp_servers = self.runtime_config.mcp.load();
        KnowledgeSourceRegistry::from_mcp_servers(&mcp_servers)
    }

    pub async fn retrieve(
        &self,
        query: KnowledgeQuery,
    ) -> Result<KnowledgeRetrievalResult, KnowledgeServiceError> {
        let registry = self.registry();
        let source_ids = if query.source_ids.is_empty() {
            registry.default_source_ids()
        } else {
            query.source_ids.clone()
        };

        let mut hits = Vec::new();
        let mut hit_lists = Vec::new();
        let mut source_statuses = Vec::new();

        for source_id in source_ids {
            let Some(source) = registry.get(&source_id) else {
                source_statuses.push(KnowledgeSourceStatus {
                    source_id,
                    state: KnowledgeSourceStatusState::Unavailable,
                    hit_count: 0,
                    message: Some("Unknown knowledge source requested.".to_string()),
                });
                continue;
            };

            if source.id == NATIVE_MEMORY_SOURCE_ID {
                match self.retrieve_memory_hits(&query).await {
                    Ok(source_hits) => {
                        source_statuses.push(KnowledgeSourceStatus {
                            source_id: source.id.clone(),
                            state: KnowledgeSourceStatusState::Used,
                            hit_count: source_hits.len(),
                            message: None,
                        });
                        hit_lists.push(source_hits);
                    }
                    Err(error) => {
                        source_statuses.push(KnowledgeSourceStatus {
                            source_id: source.id.clone(),
                            state: KnowledgeSourceStatusState::Unavailable,
                            hit_count: 0,
                            message: Some(error.user_message()),
                        });
                    }
                }
                continue;
            }

            if source.id == QMD_SOURCE_ID {
                if !matches!(
                    source.enablement.state,
                    KnowledgeSourceEnablementState::Enabled
                        | KnowledgeSourceEnablementState::Configured
                ) {
                    source_statuses.push(KnowledgeSourceStatus {
                        source_id: source.id.clone(),
                        state: KnowledgeSourceStatusState::Unavailable,
                        hit_count: 0,
                        message: source.enablement.reason.clone(),
                    });
                    continue;
                }

                match self.qmd_adapter.retrieve(&query).await {
                    Ok(QmdRetrievalOutcome {
                        hits: source_hits,
                        malformed_result_count,
                    }) => {
                        let state = if source_hits.is_empty() && malformed_result_count > 0 {
                            KnowledgeSourceStatusState::Unavailable
                        } else {
                            KnowledgeSourceStatusState::Used
                        };
                        let message = if malformed_result_count > 0 {
                            Some(format!(
                                "Dropped {malformed_result_count} malformed QMD result(s)."
                            ))
                        } else {
                            None
                        };
                        source_statuses.push(KnowledgeSourceStatus {
                            source_id: source.id.clone(),
                            state,
                            hit_count: source_hits.len(),
                            message,
                        });
                        hit_lists.push(source_hits);
                    }
                    Err(error) => {
                        source_statuses.push(KnowledgeSourceStatus {
                            source_id: source.id.clone(),
                            state: KnowledgeSourceStatusState::Unavailable,
                            hit_count: 0,
                            message: Some(error.user_message()),
                        });
                    }
                }
                continue;
            }

            let message = match source.kind {
                KnowledgeSourceKind::GoogleWorkspace => {
                    Some(status_message_for_source(&source.id, None))
                }
                _ => Some(source.enablement.reason.clone().unwrap_or_else(|| {
                    "Source is marked enabled but no native adapter is available yet.".to_string()
                })),
            };
            source_statuses.push(KnowledgeSourceStatus {
                source_id: source.id.clone(),
                state: KnowledgeSourceStatusState::Unavailable,
                hit_count: 0,
                message,
            });
        }

        if hit_lists.len() == 1 {
            hits = hit_lists.into_iter().next().unwrap_or_default();
        } else if !hit_lists.is_empty() {
            hits = reciprocal_rank_fuse(&hit_lists, query.max_results);
        }

        Ok(KnowledgeRetrievalResult {
            hits,
            source_statuses,
        })
    }

    async fn retrieve_memory_hits(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<KnowledgeHit>, KnowledgeServiceError> {
        #[cfg(test)]
        if let Some(message) = &self.forced_memory_failure {
            return Err(KnowledgeServiceError::Message(message.clone()));
        }

        let config = SearchConfig {
            mode: SearchMode::Hybrid,
            max_results: query.max_results,
            max_results_per_source: query.max_results * 2,
            ..Default::default()
        };

        let search_results = self
            .memory_search
            .search(&query.query, &config)
            .await
            .map_err(|error| KnowledgeServiceError::Message(error.to_string()))?;

        let curated = curate_results(&search_results, query.max_results);
        let store = self.memory_search.store();

        let mut hits = Vec::new();
        for result in curated {
            if let Err(error) = store.record_access(&result.memory.id).await {
                tracing::warn!(
                    memory_id = %result.memory.id,
                    %error,
                    "failed to record memory access for knowledge recall"
                );
            }

            let title = crate::summarize_first_non_empty_line(&result.memory.content, 80);
            hits.push(KnowledgeHit {
                id: result.memory.id.clone(),
                title,
                snippet: result.memory.content.clone(),
                content_type: format!("memory/{}", result.memory.memory_type),
                score: result.score,
                provenance: KnowledgeProvenance {
                    source_id: NATIVE_MEMORY_SOURCE_ID.to_string(),
                    source_kind: super::types::KnowledgeSourceKind::NativeMemory,
                    source_label: "Native memory".to_string(),
                    canonical_locator: format!("memory://{}", result.memory.id),
                },
            });
        }

        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::super::qmd::{QmdAdapter, QmdToolResponse};
    use super::KnowledgeRetrievalService;
    use crate::config::{
        DefaultsConfig, McpServerConfig, McpTransport, ResolvedAgentConfig, RuntimeConfig,
    };
    use crate::knowledge::{KnowledgeQuery, KnowledgeSourceStatusState};
    use crate::llm::routing::RoutingConfig;
    use crate::mcp::McpManager;
    use crate::memory::{
        EmbeddingModel, EmbeddingTable, Memory, MemorySearch, MemoryStore, MemoryType,
    };

    use serde_json::json;

    use std::sync::Arc;

    struct ServiceHarness {
        _instance_dir: tempfile::TempDir,
        _lance_dir: tempfile::TempDir,
        service: KnowledgeRetrievalService,
    }

    fn configured_qmd_server(enabled: bool) -> McpServerConfig {
        McpServerConfig {
            name: "qmd".to_string(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:8765/mcp".to_string(),
                headers: std::collections::HashMap::new(),
            },
            enabled,
        }
    }

    fn resolved_agent_config(
        instance_dir: &std::path::Path,
        mcp_servers: Vec<McpServerConfig>,
    ) -> ResolvedAgentConfig {
        let agent_root = instance_dir.join("agents").join("test");
        ResolvedAgentConfig {
            id: "test".to_string(),
            display_name: None,
            role: None,
            workspace: agent_root.join("workspace"),
            data_dir: agent_root.join("data"),
            archives_dir: agent_root.join("archives"),
            routing: RoutingConfig::default(),
            max_concurrent_branches: 2,
            max_concurrent_workers: 2,
            max_turns: 5,
            branch_max_turns: 10,
            context_window: 128_000,
            compaction: crate::config::CompactionConfig::default(),
            memory_persistence: crate::config::MemoryPersistenceConfig::default(),
            coalesce: crate::config::CoalesceConfig::default(),
            ingestion: crate::config::IngestionConfig::default(),
            cortex: crate::config::CortexConfig::default(),
            warmup: crate::config::WarmupConfig::default(),
            browser: crate::config::BrowserConfig::default(),
            channel: crate::config::ChannelConfig::default(),
            mcp: mcp_servers,
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
            sandbox: crate::sandbox::SandboxConfig::default(),
            projects: crate::config::ProjectsConfig::default(),
            history_backfill_count: 50,
            cron: Vec::new(),
        }
    }

    async fn test_service() -> ServiceHarness {
        test_service_with_mcp(Vec::new()).await
    }

    async fn test_service_with_mcp(mcp_servers: Vec<McpServerConfig>) -> ServiceHarness {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let instance_dir = temp_dir.path().join("instance");
        std::fs::create_dir_all(instance_dir.join("agents/test/workspace")).expect("workspace");

        let store = MemoryStore::connect_in_memory().await;
        let memory = Memory::new(
            "Victor keeps design notes in the knowledge plane",
            MemoryType::Fact,
        );
        store.save(&memory).await.expect("save memory");

        let lance_dir = tempfile::tempdir().expect("lance tempdir");
        let lance_conn = lancedb::connect(lance_dir.path().to_str().expect("lance path"))
            .execute()
            .await
            .expect("lance connect");
        let embedding_table = EmbeddingTable::open_or_create(&lance_conn)
            .await
            .expect("embedding table");
        let embedding_model = Arc::new(EmbeddingModel::new(lance_dir.path()).expect("embedding"));
        let embedding = embedding_model
            .embed_one(&memory.content)
            .await
            .expect("memory embedding");
        embedding_table
            .store(&memory.id, &memory.content, &embedding)
            .await
            .expect("store embedding");
        embedding_table
            .ensure_fts_index()
            .await
            .expect("ensure fts index");
        let memory_search = Arc::new(MemorySearch::new(store, embedding_table, embedding_model));

        let runtime_config = Arc::new(RuntimeConfig::new(
            &instance_dir,
            &resolved_agent_config(&instance_dir, mcp_servers),
            &DefaultsConfig::default(),
            crate::prompts::PromptEngine::new("en").expect("prompt engine"),
            crate::identity::Identity::default(),
            crate::skills::SkillSet::default(),
        ));

        ServiceHarness {
            _instance_dir: temp_dir,
            _lance_dir: lance_dir,
            service: KnowledgeRetrievalService::new(
                memory_search,
                runtime_config,
                Arc::new(McpManager::new(Vec::new())),
            ),
        }
    }

    #[tokio::test]
    async fn retrieve_normalizes_native_memory_hits() {
        let harness = test_service().await;
        let result = harness
            .service
            .retrieve(KnowledgeQuery {
                query: "design notes".to_string(),
                max_results: 5,
                source_ids: vec!["native_memory".to_string()],
            })
            .await
            .expect("knowledge recall");

        assert_eq!(result.source_statuses.len(), 1);
        assert_eq!(
            result.source_statuses[0].state,
            KnowledgeSourceStatusState::Used
        );
        assert_eq!(result.hits[0].provenance.source_id, "native_memory");
        assert!(
            result.hits[0]
                .provenance
                .canonical_locator
                .starts_with("memory://")
        );
    }

    #[tokio::test]
    async fn retrieve_unknown_source_is_reported_unavailable() {
        let harness = test_service().await;
        let result = harness
            .service
            .retrieve(KnowledgeQuery {
                query: "design notes".to_string(),
                max_results: 5,
                source_ids: vec!["native_memory".to_string(), "unknown_source".to_string()],
            })
            .await
            .expect("knowledge recall");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.source_statuses.len(), 2);

        let native_status = result
            .source_statuses
            .iter()
            .find(|status| status.source_id == "native_memory")
            .expect("native source status");
        assert_eq!(native_status.state, KnowledgeSourceStatusState::Used);

        let unknown_status = result
            .source_statuses
            .iter()
            .find(|status| status.source_id == "unknown_source")
            .expect("unknown source status");
        assert_eq!(
            unknown_status.state,
            KnowledgeSourceStatusState::Unavailable
        );
        assert_eq!(unknown_status.hit_count, 0);
        assert_eq!(
            unknown_status.message.as_deref(),
            Some("Unknown knowledge source requested.")
        );
    }

    #[tokio::test]
    async fn retrieve_google_workspace_sources_use_bridge_status_messages() {
        let harness = test_service().await;
        let result = harness
            .service
            .retrieve(KnowledgeQuery {
                query: "design notes".to_string(),
                max_results: 5,
                source_ids: vec![
                    "google_workspace_drive".to_string(),
                    "google_workspace_gmail".to_string(),
                    "google_workspace_calendar".to_string(),
                ],
            })
            .await
            .expect("google workspace retrieval");

        assert!(result.hits.is_empty());
        assert_eq!(result.source_statuses.len(), 3);

        for status in result.source_statuses {
            assert_eq!(status.state, KnowledgeSourceStatusState::Unavailable);
            assert_eq!(status.hit_count, 0);
            assert!(
                status
                    .message
                    .as_deref()
                    .is_some_and(|message| !message.to_lowercase().contains("gws"))
            );
            assert!(
                status
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("Google Workspace"))
            );
        }
    }

    #[tokio::test]
    async fn retrieve_normalizes_qmd_hits() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter = QmdAdapter::from_static_result(Ok(QmdToolResponse {
            result_text: "Found 1 result".to_string(),
            structured_content: Some(json!({
                "results": [
                    {
                        "docid": "#abc123",
                        "file": "vault/notes/knowledge-plane.md",
                        "title": "Knowledge Plane",
                        "score": 0.87,
                        "context": "Architecture notes",
                        "snippet": "The retrieval plane keeps native memory authoritative."
                    }
                ]
            })),
        }));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["qmd".to_string()],
            })
            .await
            .expect("qmd knowledge recall");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].provenance.source_id, "qmd");
        assert_eq!(result.hits[0].provenance.source_label, "QMD");
        assert_eq!(
            result.source_statuses[0].state,
            KnowledgeSourceStatusState::Used
        );
        assert_eq!(result.source_statuses[0].message, None);
    }

    #[tokio::test]
    async fn retrieve_qmd_failure_is_reported_as_structured_unavailable() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter =
            QmdAdapter::from_static_result(Err(crate::knowledge::KnowledgeServiceError::Message(
                "QMD MCP server is configured but not connected.".to_string(),
            )));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["qmd".to_string()],
            })
            .await
            .expect("qmd unavailable");

        assert!(result.hits.is_empty());
        assert_eq!(result.source_statuses.len(), 1);
        assert_eq!(
            result.source_statuses[0].state,
            KnowledgeSourceStatusState::Unavailable
        );
        assert_eq!(
            result.source_statuses[0].message.as_deref(),
            Some("QMD MCP server is configured but not connected.")
        );
    }

    #[tokio::test]
    async fn retrieve_disabled_qmd_source_reports_registry_reason_without_calling_adapter() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(false)]).await;
        let qmd_adapter =
            QmdAdapter::from_static_result(Err(crate::knowledge::KnowledgeServiceError::Message(
                "QMD adapter should not be called for disabled sources.".to_string(),
            )));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["qmd".to_string()],
            })
            .await
            .expect("disabled qmd retrieval");

        assert!(result.hits.is_empty());
        assert_eq!(result.source_statuses.len(), 1);
        assert_eq!(
            result.source_statuses[0].state,
            KnowledgeSourceStatusState::Unavailable
        );
        assert_eq!(
            result.source_statuses[0].message.as_deref(),
            Some("QMD is present in config but disabled for this agent.")
        );
    }

    #[tokio::test]
    async fn retrieve_mixed_sources_preserves_both_source_provenances() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter = QmdAdapter::from_static_result(Ok(QmdToolResponse {
            result_text: "Found 1 result".to_string(),
            structured_content: Some(json!({
                "results": [
                    {
                        "docid": "#abc123",
                        "file": "vault/notes/knowledge-plane.md",
                        "title": "Knowledge Plane",
                        "score": 0.87,
                        "context": "Architecture notes",
                        "snippet": "The retrieval plane keeps native memory authoritative."
                    }
                ]
            })),
        }));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["native_memory".to_string(), "qmd".to_string()],
            })
            .await
            .expect("mixed knowledge recall");

        assert_eq!(result.hits.len(), 2);
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.provenance.source_id == "native_memory")
        );
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.provenance.source_id == "qmd")
        );
        assert_eq!(result.source_statuses.len(), 2);
        assert!(
            result
                .source_statuses
                .iter()
                .all(|status| status.state == KnowledgeSourceStatusState::Used)
        );
    }

    #[tokio::test]
    async fn retrieve_mixed_sources_returns_native_hits_when_qmd_is_unavailable() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter =
            QmdAdapter::from_static_result(Err(crate::knowledge::KnowledgeServiceError::Message(
                "QMD MCP server timed out during retrieval.".to_string(),
            )));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["native_memory".to_string(), "qmd".to_string()],
            })
            .await
            .expect("mixed degraded knowledge recall");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].provenance.source_id, "native_memory");
        assert_eq!(result.source_statuses.len(), 2);
        assert!(
            result
                .source_statuses
                .iter()
                .any(|status| status.source_id == "qmd"
                    && status.state == KnowledgeSourceStatusState::Unavailable)
        );
    }

    #[tokio::test]
    async fn retrieve_mixed_sources_returns_qmd_hits_when_native_memory_is_unavailable() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter = QmdAdapter::from_static_result(Ok(QmdToolResponse {
            result_text: "Found 1 result".to_string(),
            structured_content: Some(json!({
                "results": [
                    {
                        "docid": "#abc123",
                        "file": "vault/notes/knowledge-plane.md",
                        "title": "Knowledge Plane",
                        "score": 0.87,
                        "context": "Architecture notes",
                        "snippet": "The retrieval plane keeps native memory authoritative."
                    }
                ]
            })),
        }));
        let service = harness
            .service
            .with_forced_memory_failure("Native memory search is temporarily unavailable.")
            .with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["native_memory".to_string(), "qmd".to_string()],
            })
            .await
            .expect("mixed degraded knowledge recall");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].provenance.source_id, "qmd");
        assert_eq!(result.source_statuses.len(), 2);
        assert!(
            result
                .source_statuses
                .iter()
                .any(|status| status.source_id == "native_memory"
                    && status.state == KnowledgeSourceStatusState::Unavailable
                    && status.message.as_deref()
                        == Some("Native memory search is temporarily unavailable."))
        );
        assert!(
            result
                .source_statuses
                .iter()
                .any(|status| status.source_id == "qmd"
                    && status.state == KnowledgeSourceStatusState::Used)
        );
    }

    #[tokio::test]
    async fn retrieve_qmd_partial_normalization_reports_dropped_row_count() {
        let harness = test_service_with_mcp(vec![configured_qmd_server(true)]).await;
        let qmd_adapter = QmdAdapter::from_static_result(Ok(QmdToolResponse {
            result_text: "Found 2 results".to_string(),
            structured_content: Some(json!({
                "results": [
                    {
                        "docid": "#abc123",
                        "file": "vault/notes/knowledge-plane.md",
                        "title": "Knowledge Plane",
                        "score": 0.87,
                        "context": "Architecture notes",
                        "snippet": "The retrieval plane keeps native memory authoritative."
                    },
                    {
                        "docid": "#broken",
                        "file": "",
                        "title": "Broken",
                        "score": 0.52,
                        "snippet": "missing file should be dropped"
                    }
                ]
            })),
        }));
        let service = harness.service.with_qmd_adapter(qmd_adapter);

        let result = service
            .retrieve(KnowledgeQuery {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: vec!["qmd".to_string()],
            })
            .await
            .expect("qmd knowledge recall");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(
            result.source_statuses[0].message.as_deref(),
            Some("Dropped 1 malformed QMD result(s).")
        );
    }
}
