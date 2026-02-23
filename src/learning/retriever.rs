//! Structural retrieval of distillations by type priority.
//!
//! Retrieves the most relevant distillations for a given intent using a
//! priority-ordered type hierarchy, trigger matching, domain filtering, and
//! keyword overlap scoring. An in-memory cache keyed by distillation type
//! avoids repeated database scans on every retrieval call.

use super::distillation::Distillation;
use super::store::LearningStore;

use sqlx::Row as _;

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Stop words
// ---------------------------------------------------------------------------

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "have", "has",
    "had", "do", "does", "did", "will", "would", "could", "it", "its", "of",
    "in", "to", "for", "on", "at", "by", "with", "from", "this", "that", "and",
    "or", "but",
];

/// Priority ordering for distillation types. Higher-priority types fill the
/// result set first; lower-priority types contribute only remaining slots.
const TYPE_PRIORITY_ORDER: &[&str] =
    &["policy", "playbook", "sharp_edge", "heuristic", "anti_pattern"];

// ---------------------------------------------------------------------------
// DistillationCache
// ---------------------------------------------------------------------------

/// In-memory cache of distillations grouped by `distillation_type`.
///
/// The cache is refreshed on a configurable interval. A `RwLock` protects the
/// inner maps so multiple concurrent readers (one per channel turn) can proceed
/// without blocking.
pub(crate) struct DistillationCache {
    by_type: std::sync::RwLock<HashMap<String, Vec<Distillation>>>,
    last_refresh: std::sync::RwLock<Option<std::time::Instant>>,
}

impl DistillationCache {
    pub fn new() -> Self {
        Self {
            by_type: std::sync::RwLock::new(HashMap::new()),
            last_refresh: std::sync::RwLock::new(None),
        }
    }

    /// Load all distillations from the database and rebuild the type map.
    pub async fn refresh(&self, store: &LearningStore) -> anyhow::Result<()> {
        let rows = sqlx::query(
            "SELECT id, distillation_type, statement, confidence, \
             triggers, anti_triggers, domains, times_retrieved, times_used, \
             times_helped, validation_count, contradiction_count, source_episode_id \
             FROM distillations ORDER BY confidence DESC",
        )
        .fetch_all(store.pool())
        .await?;

        let mut grouped: HashMap<String, Vec<Distillation>> = HashMap::new();

        for row in rows {
            let distillation_type: String = row.try_get("distillation_type")?;
            let triggers_json: String = row.try_get("triggers")?;
            let anti_triggers_json: String = row.try_get("anti_triggers")?;
            let domains_json: String = row.try_get("domains")?;

            let triggers: Vec<String> =
                serde_json::from_str(&triggers_json).unwrap_or_default();
            let anti_triggers: Vec<String> =
                serde_json::from_str(&anti_triggers_json).unwrap_or_default();
            let domains: Vec<String> =
                serde_json::from_str(&domains_json).unwrap_or_default();

            let parsed_type: super::distillation::DistillationType =
                distillation_type.parse().unwrap_or(super::distillation::DistillationType::Heuristic);
            let distillation = Distillation {
                id: row.try_get("id")?,
                distillation_type: parsed_type,
                statement: row.try_get("statement")?,
                confidence: row.try_get("confidence")?,
                triggers,
                anti_triggers,
                domains,
                times_retrieved: row.try_get("times_retrieved")?,
                times_used: row.try_get("times_used")?,
                times_helped: row.try_get("times_helped")?,
                validation_count: row.try_get("validation_count")?,
                contradiction_count: row.try_get("contradiction_count")?,
                source_episode_id: row.try_get("source_episode_id")?,
            };

            grouped
                .entry(distillation_type)
                .or_default()
                .push(distillation);
        }

        *self.by_type.write().expect("by_type lock poisoned") = grouped;
        *self.last_refresh.write().expect("last_refresh lock poisoned") =
            Some(std::time::Instant::now());

        Ok(())
    }

    /// Returns true when the cache has never been populated or the refresh
    /// interval has elapsed.
    fn needs_refresh(&self, interval_secs: u64) -> bool {
        let last = self
            .last_refresh
            .read()
            .expect("last_refresh lock poisoned");
        match *last {
            None => true,
            Some(instant) => instant.elapsed().as_secs() >= interval_secs,
        }
    }
}

impl std::fmt::Debug for DistillationCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self
            .by_type
            .read()
            .map(|map| map.values().map(|v| v.len()).sum::<usize>())
            .unwrap_or(0);
        f.debug_struct("DistillationCache")
            .field("total_distillations", &count)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Main retrieval function
// ---------------------------------------------------------------------------

