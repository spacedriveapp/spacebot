//! Chip evolution: effectiveness tracking, deprecation, trigger refinement,
//! and provisional chip genesis from unmatched insights.
//!
//! [`ChipEvolution`] operates on the `chip_state`, `chip_insights`, and
//! `chip_observations` tables. It drives three lifecycle transitions:
//!
//! - **Deprecation** — chips whose confidence falls below a caller-supplied
//!   threshold after enough observations are marked `deprecated`; their
//!   low-scoring insights are demoted to `discard`.
//! - **Provisional genesis** — when 5+ insights arrive that no existing chip
//!   matched, keyword clustering suggests a new chip with confidence 0.3.
//! - **Promotion validation** — a provisional chip earns full status once it
//!   accumulates sufficient insights and crosses a minimum confidence floor.

use crate::learning::LearningStore;

use anyhow::{Context as _, Result};
use sqlx::Row as _;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

// ── Constants ────────────────────────────────────────────────────────────────

/// Insights on a deprecated chip with `total_score` below this value are
/// demoted to the `discard` promotion tier.
const INSIGHT_DEMOTION_THRESHOLD: f64 = 0.5;

/// Confidence level assigned to every provisionally generated chip.
const PROVISIONAL_CHIP_CONFIDENCE: f64 = 0.3;

/// Minimum number of unmatched insights before provisional genesis is
/// attempted.
const PROVISIONAL_GENESIS_THRESHOLD: usize = 5;

/// How many top keywords become the trigger list for a provisional chip.
const PROVISIONAL_KEYWORD_COUNT: usize = 3;

// ── Stop words ───────────────────────────────────────────────────────────────

/// Common English words that carry no domain signal and are excluded from
/// keyword extraction.
static STOP_WORDS: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    [
        "a", "an", "the", "and", "or", "nor", "but", "so", "yet", "for", "of",
        "in", "on", "at", "by", "to", "up", "as", "is", "it", "its", "be",
        "been", "being", "am", "are", "was", "were", "do", "does", "did",
        "have", "has", "had", "will", "would", "could", "should", "may",
        "might", "shall", "can", "not", "no", "if", "then", "than", "that",
        "this", "these", "those", "they", "them", "their", "there", "here",
        "when", "where", "how", "what", "which", "who", "whom", "whose",
        "why", "with", "from", "into", "through", "during", "before",
        "after", "above", "below", "between", "each", "few", "more", "most",
        "some", "such", "any", "all", "both", "either", "neither", "other",
        "because", "since", "while", "also", "just", "only", "very", "too",
        "i", "we", "you", "he", "she", "me", "us", "him", "her", "my",
        "your", "his", "our", "about", "over", "under", "out", "off", "own",
    ]
    .into_iter()
    .collect()
});

// ── Public types ─────────────────────────────────────────────────────────────

/// A suggested chip produced by keyword clustering of unmatched insights.
///
/// Provisional chips start at [`PROVISIONAL_CHIP_CONFIDENCE`] (0.3) and must
/// accumulate real observations before they can be promoted to `active`.
#[derive(Debug, Clone)]
pub struct ProvisionalChip {
    pub suggested_id: String,
    pub suggested_name: String,
    pub keywords: Vec<String>,
    pub confidence: f64,
    pub insight_count: usize,
}

/// Per-chip effectiveness snapshot joining `chip_state` with `chip_insights`
/// counts.
#[derive(Debug, Clone)]
pub struct ChipEffectiveness {
    pub chip_id: String,
    pub observation_count: i64,
    pub success_rate: f64,
    pub confidence: f64,
    pub status: String,
    pub insight_count: i64,
}

// ── ChipEvolution ─────────────────────────────────────────────────────────────

/// Drives chip lifecycle transitions and surfaces provisional chip candidates.
#[derive(Debug)]
pub struct ChipEvolution {
    store: Arc<LearningStore>,
}

