//! Retrieval service for normalized knowledge lookups.

use crate::config::RuntimeConfig;
use crate::knowledge::registry::NATIVE_MEMORY_SOURCE_ID;
use crate::memory::MemorySearch;
use crate::memory::search::{SearchConfig, SearchMode, curate_results};

use std::sync::Arc;

use super::registry::KnowledgeSourceRegistry;
use super::types::{
    KnowledgeHit, KnowledgeProvenance, KnowledgeQuery, KnowledgeRetrievalResult,
    KnowledgeSourceEnablementState, KnowledgeSourceStatus, KnowledgeSourceStatusState,
};

/// Error type for the knowledge retrieval service.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeServiceError {
    #[error("knowledge retrieval failed: {0}")]
    Message(String),
}

/// Service that mediates native memory retrieval and source registry state.
#[derive(Debug, Clone)]
pub struct KnowledgeRetrievalService {
    memory_search: Arc<MemorySearch>,
    runtime_config: Arc<RuntimeConfig>,
}

impl KnowledgeRetrievalService {
    pub fn new(memory_search: Arc<MemorySearch>, runtime_config: Arc<RuntimeConfig>) -> Self {
        Self {
            memory_search,
            runtime_config,
        }
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
                let source_hits = self.retrieve_memory_hits(&query).await?;
                source_statuses.push(KnowledgeSourceStatus {
                    source_id: source.id.clone(),
                    state: KnowledgeSourceStatusState::Used,
                    hit_count: source_hits.len(),
                    message: None,
                });
                hits.extend(source_hits);
                continue;
            }

            let message = match source.enablement.state {
                KnowledgeSourceEnablementState::Configured => {
                    source.enablement.reason.clone().or(Some(
                        "Source is configured but the native adapter is not wired yet.".to_string(),
                    ))
                }
                KnowledgeSourceEnablementState::Placeholder => source.enablement.reason.clone(),
                KnowledgeSourceEnablementState::Disabled => source.enablement.reason.clone(),
                KnowledgeSourceEnablementState::Enabled => Some(
                    "Source is marked enabled but no native adapter is available yet.".to_string(),
                ),
            };
            source_statuses.push(KnowledgeSourceStatus {
                source_id: source.id.clone(),
                state: KnowledgeSourceStatusState::Unavailable,
                hit_count: 0,
                message,
            });
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
    use super::KnowledgeRetrievalService;
    use crate::config::{DefaultsConfig, ResolvedAgentConfig, RuntimeConfig};
    use crate::knowledge::{KnowledgeQuery, KnowledgeSourceStatusState};
    use crate::llm::routing::RoutingConfig;
    use crate::memory::{
        EmbeddingModel, EmbeddingTable, Memory, MemorySearch, MemoryStore, MemoryType,
    };

    use std::sync::Arc;

    struct ServiceHarness {
        _instance_dir: tempfile::TempDir,
        _lance_dir: tempfile::TempDir,
        service: KnowledgeRetrievalService,
    }

    fn resolved_agent_config(instance_dir: &std::path::Path) -> ResolvedAgentConfig {
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
            mcp: Vec::new(),
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
            &resolved_agent_config(&instance_dir),
            &DefaultsConfig::default(),
            crate::prompts::PromptEngine::new("en").expect("prompt engine"),
            crate::identity::Identity::default(),
            crate::skills::SkillSet::default(),
        ));

        ServiceHarness {
            _instance_dir: temp_dir,
            _lance_dir: lance_dir,
            service: KnowledgeRetrievalService::new(memory_search, runtime_config),
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
        assert_eq!(unknown_status.state, KnowledgeSourceStatusState::Unavailable);
        assert_eq!(unknown_status.hit_count, 0);
        assert_eq!(
            unknown_status.message.as_deref(),
            Some("Unknown knowledge source requested.")
        );
    }
}
