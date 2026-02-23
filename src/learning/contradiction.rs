//! Contradiction detection between learning insights.
//!
//! Compares new insights against existing ones in the same category using word
//! overlap, opposition pair matching, and negation asymmetry detection.
//! Classifies contradictions into four types with conservative resolution.

use super::store::LearningStore;

use serde::{Deserialize, Serialize};

use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContradictionType {
    Temporal,
    Contextual,
    Direct,
    Uncertain,
}

impl std::fmt::Display for ContradictionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Temporal => write!(f, "TEMPORAL"),
            Self::Contextual => write!(f, "CONTEXTUAL"),
            Self::Direct => write!(f, "DIRECT"),
            Self::Uncertain => write!(f, "UNCERTAIN"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContradictionResult {
    pub insight_a_id: String,
    pub insight_b_id: String,
    pub contradiction_type: ContradictionType,
    pub resolution: String,
    pub similarity_score: f64,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const OPPOSITION_PAIRS: &[(&str, &str)] = &[
    ("prefer", "avoid"),
    ("like", "hate"),
    ("always", "never"),
    ("should", "shouldn't"),
    ("good", "bad"),
    ("increase", "decrease"),
    ("enable", "disable"),
    ("include", "exclude"),
    ("before", "after"),
    ("more", "less"),
    ("fast", "slow"),
    ("simple", "complex"),
];

const NEGATION_WORDS: &[&str] = &[
    "not",
    "don't",
    "doesn't",
    "never",
    "no",
    "won't",
    "can't",
    "shouldn't",
    "isn't",
    "aren't",
];

const TEMPORAL_MARKERS: &[&str] = &[
    "now",
    "currently",
    "recently",
    "changed",
    "updated",
    "switched",
    "moved to",
];

const CONTEXTUAL_MARKERS: &[&str] =
    &["when", "if", "during", "sometimes", "in case", "depending on"];

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "it", "its", "of", "in", "to", "for", "on",
    "at", "by", "with", "from", "this", "that", "and", "or", "but",
];

// ---------------------------------------------------------------------------
// Text analysis helpers
// ---------------------------------------------------------------------------

fn tokenize(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty() && !STOP_WORDS.contains(word))
        .map(String::from)
        .collect()
}

/// Jaccard similarity between tokenized representations.
pub fn word_overlap(text_a: &str, text_b: &str) -> f64 {
    let tokens_a = tokenize(text_a);
    let tokens_b = tokenize(text_b);
    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

fn has_opposition(text_a: &str, text_b: &str) -> bool {
    let lower_a = text_a.to_lowercase();
    let lower_b = text_b.to_lowercase();
    for (word_a, word_b) in OPPOSITION_PAIRS {
        let a_has_first = lower_a.contains(word_a);
        let a_has_second = lower_a.contains(word_b);
        let b_has_first = lower_b.contains(word_a);
        let b_has_second = lower_b.contains(word_b);

        // One text contains word_a and the other contains word_b (or vice versa).
        if (a_has_first && b_has_second) || (a_has_second && b_has_first) {
            return true;
        }
    }
    false
}

fn has_negation_asymmetry(text_a: &str, text_b: &str) -> bool {
    let lower_a = text_a.to_lowercase();
    let lower_b = text_b.to_lowercase();
    let a_negated = NEGATION_WORDS.iter().any(|word| lower_a.contains(word));
    let b_negated = NEGATION_WORDS.iter().any(|word| lower_b.contains(word));
    a_negated != b_negated
}

fn classify_type(text_a: &str, text_b: &str) -> ContradictionType {
    let lower_a = text_a.to_lowercase();
    let lower_b = text_b.to_lowercase();

    let has_temporal = TEMPORAL_MARKERS
        .iter()
        .any(|marker| lower_a.contains(marker) || lower_b.contains(marker));
    if has_temporal {
        return ContradictionType::Temporal;
    }

    let has_contextual = CONTEXTUAL_MARKERS
        .iter()
        .any(|marker| lower_a.contains(marker) || lower_b.contains(marker));
    if has_contextual {
        return ContradictionType::Contextual;
    }

    if has_opposition(text_a, text_b) || has_negation_asymmetry(text_a, text_b) {
        return ContradictionType::Direct;
    }

    ContradictionType::Uncertain
}

fn resolution_for(contradiction_type: &ContradictionType) -> &'static str {
    match contradiction_type {
        ContradictionType::Temporal => "update",
        ContradictionType::Contextual => "context",
        ContradictionType::Direct => "keep_both",
        ContradictionType::Uncertain => "keep_both",
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether two insight texts contradict each other.
///
/// Returns `Some` if word overlap >= 0.3 AND either opposition pairs or
/// negation asymmetry are detected. Conservative: requires topical similarity
/// before flagging opposition.
pub fn check_contradiction(
    insight_a_id: &str,
    text_a: &str,
    insight_b_id: &str,
    text_b: &str,
) -> Option<ContradictionResult> {
    let similarity = word_overlap(text_a, text_b);
    if similarity < 0.3 {
        return None;
    }

    if !has_opposition(text_a, text_b) && !has_negation_asymmetry(text_a, text_b) {
        return None;
    }

    let contradiction_type = classify_type(text_a, text_b);
    let resolution = resolution_for(&contradiction_type).to_owned();

    Some(ContradictionResult {
        insight_a_id: insight_a_id.to_owned(),
        insight_b_id: insight_b_id.to_owned(),
        contradiction_type,
        resolution,
        similarity_score: similarity,
    })
}

/// Find contradictions between a new insight and existing insights in the same
/// category.
pub async fn find_contradictions_for_insight(
    store: &LearningStore,
    new_insight_id: &str,
    new_content: &str,
    category: &str,
) -> anyhow::Result<Vec<ContradictionResult>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, content FROM insights WHERE category = ? AND id != ?",
    )
    .bind(category)
    .bind(new_insight_id)
    .fetch_all(store.pool())
    .await?;

    let mut results = Vec::new();
    for (existing_id, existing_content) in &rows {
        if let Some(result) =
            check_contradiction(new_insight_id, new_content, existing_id, existing_content)
        {
            results.push(result);
        }
    }
    Ok(results)
}

/// Persist a contradiction record.
pub async fn save_contradiction(
    store: &LearningStore,
    result: &ContradictionResult,
) -> anyhow::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO contradictions (id, insight_a_id, insight_b_id, contradiction_type, \
         resolution, similarity_score, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
    )
    .bind(&id)
    .bind(&result.insight_a_id)
    .bind(&result.insight_b_id)
    .bind(result.contradiction_type.to_string())
    .bind(&result.resolution)
    .bind(result.similarity_score)
    .execute(store.pool())
    .await?;
    Ok(())
}
