//! Memory recall tool for branches.

use crate::config::RuntimeConfig;
use crate::error::Result;
use crate::memory::MemorySearch;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort, curate_results};
use crate::memory::types::{Memory, MemorySearchResult, MemoryType};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeSet;
use std::sync::Arc;

/// Tool for recalling memories using hybrid search.
#[derive(Debug, Clone)]
pub struct MemoryRecallTool {
    memory_search: Arc<MemorySearch>,
    runtime_config: Option<Arc<RuntimeConfig>>,
}

impl MemoryRecallTool {
    /// Create a new memory recall tool.
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self {
            memory_search,
            runtime_config: None,
        }
    }

    /// Create a memory recall tool with runtime warm-recall cache support.
    pub fn with_runtime(
        memory_search: Arc<MemorySearch>,
        runtime_config: Arc<RuntimeConfig>,
    ) -> Self {
        Self {
            memory_search,
            runtime_config: Some(runtime_config),
        }
    }

    async fn warm_cache_results(
        &self,
        query: &str,
        memory_type: Option<MemoryType>,
        max_results: usize,
    ) -> Vec<MemorySearchResult> {
        let Some(runtime_config) = self.runtime_config.as_ref() else {
            return Vec::new();
        };
        let _warm_recall_cache_guard = runtime_config.warm_recall_cache_lock.lock().await;

        let now_unix_ms = chrono::Utc::now().timestamp_millis();
        let refreshed_at_unix_ms = *runtime_config
            .warm_recall_refreshed_at_unix_ms
            .load()
            .as_ref();
        let Some(age_secs) = warm_cache_age_secs(refreshed_at_unix_ms, now_unix_ms) else {
            return Vec::new();
        };
        let warmup_refresh_secs = runtime_config.warmup.load().as_ref().refresh_secs.max(1);
        if age_secs > warmup_refresh_secs {
            return Vec::new();
        }

        let warm_memories = runtime_config.warm_recall_memories.load();
        let inflight_forget_ids = snapshot_inflight_forget_ids(runtime_config);
        score_warm_memories(
            query,
            warm_memories.as_ref(),
            memory_type,
            max_results,
            &inflight_forget_ids,
        )
    }
}

fn snapshot_inflight_forget_ids(runtime_config: &RuntimeConfig) -> BTreeSet<String> {
    let counts = match runtime_config.warm_recall_inflight_forget_counts.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("warm recall inflight forget counts lock poisoned; recovering state");
            poisoned.into_inner()
        }
    };
    counts.keys().cloned().collect()
}

fn warm_cache_age_secs(refreshed_at_unix_ms: Option<i64>, now_unix_ms: i64) -> Option<u64> {
    refreshed_at_unix_ms.map(|refresh_ms| {
        if now_unix_ms > refresh_ms {
            ((now_unix_ms - refresh_ms) / 1000) as u64
        } else {
            0
        }
    })
}

fn tokenize_query_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(|term| term.to_lowercase())
        .collect()
}

fn score_warm_memories(
    query: &str,
    warm_memories: &[Memory],
    memory_type: Option<MemoryType>,
    max_results: usize,
    inflight_forget_ids: &BTreeSet<String>,
) -> Vec<MemorySearchResult> {
    if max_results == 0 {
        return Vec::new();
    }

    let query_terms = tokenize_query_terms(query);
    if query_terms.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    for memory in warm_memories {
        if let Some(score) =
            score_memory_with_terms(&query_terms, memory, memory_type, inflight_forget_ids)
        {
            results.push(MemorySearchResult {
                memory: memory.clone(),
                score,
                rank: 0,
            });
        }
    }

    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.memory.importance.total_cmp(&left.memory.importance))
    });
    results.truncate(max_results);

    for (index, result) in results.iter_mut().enumerate() {
        result.rank = index + 1;
    }

    results
}

fn score_memory_with_terms(
    query_terms: &[String],
    memory: &Memory,
    memory_type: Option<MemoryType>,
    inflight_forget_ids: &BTreeSet<String>,
) -> Option<f32> {
    if memory.forgotten {
        return None;
    }

    if inflight_forget_ids.contains(&memory.id) {
        return None;
    }

    if memory_type.is_some_and(|kind| memory.memory_type != kind) {
        return None;
    }

    let content = memory.content.to_lowercase();
    let matched_terms = query_terms
        .iter()
        .filter(|term| content.contains(term.as_str()))
        .count();
    if matched_terms == 0 {
        return None;
    }

    let coverage = matched_terms as f32 / query_terms.len() as f32;
    Some(coverage * 0.85 + memory.importance.clamp(0.0, 1.0) * 0.15)
}

