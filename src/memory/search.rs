//! Memory search: hybrid (vector + FTS + RRF + graph), temporal, importance, and typed queries.

use crate::error::Result;
use crate::memory::types::{Memory, MemorySearchResult, MemoryType, RelationType};
use crate::memory::{EmbeddingModel, EmbeddingTable, MemoryStore};

use std::collections::HashMap;
use std::sync::Arc;

/// Which search strategy to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Full hybrid: vector + FTS + graph + RRF. Requires a query string.
    #[default]
    Hybrid,
    /// Most recent memories by creation time. No query needed.
    Recent,
    /// Highest importance memories. No query needed.
    Important,
    /// Filter by MemoryType with configurable sort. Requires `memory_type`.
    Typed,
}

/// Sort order for non-hybrid search modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchSort {
    /// Most recent first (created_at DESC).
    #[default]
    Recent,
    /// Highest importance first (importance DESC).
    Importance,
    /// Most accessed first (access_count DESC).
    MostAccessed,
}

/// Bundles all memory search dependencies.
pub struct MemorySearch {
    store: Arc<MemoryStore>,
    embedding_table: EmbeddingTable,
    embedding_model: Arc<EmbeddingModel>,
}

impl Clone for MemorySearch {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            embedding_table: self.embedding_table.clone(),
            embedding_model: Arc::clone(&self.embedding_model),
        }
    }
}

