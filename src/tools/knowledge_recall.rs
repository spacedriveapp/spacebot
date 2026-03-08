//! Knowledge recall tool for branches.

use crate::knowledge::{
    KnowledgeHit, KnowledgeQuery, KnowledgeRetrievalService, KnowledgeServiceError,
    KnowledgeSourceDescriptor, KnowledgeSourceStatus,
};
use crate::mcp::McpManager;
use crate::memory::MemorySearch;
use crate::tools::truncate_utf8_ellipsis;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for normalized knowledge retrieval across native and external sources.
#[derive(Debug, Clone)]
pub struct KnowledgeRecallTool {
    service: KnowledgeRetrievalService,
}

impl KnowledgeRecallTool {
    pub fn new(
        memory_search: Arc<MemorySearch>,
        runtime_config: Arc<crate::config::RuntimeConfig>,
        mcp_manager: Arc<McpManager>,
    ) -> Self {
        Self {
            service: KnowledgeRetrievalService::new(memory_search, runtime_config, mcp_manager),
        }
    }

    #[cfg(test)]
    pub fn with_service(service: KnowledgeRetrievalService) -> Self {
        Self { service }
    }
}

/// Error type for knowledge_recall tool.
#[derive(Debug, thiserror::Error)]
#[error("knowledge_recall failed: {0}")]
pub struct KnowledgeRecallError(String);

impl From<KnowledgeServiceError> for KnowledgeRecallError {
    fn from(error: KnowledgeServiceError) -> Self {
        Self(error.to_string())
    }
}

/// Arguments for knowledge_recall.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KnowledgeRecallArgs {
    /// The search query. Required.
    pub query: String,
    /// Maximum number of normalized hits to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Optional source list. Defaults to enabled and configured branch-visible sources.
    #[serde(default)]
    pub source_ids: Vec<String>,
}

fn default_max_results() -> usize {
    10
}

/// Output for knowledge_recall.
#[derive(Debug, Serialize)]
pub struct KnowledgeRecallOutput {
    pub query: String,
    pub hits: Vec<KnowledgeHit>,
    pub source_statuses: Vec<KnowledgeSourceStatus>,
    pub available_sources: Vec<KnowledgeSourceDescriptor>,
    pub summary: String,
}

impl Tool for KnowledgeRecallTool {
    const NAME: &'static str = "knowledge_recall";

    type Error = KnowledgeRecallError;
    type Args = KnowledgeRecallArgs;
    type Output = KnowledgeRecallOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/knowledge_recall").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to search for across native memory and future external knowledge sources."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 10,
                        "description": "Maximum number of normalized hits to return (1-50)."
                    },
                    "source_ids": {
                        "type": "array",
                        "description": "Optional source IDs to constrain retrieval. Examples: native_memory, qmd, google_workspace_drive.",
                        "items": { "type": "string" }
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(KnowledgeRecallError(
                "query must be a non-empty string".to_string(),
            ));
        }

        let max_results = args.max_results.clamp(1, 50);
        let result = self
            .service
            .retrieve(KnowledgeQuery {
                query: query.clone(),
                max_results,
                source_ids: args.source_ids,
            })
            .await?;
        let available_sources = self.service.registry().sources().to_vec();
        let summary = format_summary(&result.hits, &result.source_statuses);

        Ok(KnowledgeRecallOutput {
            query,
            hits: result.hits,
            source_statuses: result.source_statuses,
            available_sources,
            summary,
        })
    }
}

fn format_summary(hits: &[KnowledgeHit], source_statuses: &[KnowledgeSourceStatus]) -> String {
    let hit_summary = if hits.is_empty() {
        "No knowledge hits found.".to_string()
    } else {
        let labels = hits
            .iter()
            .map(|hit| format!("{} ({})", hit.title, hit.provenance.source_id))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "Found {} knowledge hit(s): {}",
            hits.len(),
            truncate_utf8_ellipsis(&labels, 400)
        )
    };

    if source_statuses.is_empty() {
        return hit_summary;
    }

    let source_summary = source_statuses
        .iter()
        .map(|status| match &status.message {
            Some(message) => format!(
                "{}={} ({} hit(s); {})",
                status.source_id,
                serde_json::to_string(&status.state)
                    .unwrap_or_else(|_| "\"unknown\"".to_string())
                    .trim_matches('"'),
                status.hit_count,
                message
            ),
            None => format!(
                "{}={} ({} hit(s))",
                status.source_id,
                serde_json::to_string(&status.state)
                    .unwrap_or_else(|_| "\"unknown\"".to_string())
                    .trim_matches('"'),
                status.hit_count
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");

    format!("{hit_summary}\nSources: {source_summary}")
}

#[cfg(test)]
mod tests {
    use super::{KnowledgeRecallArgs, KnowledgeRecallTool};
    use crate::config::{
        DefaultsConfig, McpServerConfig, McpTransport, ResolvedAgentConfig, RuntimeConfig,
    };
    use crate::knowledge::{KnowledgeRetrievalService, QmdAdapter, QmdToolResponse};
    use crate::llm::routing::RoutingConfig;
    use crate::mcp::McpManager;
    use crate::memory::{
        EmbeddingModel, EmbeddingTable, Memory, MemorySearch, MemoryStore, MemoryType,
    };

    use rig::tool::Tool as _;
    use serde_json::json;

    use std::sync::Arc;

    struct ToolHarness {
        _instance_dir: tempfile::TempDir,
        _lance_dir: tempfile::TempDir,
        tool: KnowledgeRecallTool,
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
            mcp: vec![McpServerConfig {
                name: "qmd".to_string(),
                transport: McpTransport::Http {
                    url: "http://127.0.0.1:8765/mcp".to_string(),
                    headers: std::collections::HashMap::new(),
                },
                enabled: true,
            }],
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
            sandbox: crate::sandbox::SandboxConfig::default(),
            projects: crate::config::ProjectsConfig::default(),
            history_backfill_count: 50,
            cron: Vec::new(),
        }
    }

    async fn test_tool() -> ToolHarness {
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
        let service = KnowledgeRetrievalService::new(
            memory_search,
            runtime_config,
            Arc::new(McpManager::new(Vec::new())),
        )
        .with_qmd_adapter(qmd_adapter);

        ToolHarness {
            _instance_dir: temp_dir,
            _lance_dir: lance_dir,
            tool: KnowledgeRecallTool::with_service(service),
        }
    }

    #[tokio::test]
    async fn knowledge_recall_uses_configured_qmd_in_default_source_set() {
        let harness = test_tool().await;
        let output = harness
            .tool
            .call(KnowledgeRecallArgs {
                query: "knowledge plane".to_string(),
                max_results: 5,
                source_ids: Vec::new(),
            })
            .await
            .expect("knowledge recall");

        assert!(
            output
                .hits
                .iter()
                .any(|hit| hit.provenance.source_id == "qmd")
        );
        assert!(
            output
                .available_sources
                .iter()
                .any(|source| source.id == "qmd")
        );
        assert!(output.summary.contains("qmd"));
        assert!(!output.summary.contains("query"));
    }
}