fn select_hybrid_results(
    warm_results: Vec<MemorySearchResult>,
    search_result: std::result::Result<Vec<MemorySearchResult>, MemoryRecallError>,
) -> std::result::Result<(Vec<MemorySearchResult>, Option<String>), MemoryRecallError> {
    match search_result {
        Ok(search_results) => Ok((search_results, None)),
        Err(error) => {
            if warm_results.is_empty() {
                return Err(error);
            }
            let search_error = error.0;
            tracing::warn!(
                error = %search_error,
                warm_matches = warm_results.len(),
                "hybrid search failed, returning partial warm recall results"
            );
            Ok((warm_results, Some(search_error)))
        }
    }
}

/// Error type for memory recall tool.
#[derive(Debug, thiserror::Error)]
#[error("Memory recall failed: {0}")]
pub struct MemoryRecallError(String);

impl From<crate::error::Error> for MemoryRecallError {
    fn from(e: crate::error::Error) -> Self {
        MemoryRecallError(format!("{e}"))
    }
}

/// Arguments for memory recall tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryRecallArgs {
    /// The search query. Required for hybrid mode, ignored for recent/important/typed modes.
    #[serde(default)]
    pub query: Option<String>,
    /// Maximum number of results to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Optional memory type filter. Required for "typed" mode.
    pub memory_type: Option<String>,
    /// Search mode: "hybrid" (default), "recent", "important", "typed".
    #[serde(default)]
    pub mode: Option<String>,
    /// Sort order for non-hybrid modes: "recent" (default), "importance", "most_accessed".
    #[serde(default)]
    pub sort_by: Option<String>,
}

fn default_max_results() -> usize {
    10
}

fn parse_search_mode(s: &str) -> std::result::Result<SearchMode, MemoryRecallError> {
    match s {
        "hybrid" => Ok(SearchMode::Hybrid),
        "recent" => Ok(SearchMode::Recent),
        "important" => Ok(SearchMode::Important),
        "typed" => Ok(SearchMode::Typed),
        other => Err(MemoryRecallError(format!(
            "unknown mode \"{other}\". Valid modes: hybrid, recent, important, typed"
        ))),
    }
}

fn parse_search_sort(s: &str) -> std::result::Result<SearchSort, MemoryRecallError> {
    match s {
        "recent" => Ok(SearchSort::Recent),
        "importance" => Ok(SearchSort::Importance),
        "most_accessed" => Ok(SearchSort::MostAccessed),
        other => Err(MemoryRecallError(format!(
            "unknown sort \"{other}\". Valid sorts: recent, importance, most_accessed"
        ))),
    }
}