impl std::fmt::Debug for MemorySearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySearch")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl MemorySearch {
    /// Create a new MemorySearch instance.
    pub fn new(
        store: Arc<MemoryStore>,
        embedding_table: EmbeddingTable,
        embedding_model: Arc<EmbeddingModel>,
    ) -> Self {
        Self {
            store,
            embedding_table,
            embedding_model,
        }
    }

    /// Get a reference to the memory store.
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// Get a reference to the embedding table.
    pub fn embedding_table(&self) -> &EmbeddingTable {
        &self.embedding_table
    }

    /// Get a reference to the embedding model.
    pub fn embedding_model(&self) -> &EmbeddingModel {
        &self.embedding_model
    }

    /// Get a shared handle to the embedding model (for async embed_one).
    pub fn embedding_model_arc(&self) -> &Arc<EmbeddingModel> {
        &self.embedding_model
    }

    /// Unified search entry point. Dispatches to the appropriate strategy
    /// based on `config.mode`.
    pub async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<MemorySearchResult>> {
        match config.mode {
            SearchMode::Hybrid => self.hybrid_search(query, config).await,
            SearchMode::Recent => self.metadata_search(SearchSort::Recent, config).await,
            SearchMode::Important => self.metadata_search(SearchSort::Importance, config).await,
            SearchMode::Typed => self.metadata_search(config.sort_by, config).await,
        }
    }

    /// Metadata-based search: queries SQLite directly, no vector/FTS/RRF.
    /// Used by Recent, Important, and Typed modes.
    async fn metadata_search(
        &self,
        sort: SearchSort,
        config: &SearchConfig,
    ) -> Result<Vec<MemorySearchResult>> {
        let memories = self
            .store
            .get_sorted(sort, config.max_results as i64, config.memory_type)
            .await?;

        let total = memories.len();
        let results = memories
            .into_iter()
            .enumerate()
            .map(|(rank, memory)| {
                // Normalized positional score so output type is consistent.
                // First result gets 1.0, decays linearly.
                let score = if total > 1 {
                    1.0 - (rank as f32 / total as f32)
                } else {
                    1.0
                };
                MemorySearchResult {
                    memory,
                    score,
                    rank: rank + 1,
                }
            })
            .collect();

        Ok(results)
    }

    /// Perform hybrid search across all memory sources.
    pub async fn hybrid_search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<MemorySearchResult>> {
        // Collect results from different sources
        let mut vector_results = Vec::new();
        let mut fts_results = Vec::new();
        let mut graph_results = Vec::new();

        // 1. Full-text search via LanceDB
        // FTS requires an inverted index. If the index doesn't exist yet (empty
        // table, first run) this will fail — fall back to vector + graph search.
        match self
            .embedding_table
            .text_search(query, config.max_results_per_source)
            .await
        {
            Ok(fts_matches) => {
                for (memory_id, score) in fts_matches {
                    if let Some(memory) = self.store.load(&memory_id).await? {
                        if !memory.forgotten {
                            fts_results.push(ScoredMemory {
                                memory,
                                score: score as f64,
                            });
                        }
                    }
                }
            }
            Err(error) => {
                tracing::debug!(%error, "FTS search unavailable, falling back to vector + graph");
            }
        }

        // 2. Vector similarity search via LanceDB
        let query_embedding = self.embedding_model.embed_one(query).await?;
        match self
            .embedding_table
            .vector_search(&query_embedding, config.max_results_per_source)
            .await
        {
            Ok(vector_matches) => {
                for (memory_id, distance) in vector_matches {
                    let similarity = 1.0 - distance;
                    if let Some(memory) = self.store.load(&memory_id).await? {
                        if !memory.forgotten {
                            vector_results.push(ScoredMemory {
                                memory,
                                score: similarity as f64,
                            });
                        }
                    }
                }
            }
            Err(error) => {
                tracing::debug!(%error, "vector search unavailable, falling back to graph only");
            }
        }

        // 3. Graph traversal from high-importance memories
        // Get identity and high-importance memories as starting points
        let seed_memories = self.store.get_high_importance(0.8, 20).await?;

        for seed in seed_memories {
            // Check if seed is semantically related to query via simple keyword matching
            if query
                .to_lowercase()
                .split_whitespace()
                .any(|term| seed.content.to_lowercase().contains(term))
            {
                graph_results.push(ScoredMemory {
                    memory: seed.clone(),
                    score: seed.importance as f64,
                });

                // Traverse graph to find related memories
                self.traverse_graph(&seed.id, config.max_graph_depth, &mut graph_results)
                    .await?;
            }
        }

        // 4. Merge results using Reciprocal Rank Fusion (RRF)
        let fused_results =
            reciprocal_rank_fusion(&vector_results, &fts_results, &graph_results, config.rrf_k);

        // Convert to MemorySearchResult with ranks, applying optional type filter
        let results: Vec<MemorySearchResult> = fused_results
            .into_iter()
            .filter(|scored| {
                config
                    .memory_type
                    .is_none_or(|t| scored.memory.memory_type == t)
            })
            .enumerate()
            .map(|(rank, scored)| MemorySearchResult {
                memory: scored.memory,
                score: scored.score as f32,
                rank: rank + 1,
            })
            .filter(|r| r.score >= config.min_score)
            .take(config.max_results_per_source)
            .collect();

        Ok(results)
    }

    /// Traverse the memory graph to find related memories (iterative to avoid async recursion).
    async fn traverse_graph(
        &self,
        start_id: &str,
        max_depth: usize,
        results: &mut Vec<ScoredMemory>,
    ) -> Result<()> {
        use std::collections::VecDeque;

        // Queue of (memory_id, current_depth)
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        queue.push_back((start_id.to_string(), 0));
        visited.insert(start_id.to_string());

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }

            let associations = self.store.get_associations(&current_id).await?;

            for assoc in associations {
                // Get the related memory
                let related_id = if assoc.source_id == current_id {
                    &assoc.target_id
                } else {
                    &assoc.source_id
                };

                if visited.contains(related_id) {
                    continue;
                }
                visited.insert(related_id.clone());

                if let Some(memory) = self.store.load(related_id).await? {
                    if memory.forgotten {
                        continue;
                    }
                    // Score based on relation type and weight
                    let type_multiplier = match assoc.relation_type {
                        RelationType::Updates => 1.5,
                        RelationType::CausedBy | RelationType::ResultOf => 1.3,
                        RelationType::RelatedTo => 1.0,
                        RelationType::Contradicts => 0.5,
                        RelationType::PartOf => 0.8,
                    };

                    let score = memory.importance as f64 * assoc.weight as f64 * type_multiplier;

                    results.push(ScoredMemory {
                        memory: memory.clone(),
                        score,
                    });

                    // Add to queue for RelatedTo and PartOf relations
                    if matches!(
                        assoc.relation_type,
                        RelationType::RelatedTo | RelationType::PartOf
                    ) {
                        queue.push_back((related_id.clone(), depth + 1));
                    }
                }
            }
        }

        Ok(())
    }
}

/// Search configuration for all modes.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Which search strategy to use.
    pub mode: SearchMode,
    /// Optional memory type filter. Required for `Typed` mode, optional for others.
    pub memory_type: Option<MemoryType>,
    /// Sort order for non-hybrid modes.
    pub sort_by: SearchSort,
    /// Maximum number of results to return.
    pub max_results: usize,
    /// Maximum number of results from each source (vector, fts, graph) in hybrid mode.
    pub max_results_per_source: usize,
    /// RRF k parameter (typically 60). Only used in hybrid mode.
    pub rrf_k: f64,
    /// Minimum score threshold for results. Only used in hybrid mode.
    pub min_score: f32,
    /// Maximum graph traversal depth. Only used in hybrid mode.
    pub max_graph_depth: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::Hybrid,
            memory_type: None,
            sort_by: SearchSort::Recent,
            max_results: 10,
            max_results_per_source: 50,
            rrf_k: 60.0,
            // RRF scores are 1/(k+rank), so with k=60 the max single-source
            // score is ~0.016. Set threshold low enough to not discard everything.
            min_score: 0.0,
            max_graph_depth: 2,
        }
    }
}