/// Retrieve the most relevant distillations for a given intent.
///
/// Distillation types are evaluated in priority order: policy, playbook,
/// sharp_edge, heuristic, anti_pattern. Within each type, candidates are
/// scored by trigger matches, domain matches, and keyword overlap then sorted
/// descending. Higher-priority types fill the result set first.
///
/// Each returned distillation has its `times_retrieved` counter incremented
/// in the database as a fire-and-forget background write.
pub async fn retrieve_relevant(
    store: &LearningStore,
    cache: &DistillationCache,
    intent: &str,
    tool: Option<&str>,
    domain: Option<&str>,
    max_results: usize,
    tick_interval_secs: u64,
) -> anyhow::Result<Vec<Distillation>> {
    if cache.needs_refresh(tick_interval_secs) {
        cache.refresh(store).await?;
    }

    let by_type = cache.by_type.read().expect("by_type lock poisoned");

    let mut collected: Vec<Distillation> = Vec::with_capacity(max_results);

    for distillation_type in TYPE_PRIORITY_ORDER {
        if collected.len() >= max_results {
            break;
        }

        let candidates = match by_type.get(*distillation_type) {
            Some(candidates) => candidates,
            None => continue,
        };

        // Score and filter candidates for this type.
        let mut scored: Vec<(f64, &Distillation)> = candidates
            .iter()
            .filter_map(|distillation| {
                if has_anti_trigger(distillation, intent) {
                    return None;
                }
                let trigger_score = matches_trigger(distillation, intent, tool);
                let domain_score = matches_domain(distillation, domain);
                let keyword_score = keyword_overlap(intent, &distillation.statement);
                let combined =
                    trigger_score * 3.0 + domain_score * 2.0 + keyword_score;
                Some((combined, distillation))
            })
            .collect();

        // Descending by combined score.
        scored.sort_by(|(score_a, _), (score_b, _)| {
            score_b
                .partial_cmp(score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let remaining_slots = max_results - collected.len();
        for (_, distillation) in scored.into_iter().take(remaining_slots) {
            collected.push(distillation.clone());
        }
    }

    // Increment times_retrieved for each result as a fire-and-forget write.
    for distillation in &collected {
        let pool = store.pool().clone();
        let id = distillation.id.clone();
        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE distillations SET times_retrieved = times_retrieved + 1 WHERE id = ?",
            )
            .bind(&id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, distillation_id = %id, "failed to increment times_retrieved");
            }
        });
    }

    Ok(collected)
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Lowercase, split on non-alphanumeric characters, and remove stop words.
fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty() && !STOP_WORDS.contains(token))
        .map(String::from)
        .collect()
}

/// Jaccard similarity between the token sets of two strings.
///
/// Returns 0.0 when both sets are empty to avoid division by zero.
fn keyword_overlap(text_a: &str, text_b: &str) -> f64 {
    let tokens_a = tokenize(text_a);
    let tokens_b = tokenize(text_b);

    let intersection_size = tokens_a.intersection(&tokens_b).count();
    let union_size = tokens_a.union(&tokens_b).count();

    if union_size == 0 {
        return 0.0;
    }

    intersection_size as f64 / union_size as f64
}

/// Count how many of the distillation's triggers appear as substrings in the
/// intent or (if provided) the tool name.
fn matches_trigger(
    distillation: &Distillation,
    intent: &str,
    tool: Option<&str>,
) -> f64 {
    let intent_lower = intent.to_lowercase();
    let tool_lower = tool.map(|name| name.to_lowercase());

    distillation
        .triggers
        .iter()
        .filter(|trigger| {
            let trigger_lower = trigger.to_lowercase();
            intent_lower.contains(trigger_lower.as_str())
                || tool_lower
                    .as_deref()
                    .map(|tool_name| tool_name.contains(trigger_lower.as_str()))
                    .unwrap_or(false)
        })
        .count() as f64
}

/// Count how many of the distillation's domains match the provided domain.
fn matches_domain(distillation: &Distillation, domain: Option<&str>) -> f64 {
    let Some(domain) = domain else {
        return 0.0;
    };
    let domain_lower = domain.to_lowercase();
    distillation
        .domains
        .iter()
        .filter(|distillation_domain| {
            distillation_domain.to_lowercase() == domain_lower
        })
        .count() as f64
}

/// Returns true when any anti-trigger substring is present in the intent.
fn has_anti_trigger(distillation: &Distillation, intent: &str) -> bool {
    let intent_lower = intent.to_lowercase();
    distillation
        .anti_triggers
        .iter()
        .any(|anti_trigger| intent_lower.contains(anti_trigger.to_lowercase().as_str()))
}