fn parse_memory_type(s: &str) -> std::result::Result<crate::memory::MemoryType, MemoryRecallError> {
    use crate::memory::MemoryType;
    match s {
        "fact" => Ok(MemoryType::Fact),
        "preference" => Ok(MemoryType::Preference),
        "decision" => Ok(MemoryType::Decision),
        "identity" => Ok(MemoryType::Identity),
        "event" => Ok(MemoryType::Event),
        "observation" => Ok(MemoryType::Observation),
        "goal" => Ok(MemoryType::Goal),
        "todo" => Ok(MemoryType::Todo),
        other => Err(MemoryRecallError(format!(
            "unknown memory_type \"{other}\". Valid types: {}",
            crate::memory::MemoryType::ALL
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Output from memory recall tool.
#[derive(Debug, Serialize)]
pub struct MemoryRecallOutput {
    /// The memories found by the search.
    pub memories: Vec<MemoryOutput>,
    /// Total number of results found before curation.
    pub total_found: usize,
    /// True when hybrid search failed and warm cache fallback was returned.
    pub degraded_fallback_used: bool,
    /// Root-cause error from hybrid search when degraded fallback was used.
    pub degraded_fallback_error: Option<String>,
    /// Formatted summary of the memories.
    pub summary: String,
}

/// Simplified memory output for serialization.
#[derive(Debug, Serialize)]
pub struct MemoryOutput {
    /// The memory ID.
    pub id: String,
    /// The memory content.
    pub content: String,
    /// The memory type.
    pub memory_type: String,
    /// The importance score.
    pub importance: f32,
    /// When the memory was created.
    pub created_at: String,
    /// The relevance score from the search.
    pub relevance_score: f32,
}

impl Tool for MemoryRecallTool {
    const NAME: &'static str = "memory_recall";

    type Error = MemoryRecallError;
    type Args = MemoryRecallArgs;
    type Output = MemoryRecallOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_recall").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for hybrid mode. Required for hybrid, ignored for other modes."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 10,
                        "description": "Maximum number of memories to return (1-50)"
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": crate::memory::types::MemoryType::ALL
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>(),
                        "description": "Filter to a specific memory type. Required for \"typed\" mode, optional filter for other modes."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hybrid", "recent", "important", "typed"],
                        "default": "hybrid",
                        "description": "Search mode. \"hybrid\": semantic + keyword + graph (needs query). \"recent\": most recent by time. \"important\": highest importance. \"typed\": filter by memory_type."
                    },
                    "sort_by": {
                        "type": "string",
                        "enum": ["recent", "importance", "most_accessed"],
                        "default": "recent",
                        "description": "Sort order for non-hybrid modes. Default: recent."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let mode = match args.mode.as_deref() {
            Some(m) => parse_search_mode(m)?,
            None => SearchMode::Hybrid,
        };

        let sort_by = match args.sort_by.as_deref() {
            Some(s) => parse_search_sort(s)?,
            None => SearchSort::Recent,
        };

        let memory_type = args
            .memory_type
            .as_deref()
            .map(parse_memory_type)
            .transpose()?;

        // Validate mode-specific requirements
        if mode == SearchMode::Hybrid && args.query.as_ref().is_none_or(|q| q.is_empty()) {
            return Err(MemoryRecallError(
                "hybrid mode requires a non-empty query".to_string(),
            ));
        }
        if mode == SearchMode::Typed && memory_type.is_none() {
            return Err(MemoryRecallError(
                "typed mode requires a memory_type filter".to_string(),
            ));
        }

        let config = SearchConfig {
            mode,
            memory_type,
            sort_by,
            max_results: args.max_results,
            max_results_per_source: args.max_results * 2,
            ..Default::default()
        };

        let query = args.query.as_deref().unwrap_or("");
        let (search_results, degraded_fallback_error) = if mode == SearchMode::Hybrid {
            let search_result = self
                .memory_search
                .search(query, &config)
                .await
                .map_err(|e| MemoryRecallError(format!("Search failed: {e}")));
            let warm_results = if search_result.is_err() {
                self.warm_cache_results(query, memory_type, config.max_results_per_source)
                    .await
            } else {
                Vec::new()
            };
            let (selected, degraded_fallback_error) =
                select_hybrid_results(warm_results, search_result)?;
            if selected.len() < args.max_results {
                tracing::debug!(
                    matched = selected.len(),
                    requested = args.max_results,
                    "memory recall returned partial warm or hybrid results"
                );
            }
            (selected, degraded_fallback_error)
        } else {
            let search_results = self
                .memory_search
                .search(query, &config)
                .await
                .map_err(|e| MemoryRecallError(format!("Search failed: {e}")))?;
            (search_results, None)
        };

        let curated = curate_results(&search_results, args.max_results);

        let store = self.memory_search.store();
        let mut memories = Vec::new();

        for result in &curated {
            if let Err(error) = store.record_access(&result.memory.id).await {
                tracing::warn!(
                    memory_id = %result.memory.id,
                    %error,
                    "failed to record memory access"
                );
            }

            memories.push(MemoryOutput {
                id: result.memory.id.clone(),
                content: result.memory.content.clone(),
                memory_type: result.memory.memory_type.to_string(),
                importance: result.memory.importance,
                created_at: result.memory.created_at.to_rfc3339(),
                relevance_score: result.score,
            });
        }

        let total_found = search_results.len();
        let summary = format_memories(&memories);

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global().memory_reads_total.inc();

        Ok(MemoryRecallOutput {
            memories,
            total_found,
            degraded_fallback_used: degraded_fallback_error.is_some(),
            degraded_fallback_error,
            summary,
        })
    }
}

/// Format memories for display to an agent.
pub fn format_memories(memories: &[MemoryOutput]) -> String {
    if memories.is_empty() {
        return "No relevant memories found.".to_string();
    }

    let mut output = String::from("## Relevant Memories\n\n");

    for (i, memory) in memories.iter().enumerate() {
        let preview = memory.content.lines().next().unwrap_or(&memory.content);
        output.push_str(&format!(
            "{}. [{}] (importance: {:.2}, relevance: {:.2})\n   {}\n\n",
            i + 1,
            memory.memory_type,
            memory.importance,
            memory.relevance_score,
            preview
        ));
    }

    output
}

/// Legacy convenience function for direct memory recall.
pub async fn memory_recall(
    memory_search: Arc<MemorySearch>,
    query: &str,
    max_results: usize,
) -> Result<Vec<Memory>> {
    let tool = MemoryRecallTool::new(Arc::clone(&memory_search));
    let args = MemoryRecallArgs {
        query: Some(query.to_string()),
        max_results,
        memory_type: None,
        mode: None,
        sort_by: None,
    };

    let output = tool
        .call(args)
        .await
        .map_err(|e| crate::error::AgentError::Other(anyhow::anyhow!(e)))?;

    // Convert back to Memory type for backward compatibility
    let store = memory_search.store();
    let mut memories = Vec::new();

    for mem_out in output.memories {
        if let Ok(Some(memory)) = store.load(&mem_out.id).await {
            memories.push(memory);
        }
    }

    Ok(memories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryType;

    #[test]
    fn test_parse_search_mode_valid() {
        assert_eq!(parse_search_mode("hybrid").unwrap(), SearchMode::Hybrid);
        assert_eq!(parse_search_mode("recent").unwrap(), SearchMode::Recent);
        assert_eq!(
            parse_search_mode("important").unwrap(),
            SearchMode::Important
        );
        assert_eq!(parse_search_mode("typed").unwrap(), SearchMode::Typed);
    }

    #[test]
    fn test_parse_search_mode_invalid() {
        assert!(parse_search_mode("invalid").is_err());
        assert!(parse_search_mode("").is_err());
    }

    #[test]
    fn test_parse_search_sort_valid() {
        assert_eq!(parse_search_sort("recent").unwrap(), SearchSort::Recent);
        assert_eq!(
            parse_search_sort("importance").unwrap(),
            SearchSort::Importance
        );
        assert_eq!(
            parse_search_sort("most_accessed").unwrap(),
            SearchSort::MostAccessed
        );
    }

    #[test]
    fn test_parse_search_sort_invalid() {
        assert!(parse_search_sort("invalid").is_err());
    }

    #[test]
    fn test_parse_memory_type_valid() {
        use crate::memory::MemoryType;
        assert_eq!(parse_memory_type("fact").unwrap(), MemoryType::Fact);
        assert_eq!(parse_memory_type("identity").unwrap(), MemoryType::Identity);
        assert_eq!(parse_memory_type("decision").unwrap(), MemoryType::Decision);
        assert_eq!(parse_memory_type("goal").unwrap(), MemoryType::Goal);
        assert_eq!(parse_memory_type("todo").unwrap(), MemoryType::Todo);
    }

    #[test]
    fn test_parse_memory_type_invalid() {
        assert!(parse_memory_type("invalid").is_err());
    }

    #[test]
    fn test_warm_cache_age_secs_none_stays_none() {
        assert_eq!(warm_cache_age_secs(None, 10_000), None);
    }

    #[test]
    fn test_warm_cache_age_secs_clamps_future_to_zero() {
        assert_eq!(warm_cache_age_secs(Some(5_000), 4_000), Some(0));
    }

    #[test]
    fn test_score_warm_memories_prefers_stronger_term_match() {
        let mut auth_memory = Memory::new("auth token rotation policy", MemoryType::Fact);
        auth_memory.importance = 0.4;
        let mut cache_memory = Memory::new("cache eviction details", MemoryType::Fact);
        cache_memory.importance = 0.9;

        let results = score_warm_memories(
            "auth token",
            &[cache_memory, auth_memory.clone()],
            None,
            10,
            &BTreeSet::new(),
        );

        assert!(!results.is_empty());
        assert_eq!(results[0].memory.id, auth_memory.id);
    }

    #[test]
    fn test_score_warm_memories_applies_memory_type_filter() {
        let fact_memory = Memory::new("auth strategy", MemoryType::Fact);
        let decision_memory = Memory::new("auth decision", MemoryType::Decision);

        let results = score_warm_memories(
            "auth decision",
            &[fact_memory, decision_memory.clone()],
            Some(MemoryType::Decision),
            10,
            &BTreeSet::new(),
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.memory_type, MemoryType::Decision);
        assert_eq!(results[0].memory.id, decision_memory.id);
    }

    #[test]
    fn test_score_warm_memories_returns_empty_for_non_word_query() {
        let memory = Memory::new("auth strategy", MemoryType::Fact);
        let results = score_warm_memories("...", &[memory], None, 10, &BTreeSet::new());
        assert!(results.is_empty());
    }

    #[test]
    fn test_score_warm_memories_excludes_inflight_forget_ids() {
        let memory = Memory::new("auth strategy", MemoryType::Fact);
        let mut inflight_forget_ids = BTreeSet::new();
        inflight_forget_ids.insert(memory.id.clone());

        let results = score_warm_memories("auth", &[memory], None, 10, &inflight_forget_ids);
        assert!(results.is_empty());
    }

    #[test]
    fn test_select_hybrid_results_uses_partial_warm_results_on_search_error() {
        let warm_memory = Memory::new("auth strategy", MemoryType::Fact);
        let warm_results = vec![MemorySearchResult {
            memory: warm_memory,
            score: 0.9,
            rank: 1,
        }];

        let selected = select_hybrid_results(
            warm_results.clone(),
            Err(MemoryRecallError("Search failed: boom".to_string())),
        )
        .expect("expected warm fallback");

        assert_eq!(selected.0.len(), warm_results.len());
        assert_eq!(selected.0[0].rank, warm_results[0].rank);
        assert_eq!(selected.1, Some("Search failed: boom".to_string()));
    }

    #[test]
    fn test_select_hybrid_results_prefers_search_results_when_search_succeeds() {
        let warm_memory = Memory::new("warm cache result", MemoryType::Fact);
        let search_memory = Memory::new("hybrid result", MemoryType::Fact);
        let warm_results = vec![MemorySearchResult {
            memory: warm_memory,
            score: 0.95,
            rank: 1,
        }];
        let search_results = vec![MemorySearchResult {
            memory: search_memory.clone(),
            score: 0.5,
            rank: 1,
        }];

        let selected =
            select_hybrid_results(warm_results, Ok(search_results.clone())).expect("select");

        assert_eq!(selected.0.len(), 1);
        assert_eq!(selected.0[0].memory.id, search_memory.id);
        assert!(selected.1.is_none());
    }

    #[test]
    fn test_select_hybrid_results_returns_error_when_search_fails_and_warm_is_empty() {
        let result = select_hybrid_results(
            Vec::new(),
            Err(MemoryRecallError("Search failed: boom".to_string())),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_score_memory_with_terms_filters_type_and_content() {
        let query_terms = vec!["auth".to_string()];
        let memory = Memory::new("auth strategy", MemoryType::Fact);
        let wrong_type_score = score_memory_with_terms(
            &query_terms,
            &memory,
            Some(MemoryType::Decision),
            &BTreeSet::new(),
        );
        let no_match_score = score_memory_with_terms(
            &["billing".to_string()],
            &memory,
            Some(MemoryType::Fact),
            &BTreeSet::new(),
        );
        let match_score = score_memory_with_terms(
            &query_terms,
            &memory,
            Some(MemoryType::Fact),
            &BTreeSet::new(),
        );

        assert!(wrong_type_score.is_none());
        assert!(no_match_score.is_none());
        assert!(match_score.is_some());
    }

    #[test]
    fn test_score_memory_with_terms_filters_inflight_forget() {
        let query_terms = vec!["auth".to_string()];
        let memory = Memory::new("auth strategy", MemoryType::Fact);
        let mut inflight_forget_ids = BTreeSet::new();
        inflight_forget_ids.insert(memory.id.clone());

        let score = score_memory_with_terms(
            &query_terms,
            &memory,
            Some(MemoryType::Fact),
            &inflight_forget_ids,
        );

        assert!(score.is_none());
    }
}