/// Simple scored memory for internal use.
#[derive(Debug, Clone)]
struct ScoredMemory {
    memory: Memory,
    score: f64,
}

/// Reciprocal Rank Fusion to combine results from multiple sources.
/// RRF score = sum(1 / (k + rank)) for each list where the item appears.
fn reciprocal_rank_fusion(
    vector_results: &[ScoredMemory],
    fts_results: &[ScoredMemory],
    graph_results: &[ScoredMemory],
    k: f64,
) -> Vec<ScoredMemory> {
    // Build a map of memory ID to RRF score
    let mut rrf_scores: HashMap<String, (f64, Memory)> = HashMap::new();

    // Add vector results
    for (rank, scored) in vector_results.iter().enumerate() {
        let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
        let entry = rrf_scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, scored.memory.clone()));
        entry.0 += rrf_score;
    }

    // Add FTS results
    for (rank, scored) in fts_results.iter().enumerate() {
        let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
        let entry = rrf_scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, scored.memory.clone()));
        entry.0 += rrf_score;
    }

    // Add graph results
    for (rank, scored) in graph_results.iter().enumerate() {
        let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
        let entry = rrf_scores
            .entry(scored.memory.id.clone())
            .or_insert((0.0, scored.memory.clone()));
        entry.0 += rrf_score;
    }

    // Convert to vec and sort by RRF score
    let mut fused: Vec<ScoredMemory> = rrf_scores
        .into_iter()
        .map(|(_, (score, memory))| ScoredMemory { memory, score })
        .collect();

    fused.sort_by(|a, b| b.score.total_cmp(&a.score));

    fused
}