impl ChipEvolution {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Return the IDs of active chips whose confidence has fallen below
    /// `deprecation_threshold` after at least `min_observations` have been
    /// recorded.
    ///
    /// Callers should pass the returned IDs to [`deprecate_chip`] to complete
    /// the transition.
    pub async fn check_for_deprecation(
        &self,
        deprecation_threshold: f64,
        min_observations: i64,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT chip_id
            FROM chip_state
            WHERE status = 'active'
              AND confidence < ?
              AND observation_count >= ?
            ORDER BY confidence ASC
            "#,
        )
        .bind(deprecation_threshold)
        .bind(min_observations)
        .fetch_all(self.store.pool())
        .await
        .context("failed to query chips for deprecation")?;

        let chip_ids = rows
            .iter()
            .map(|row| row.try_get::<String, _>("chip_id"))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to decode chip_id from deprecation query")?;

        Ok(chip_ids)
    }

    /// Mark a chip as `deprecated` and demote its low-scoring insights to the
    /// `discard` tier.
    ///
    /// Only insights with `total_score < 0.5` are demoted — higher-scoring
    /// insights may still be useful for the merger pipeline.
    pub async fn deprecate_chip(&self, chip_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE chip_state
            SET status = 'deprecated', updated_at = datetime('now')
            WHERE chip_id = ?
            "#,
        )
        .bind(chip_id)
        .execute(self.store.pool())
        .await
        .with_context(|| format!("failed to deprecate chip {chip_id}"))?;

        sqlx::query(
            r#"
            UPDATE chip_insights
            SET promotion_tier = 'discard'
            WHERE chip_id = ? AND total_score < ?
            "#,
        )
        .bind(chip_id)
        .bind(INSIGHT_DEMOTION_THRESHOLD)
        .execute(self.store.pool())
        .await
        .with_context(|| format!("failed to demote insights for deprecated chip {chip_id}"))?;

        tracing::info!(chip_id, "chip deprecated and low-scoring insights demoted");

        Ok(())
    }

    /// Scan unmatched insights for keyword clusters that suggest a new chip.
    ///
    /// `unmatched_insights` is a slice of `(content, chips_tried)` pairs —
    /// the content is the insight text; the second element lists which chips
    /// were tested and didn't match (informational, unused here).
    ///
    /// Returns `None` when fewer than [`PROVISIONAL_GENESIS_THRESHOLD`]
    /// insights are provided, or when keyword extraction yields no candidates.
    pub async fn check_provisional_genesis(
        &self,
        unmatched_insights: &[(String, Vec<String>)],
    ) -> Option<ProvisionalChip> {
        if unmatched_insights.len() < PROVISIONAL_GENESIS_THRESHOLD {
            return None;
        }

        let contents: Vec<String> = unmatched_insights
            .iter()
            .map(|(content, _)| content.clone())
            .collect();

        let keyword_frequencies = extract_keywords(&contents);

        if keyword_frequencies.is_empty() {
            return None;
        }

        let top_keywords: Vec<String> = keyword_frequencies
            .into_iter()
            .take(PROVISIONAL_KEYWORD_COUNT)
            .map(|(keyword, _)| keyword)
            .collect();

        if top_keywords.is_empty() {
            return None;
        }

        // Derive a stable suggested ID from the top keywords so callers can
        // detect duplicates by checking against existing chip IDs.
        let keyword_slug = top_keywords.join("_");
        let suggested_id = format!("provisional_{keyword_slug}");
        let suggested_name = top_keywords
            .iter()
            .map(|keyword| {
                let mut characters = keyword.chars();
                match characters.next() {
                    None => String::new(),
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + characters.as_str()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        Some(ProvisionalChip {
            suggested_id,
            suggested_name,
            keywords: top_keywords,
            confidence: PROVISIONAL_CHIP_CONFIDENCE,
            insight_count: unmatched_insights.len(),
        })
    }

    /// Return `true` if the provisional chip identified by `chip_id` has
    /// accumulated at least `min_insights` matched insights and a confidence
    /// score at or above `min_confidence`.
    ///
    /// Returns `false` — not an error — when the chip is not found or has not
    /// yet reached either threshold.
    pub async fn validate_provisional(
        &self,
        chip_id: &str,
        min_insights: i64,
        min_confidence: f64,
    ) -> Result<bool> {
        // Read current confidence from chip_state; only provisional chips
        // are candidates for promotion here.
        let state_row = sqlx::query(
            r#"
            SELECT confidence
            FROM chip_state
            WHERE chip_id = ? AND status = 'provisional'
            "#,
        )
        .bind(chip_id)
        .fetch_optional(self.store.pool())
        .await
        .with_context(|| format!("failed to query chip_state for {chip_id}"))?;

        let Some(row) = state_row else {
            return Ok(false);
        };

        let confidence: f64 = row
            .try_get("confidence")
            .context("failed to decode confidence from chip_state")?;

        if confidence < min_confidence {
            return Ok(false);
        }

        // Count insights accumulated for this chip.
        let count_row = sqlx::query(
            r#"
            SELECT COUNT(*) as insight_count
            FROM chip_insights
            WHERE chip_id = ?
            "#,
        )
        .bind(chip_id)
        .fetch_one(self.store.pool())
        .await
        .with_context(|| format!("failed to count insights for {chip_id}"))?;

        let insight_count: i64 = count_row
            .try_get("insight_count")
            .context("failed to decode insight_count")?;

        Ok(insight_count >= min_insights)
    }

    /// Overwrite the stored confidence score for a chip and stamp `updated_at`.
    pub async fn update_chip_confidence(
        &self,
        chip_id: &str,
        new_confidence: f64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE chip_state
            SET confidence = ?, updated_at = datetime('now')
            WHERE chip_id = ?
            "#,
        )
        .bind(new_confidence)
        .bind(chip_id)
        .execute(self.store.pool())
        .await
        .with_context(|| format!("failed to update confidence for chip {chip_id}"))?;

        Ok(())
    }

    /// Return an effectiveness summary for every chip in `chip_state`, joined
    /// with the number of insights accumulated in `chip_insights`.
    ///
    /// Results are ordered by confidence descending so the healthiest chips
    /// appear first.
    pub async fn get_chip_effectiveness_summary(&self) -> Result<Vec<ChipEffectiveness>> {
        let rows = sqlx::query(
            r#"
            SELECT
                cs.chip_id,
                cs.observation_count,
                cs.success_rate,
                cs.confidence,
                cs.status,
                COUNT(ci.id) AS insight_count
            FROM chip_state cs
            LEFT JOIN chip_insights ci ON ci.chip_id = cs.chip_id
            GROUP BY cs.chip_id, cs.observation_count, cs.success_rate,
                     cs.confidence, cs.status
            ORDER BY cs.confidence DESC
            "#,
        )
        .fetch_all(self.store.pool())
        .await
        .context("failed to fetch chip effectiveness summary")?;

        let summaries = rows
            .iter()
            .map(|row| {
                Ok(ChipEffectiveness {
                    chip_id: row.try_get("chip_id")?,
                    observation_count: row.try_get("observation_count")?,
                    success_rate: row.try_get("success_rate")?,
                    confidence: row.try_get("confidence")?,
                    status: row.try_get("status")?,
                    insight_count: row.try_get("insight_count")?,
                })
            })
            .collect::<std::result::Result<Vec<_>, sqlx::Error>>()
            .context("failed to decode chip effectiveness row")?;

        Ok(summaries)
    }

    /// Re-evaluate insights for a deprecated chip and demote any that fall
    /// below the demotion threshold.
    ///
    /// This is a targeted cleanup pass — useful when a chip is deprecated
    /// mid-run and some insights were written after the confidence score
    /// degraded but before the status was updated.
    pub async fn cleanup_deprecated(&self, chip_id: &str) -> Result<()> {
        let demoted_count = sqlx::query(
            r#"
            UPDATE chip_insights
            SET promotion_tier = 'discard'
            WHERE chip_id = ?
              AND total_score < ?
              AND (promotion_tier IS NULL OR promotion_tier != 'discard')
            "#,
        )
        .bind(chip_id)
        .bind(INSIGHT_DEMOTION_THRESHOLD)
        .execute(self.store.pool())
        .await
        .with_context(|| format!("failed to cleanup insights for deprecated chip {chip_id}"))?
        .rows_affected();

        if demoted_count > 0 {
            tracing::debug!(
                chip_id,
                demoted_count,
                "demoted below-threshold insights during deprecated chip cleanup"
            );
        }

        Ok(())
    }
}

// ── Keyword extraction ────────────────────────────────────────────────────────

/// Extract content words from a slice of texts, skipping stop words and
/// single-character tokens.
///
/// Returns keywords sorted by frequency descending. Words are lowercased and
/// stripped of non-alphabetic characters before counting, so `"rust."` and
/// `"rust"` contribute to the same bucket.
fn extract_keywords(texts: &[String]) -> Vec<(String, usize)> {
    let mut frequency_map: HashMap<String, usize> = HashMap::new();

    for text in texts {
        for raw_token in text.split_whitespace() {
            // Strip leading/trailing punctuation but keep interior hyphens
            // so "compile-time" remains meaningful.
            let token: String = raw_token
                .chars()
                .filter(|character| character.is_alphabetic() || *character == '-')
                .collect::<String>()
                .to_lowercase();

            // Skip empty tokens, single characters, and stop words.
            if token.len() <= 1 {
                continue;
            }
            if STOP_WORDS.contains(token.as_str()) {
                continue;
            }

            *frequency_map.entry(token).or_insert(0) += 1;
        }
    }

    let mut keyword_frequencies: Vec<(String, usize)> = frequency_map.into_iter().collect();

    // Primary sort: frequency descending. Secondary sort: alphabetical for
    // deterministic output when frequencies are equal.
    keyword_frequencies.sort_by(|(keyword_a, count_a), (keyword_b, count_b)| {
        count_b.cmp(count_a).then_with(|| keyword_a.cmp(keyword_b))
    });

    keyword_frequencies
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords_basic_frequency() {
        let texts = vec![
            "rust compilation error in async code".to_string(),
            "async rust code fails to compile".to_string(),
            "rust borrow checker error in async trait".to_string(),
        ];

        let keywords = extract_keywords(&texts);

        // "rust" and "async" appear 3 times each; "error" and "code" twice.
        let top_words: Vec<&str> = keywords
            .iter()
            .take(4)
            .map(|(word, _)| word.as_str())
            .collect();

        assert!(
            top_words.contains(&"rust"),
            "expected 'rust' in top keywords, got: {top_words:?}"
        );
        assert!(
            top_words.contains(&"async"),
            "expected 'async' in top keywords, got: {top_words:?}"
        );
    }

    #[test]
    fn test_extract_keywords_filters_stop_words() {
        let texts = vec![
            "the quick brown fox jumps over the lazy dog".to_string(),
            "the fox is very quick and the dog is lazy".to_string(),
        ];

        let keywords = extract_keywords(&texts);
        let words: Vec<&str> = keywords.iter().map(|(word, _)| word.as_str()).collect();

        // Stop words must not appear in the output.
        for stop_word in &["the", "is", "and", "over", "very"] {
            assert!(
                !words.contains(stop_word),
                "stop word '{stop_word}' should be excluded, got: {words:?}"
            );
        }

        assert!(words.contains(&"fox"), "expected 'fox' in keywords");
    }

    #[test]
    fn test_extract_keywords_punctuation_stripped() {
        let texts = vec![
            "sqlx. sqlx! sqlx? is the database library.".to_string(),
            "use sqlx for database queries".to_string(),
        ];

        let keywords = extract_keywords(&texts);
        let words: Vec<&str> = keywords.iter().map(|(word, _)| word.as_str()).collect();

        assert!(
            words.contains(&"sqlx"),
            "expected 'sqlx' after punctuation stripping, got: {words:?}"
        );

        // All four occurrences should fold into one "sqlx" entry.
        let sqlx_count = keywords
            .iter()
            .find(|(word, _)| word == "sqlx")
            .map(|(_, count)| *count)
            .unwrap_or(0);

        assert_eq!(sqlx_count, 4, "expected 4 occurrences of 'sqlx'");
    }

    #[test]
    fn test_extract_keywords_empty_input() {
        let keywords = extract_keywords(&[]);
        assert!(keywords.is_empty(), "expected empty result for empty input");
    }

    #[test]
    fn test_extract_keywords_sorted_descending() {
        let texts = vec![
            "database database database query query table".to_string(),
        ];

        let keywords = extract_keywords(&texts);

        // Frequencies must be non-increasing.
        for window in keywords.windows(2) {
            let (_, count_first) = &window[0];
            let (_, count_second) = &window[1];
            assert!(
                count_first >= count_second,
                "keywords not sorted descending: {keywords:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_provisional_genesis_below_threshold() {
        // Build a minimal fake store path — we only test the non-DB path here.
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("learning.db");
        let store = LearningStore::connect(&db_path).await.unwrap();
        let evolution = ChipEvolution::new(store);

        // Fewer than PROVISIONAL_GENESIS_THRESHOLD (5) insights — should be None.
        let unmatched: Vec<(String, Vec<String>)> = vec![
            ("memory leak in async spawned task".to_string(), vec![]),
            ("tokio task memory grows unbounded".to_string(), vec![]),
        ];

        let result = evolution.check_provisional_genesis(&unmatched).await;
        assert!(result.is_none(), "expected None below threshold");
    }

    #[tokio::test]
    async fn test_provisional_genesis_returns_chip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("learning.db");
        let store = LearningStore::connect(&db_path).await.unwrap();
        let evolution = ChipEvolution::new(store);

        // Five insights all mentioning "memory" and "tokio" prominently.
        let unmatched: Vec<(String, Vec<String>)> = vec![
            ("memory leak in tokio async task spawning".to_string(), vec![]),
            ("tokio task memory grows unbounded after spawn".to_string(), vec![]),
            ("memory pressure from unbounded tokio tasks".to_string(), vec![]),
            ("monitor memory usage in long-running tokio workers".to_string(), vec![]),
            ("tokio memory consumption spikes during burst load".to_string(), vec![]),
        ];

        let result = evolution.check_provisional_genesis(&unmatched).await;
        assert!(result.is_some(), "expected a ProvisionalChip for 5+ insights");

        let chip = result.unwrap();
        assert_eq!(chip.confidence, PROVISIONAL_CHIP_CONFIDENCE);
        assert_eq!(chip.insight_count, 5);
        assert!(!chip.keywords.is_empty(), "provisional chip must have keywords");
        assert!(
            chip.suggested_id.starts_with("provisional_"),
            "suggested_id must be prefixed with 'provisional_'"
        );

        // "tokio" and "memory" should surface as top keywords.
        let has_prominent_keyword = chip.keywords.contains(&"tokio".to_string())
            || chip.keywords.contains(&"memory".to_string());
        assert!(
            has_prominent_keyword,
            "expected 'tokio' or 'memory' among top keywords, got: {:?}",
            chip.keywords
        );
    }

    #[tokio::test]
    async fn test_provisional_genesis_keyword_count_capped() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("learning.db");
        let store = LearningStore::connect(&db_path).await.unwrap();
        let evolution = ChipEvolution::new(store);

        // Enough variety to produce many keywords.
        let unmatched: Vec<(String, Vec<String>)> = (0..PROVISIONAL_GENESIS_THRESHOLD)
            .map(|index| {
                (
                    format!("error alpha beta gamma delta {index} rust tokio sqlx"),
                    vec![],
                )
            })
            .collect();

        let result = evolution.check_provisional_genesis(&unmatched).await;
        assert!(result.is_some());

        let chip = result.unwrap();
        assert!(
            chip.keywords.len() <= PROVISIONAL_KEYWORD_COUNT,
            "keyword count must be at most {PROVISIONAL_KEYWORD_COUNT}, got {}",
            chip.keywords.len()
        );
    }
}