/// Curate search results to return only the most relevant.
pub fn curate_results(
    results: &[MemorySearchResult],
    max_results: usize,
) -> Vec<&MemorySearchResult> {
    results.iter().take(max_results).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::types::MemoryType;
    use chrono::{Duration, Utc};

    fn make_scored(id: &str, score: f64) -> ScoredMemory {
        let mut memory = Memory::new(format!("content for {id}"), MemoryType::Fact);
        memory.id = id.to_string();
        ScoredMemory { memory, score }
    }

    #[test]
    fn test_rrf_single_list() {
        let vector = vec![make_scored("a", 0.9), make_scored("b", 0.7)];
        let fused = reciprocal_rank_fusion(&vector, &[], &[], 60.0);

        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].memory.id, "a");
        assert_eq!(fused[1].memory.id, "b");
        // First item: 1/(60+1) ≈ 0.01639
        assert!((fused[0].score - 1.0 / 61.0).abs() < 1e-10);
    }

    #[test]
    fn test_rrf_deduplication() {
        // Same memory appears in vector and FTS — scores should sum
        let vector = vec![make_scored("a", 0.9)];
        let fts = vec![make_scored("a", 5.0)];

        let fused = reciprocal_rank_fusion(&vector, &fts, &[], 60.0);
        assert_eq!(fused.len(), 1);
        // Should be 2 * 1/(60+1)
        let expected = 2.0 / 61.0;
        assert!((fused[0].score - expected).abs() < 1e-10);
    }

    #[test]
    fn test_rrf_multi_list_ranking() {
        // "a" appears in all three lists, "b" only in vector
        let vector = vec![make_scored("a", 0.9), make_scored("b", 0.5)];
        let fts = vec![make_scored("a", 5.0)];
        let graph = vec![make_scored("a", 0.8)];

        let fused = reciprocal_rank_fusion(&vector, &fts, &graph, 60.0);
        assert_eq!(fused[0].memory.id, "a");
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn test_rrf_empty_lists() {
        let fused = reciprocal_rank_fusion(&[], &[], &[], 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_curate_results_respects_limit() {
        let results: Vec<MemorySearchResult> = (0..10)
            .map(|i| MemorySearchResult {
                memory: Memory::new(format!("mem {i}"), MemoryType::Fact),
                score: 1.0 - (i as f32 * 0.1),
                rank: i + 1,
            })
            .collect();

        let curated = curate_results(&results, 3);
        assert_eq!(curated.len(), 3);
        assert_eq!(curated[0].rank, 1);
    }

    #[test]
    fn test_curate_results_handles_empty() {
        let curated = curate_results(&[], 5);
        assert!(curated.is_empty());
    }

    // Non-hybrid modes only need SQLite, no LanceDB/embeddings.
    // We construct a MemorySearch with dummy LanceDB/embedding fields
    // and only exercise code paths that don't touch them.

    async fn setup_search_with_memories() -> (Arc<crate::memory::MemoryStore>, Vec<Memory>) {
        let store = crate::memory::MemoryStore::connect_in_memory().await;
        let now = Utc::now();
        let mut memories = Vec::new();

        let types_and_importance = [
            (
                "user identity info",
                MemoryType::Identity,
                1.0,
                now - Duration::days(30),
            ),
            (
                "recent event",
                MemoryType::Event,
                0.4,
                now - Duration::hours(1),
            ),
            (
                "important decision",
                MemoryType::Decision,
                0.9,
                now - Duration::days(2),
            ),
            (
                "casual observation",
                MemoryType::Observation,
                0.2,
                now - Duration::days(7),
            ),
            (
                "user preference",
                MemoryType::Preference,
                0.7,
                now - Duration::days(1),
            ),
        ];

        for (content, memory_type, importance, created_at) in types_and_importance {
            let mut memory = Memory::new(content, memory_type).with_importance(importance);
            memory.created_at = created_at;
            memory.updated_at = created_at;
            memory.last_accessed_at = created_at;
            store.save(&memory).await.unwrap();
            memories.push(memory);
        }

        (store, memories)
    }

    #[tokio::test]
    async fn test_metadata_search_recent() {
        let (store, _memories) = setup_search_with_memories().await;

        // Construct MemorySearch with dummy lance/embedding (we won't use them)
        let lance_dir = tempfile::tempdir().unwrap();
        let lance_conn = lancedb::connect(lance_dir.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let embedding_table = EmbeddingTable::open_or_create(&lance_conn).await.unwrap();
        let embedding_model = Arc::new(EmbeddingModel::new(lance_dir.path()).unwrap());
        let search = MemorySearch::new(store, embedding_table, embedding_model);

        let config = SearchConfig {
            mode: SearchMode::Recent,
            max_results: 3,
            ..Default::default()
        };

        let results = search.search("", &config).await.unwrap();
        assert_eq!(results.len(), 3);
        // Most recent should be first (the event from 1 hour ago)
        assert_eq!(results[0].memory.content, "recent event");
        // Scores should be descending
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
    }

    #[tokio::test]
    async fn test_metadata_search_important() {
        let (store, _memories) = setup_search_with_memories().await;

        let lance_dir = tempfile::tempdir().unwrap();
        let lance_conn = lancedb::connect(lance_dir.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let embedding_table = EmbeddingTable::open_or_create(&lance_conn).await.unwrap();
        let embedding_model = Arc::new(EmbeddingModel::new(lance_dir.path()).unwrap());
        let search = MemorySearch::new(store, embedding_table, embedding_model);

        let config = SearchConfig {
            mode: SearchMode::Important,
            max_results: 5,
            ..Default::default()
        };

        let results = search.search("", &config).await.unwrap();
        // Identity (1.0) should be first, then Decision (0.9)
        assert_eq!(results[0].memory.memory_type, MemoryType::Identity);
        assert_eq!(results[1].memory.memory_type, MemoryType::Decision);
    }

    #[tokio::test]
    async fn test_metadata_search_typed() {
        let (store, _memories) = setup_search_with_memories().await;

        let lance_dir = tempfile::tempdir().unwrap();
        let lance_conn = lancedb::connect(lance_dir.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let embedding_table = EmbeddingTable::open_or_create(&lance_conn).await.unwrap();
        let embedding_model = Arc::new(EmbeddingModel::new(lance_dir.path()).unwrap());
        let search = MemorySearch::new(store, embedding_table, embedding_model);

        let config = SearchConfig {
            mode: SearchMode::Typed,
            memory_type: Some(MemoryType::Decision),
            max_results: 10,
            ..Default::default()
        };

        let results = search.search("", &config).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.memory_type, MemoryType::Decision);
    }

    #[tokio::test]
    async fn test_metadata_search_typed_empty() {
        let (store, _memories) = setup_search_with_memories().await;

        let lance_dir = tempfile::tempdir().unwrap();
        let lance_conn = lancedb::connect(lance_dir.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let embedding_table = EmbeddingTable::open_or_create(&lance_conn).await.unwrap();
        let embedding_model = Arc::new(EmbeddingModel::new(lance_dir.path()).unwrap());
        let search = MemorySearch::new(store, embedding_table, embedding_model);

        let config = SearchConfig {
            mode: SearchMode::Typed,
            memory_type: Some(MemoryType::Goal),
            max_results: 10,
            ..Default::default()
        };

        let results = search.search("", &config).await.unwrap();
        assert!(results.is_empty());
    }
}
