//! Meta-Ralph quality gate — a programmatic (not LLM) filter for learning insights.
//!
//! Evaluates candidate insights against a battery of heuristic checks covering
//! actionability, novelty, reasoning depth, specificity, outcome linkage, and ethics.
//! Produces a [`RalphVerdict`] and optional persistence to the learning store.

use std::collections::HashSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::store::LearningStore;

// ── Verdict ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RalphVerdict {
    Quality,
    NeedsWork,
    Primitive,
    Duplicate,
}

impl std::fmt::Display for RalphVerdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RalphVerdict::Quality => write!(formatter, "QUALITY"),
            RalphVerdict::NeedsWork => write!(formatter, "NEEDS_WORK"),
            RalphVerdict::Primitive => write!(formatter, "PRIMITIVE"),
            RalphVerdict::Duplicate => write!(formatter, "DUPLICATE"),
        }
    }
}

// ── Scores ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityScores {
    pub actionability: u8,
    pub novelty: u8,
    pub reasoning: u8,
    pub specificity: u8,
    pub outcome_linked: u8,
    pub ethics: u8,
}

impl QualityScores {
    pub fn total(&self) -> f64 {
        (self.actionability
            + self.novelty
            + self.reasoning
            + self.specificity
            + self.outcome_linked
            + self.ethics) as f64
    }

    fn zero() -> Self {
        Self {
            actionability: 0,
            novelty: 0,
            reasoning: 0,
            specificity: 0,
            outcome_linked: 0,
            ethics: 0,
        }
    }
}

// ── Result ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RalphResult {
    pub verdict: RalphVerdict,
    pub scores: QualityScores,
    pub total_score: f64,
    pub input_hash: String,
}

// ── Primitive filter ─────────────────────────────────────────────────────────

fn is_primitive(text: &str) -> bool {
    let lowercased = text.to_lowercase();

    // Too short to be meaningful.
    if text.len() < 20 {
        return true;
    }

    // Tool names appearing as words indicate operational log noise.
    let tool_names = ["shell", "exec", "memory_save", "memory_recall"];
    for tool in tool_names {
        // Word-boundary check: surrounded by non-alphanumeric or at string edges.
        if let Some(position) = lowercased.find(tool) {
            let before = position.checked_sub(1).map(|i| lowercased.as_bytes()[i]);
            let after = lowercased.as_bytes().get(position + tool.len());
            let left_boundary = before.map_or(true, |byte| !byte.is_ascii_alphanumeric());
            let right_boundary =
                after.map_or(true, |byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
            if left_boundary && right_boundary {
                return true;
            }
        }
    }

    // Operational keyword density indicates raw tool output, not insight.
    let operational_keywords = [
        "executed",
        "returned",
        "output",
        "invoked",
        "called",
        "ran",
    ];
    let operational_count = operational_keywords
        .iter()
        .filter(|keyword| lowercased.contains(*keyword))
        .count();
    if operational_count >= 2 {
        return true;
    }

    // Arrow patterns suggest trace/pipeline notation, not insight.
    if text.contains('→') || text.contains("->") {
        return true;
    }

    // Tautology prefixes add no information.
    let tautology_prefixes = [
        "always check",
        "be careful",
        "make sure",
        "remember to",
        "don't forget to",
        "it's important",
    ];
    for prefix in tautology_prefixes {
        if lowercased.starts_with(prefix) {
            return true;
        }
    }

    // Generic hedging language without substance.
    let generic_qualifiers = [
        "generally",
        "usually",
        "often",
        "typically",
        "sometimes",
        "probably",
    ];
    let qualifier_count = generic_qualifiers
        .iter()
        .filter(|qualifier| lowercased.contains(*qualifier))
        .count();
    if qualifier_count >= 2 {
        return true;
    }

    false
}

// ── Semantic hash ─────────────────────────────────────────────────────────────

pub fn semantic_hash(text: &str) -> String {
    let lowercased = text.to_lowercase();

    // Keep only alphanumeric characters and spaces.
    let alphanumeric_only: String = lowercased
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == ' ' {
                character
            } else {
                ' '
            }
        })
        .collect();

    // Replace digit sequences with a normalizing placeholder.
    let mut normalized = String::with_capacity(alphanumeric_only.len());
    let mut in_digit_run = false;
    for character in alphanumeric_only.chars() {
        if character.is_ascii_digit() {
            if !in_digit_run {
                normalized.push('N');
                in_digit_run = true;
            }
        } else {
            in_digit_run = false;
            normalized.push(character);
        }
    }

    // Collapse whitespace and trim.
    let collapsed: String = normalized
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");

    format!("{:x}", md5::compute(collapsed))
}

// ── Dimension scoring ─────────────────────────────────────────────────────────

fn score_dimensions(text: &str, existing_hashes: &HashSet<String>) -> QualityScores {
    let lowercased = text.to_lowercase();

    // ── actionability ──
    let action_verbs = [
        "use",
        "add",
        "remove",
        "check",
        "run",
        "create",
        "avoid",
        "prefer",
        "ensure",
        "implement",
        "consider",
        "try",
        "switch",
        "replace",
        "apply",
        "set",
        "configure",
    ];
    let words: Vec<&str> = lowercased.split_whitespace().collect();
    let actionability = {
        let mut score = 0u8;
        'outer: for (index, word) in words.iter().enumerate() {
            let clean_word: String = word.chars().filter(|c| c.is_alphabetic()).collect();
            if action_verbs.contains(&clean_word.as_str()) {
                // Check whether there is at least one word following the verb.
                if index + 1 < words.len() {
                    score = 2;
                } else {
                    score = 1;
                }
                break 'outer;
            }
        }
        score
    };

    // ── novelty ──
    let hash = semantic_hash(text);
    let novelty = if existing_hashes.contains(&hash) { 0 } else { 2 };

    // ── reasoning ──
    let causal_words = [
        "because",
        "since",
        "therefore",
        "so that",
        "in order to",
        "due to",
        "as a result",
        "which means",
        "this means",
    ];
    let causal_count = causal_words
        .iter()
        .filter(|causal| lowercased.contains(*causal))
        .count();
    let reasoning = if causal_count == 0 {
        0
    } else if causal_count >= 1 && text.len() > 50 {
        2
    } else {
        1
    };

    // ── specificity ──
    let has_digits = text.chars().any(|character| character.is_ascii_digit());
    let has_path = text.contains('/') || text.contains('\\');
    let has_identifier = text.contains('_') || text.contains('.');
    let named_tools = [
        "shell",
        "exec",
        "sqlx",
        "tokio",
        "lancedb",
        "redb",
        "sqlite",
    ];
    let has_named_tool = named_tools.iter().any(|tool| lowercased.contains(tool));

    let marker_count = [has_digits, has_path, has_identifier, has_named_tool]
        .iter()
        .filter(|flag| **flag)
        .count();
    let specificity = if marker_count == 0 {
        0
    } else if marker_count == 1 {
        1
    } else {
        2
    };

    // ── outcome_linked ──
    let outcome_terms = [
        "success", "fail", "error", "worked", "broke", "fixed", "resolved",
    ];
    let strong_link_phrases = [
        "succeeded because",
        "failed due",
        "resulted in",
        "caused by",
        "led to",
    ];
    let has_outcome_terms = outcome_terms
        .iter()
        .any(|term| lowercased.contains(term));
    let has_strong_link = strong_link_phrases
        .iter()
        .any(|phrase| lowercased.contains(phrase));

    let outcome_linked = if has_strong_link {
        2
    } else if has_outcome_terms {
        1
    } else {
        0
    };

    // ── ethics ──
    let ethics_violation_phrases = [
        "delete all",
        "force push",
        "override safety",
        "skip validation",
        "rm -rf",
    ];
    let ethics_critical_phrases = [
        "ignore errors",
        "bypass security",
        "disable auth",
        "drop database",
    ];
    let ethics = if ethics_critical_phrases
        .iter()
        .any(|phrase| lowercased.contains(phrase))
    {
        0
    } else if ethics_violation_phrases
        .iter()
        .any(|phrase| lowercased.contains(phrase))
    {
        1
    } else {
        2
    };

    QualityScores {
        actionability,
        novelty,
        reasoning,
        specificity,
        outcome_linked,
        ethics,
    }
}

// ── Main evaluation ──────────────────────────────────────────────────────────

pub fn evaluate(text: &str, existing_hashes: &HashSet<String>) -> RalphResult {
    let input_hash = semantic_hash(text);

    // Duplicate check comes first.
    if existing_hashes.contains(&input_hash) {
        return RalphResult {
            verdict: RalphVerdict::Duplicate,
            scores: QualityScores::zero(),
            total_score: 0.0,
            input_hash,
        };
    }

    // Primitive check before scoring.
    if is_primitive(text) {
        return RalphResult {
            verdict: RalphVerdict::Primitive,
            scores: QualityScores::zero(),
            total_score: 0.0,
            input_hash,
        };
    }

    let scores = score_dimensions(text, existing_hashes);
    let total_score = scores.total();

    let verdict = if total_score >= 4.0 {
        RalphVerdict::Quality
    } else if total_score >= 2.0 {
        RalphVerdict::NeedsWork
    } else {
        RalphVerdict::Primitive
    };

    RalphResult {
        verdict,
        scores,
        total_score,
        input_hash,
    }
}

// ── Auto-refinement ───────────────────────────────────────────────────────────

pub fn attempt_refinement(text: &str) -> Option<String> {
    // Too short to refine programmatically.
    if text.len() < 30 {
        return None;
    }

    // If the text already contains a "because" clause, extract it and surface it
    // as leading context to improve scannability.
    let lowercased = text.to_lowercase();
    if let Some(because_position) = lowercased.find("because") {
        // Only restructure if "because" is not already at the start.
        if because_position > 5 {
            let before_because = text[..because_position].trim();
            let from_because = text[because_position..].trim();
            // Only emit if we actually produce something longer and substantive.
            if !before_because.is_empty() && !from_because.is_empty() {
                let restructured =
                    format!("Context: {from_because}. Insight: {before_because}.");
                if restructured.len() > text.len() {
                    return Some(restructured);
                }
            }
        }
    }

    // No programmatic improvement available — keep it honest.
    None
}

// ── DB persistence ────────────────────────────────────────────────────────────

pub async fn save_verdict(
    store: &LearningStore,
    input_text: &str,
    result: &RalphResult,
    source_type: Option<&str>,
) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let verdict_string = result.verdict.to_string();
    let scores_json = serde_json::to_string(&result.scores)?;

    sqlx::query(
        r#"
        INSERT INTO ralph_verdicts
            (id, input_text, input_hash, verdict, scores, total_score, source_type, created_at)
        VALUES
            (?, ?, ?, ?, ?, ?, ?, datetime('now'))
        "#,
    )
    .bind(&id)
    .bind(input_text)
    .bind(&result.input_hash)
    .bind(&verdict_string)
    .bind(&scores_json)
    .bind(result.total_score)
    .bind(source_type)
    .execute(store.pool())
    .await?;

    Ok(())
}

pub async fn load_existing_hashes(store: &LearningStore) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as(r#"SELECT input_hash FROM ralph_verdicts"#)
            .fetch_all(store.pool())
            .await?;

    let hashes = rows.into_iter().map(|(hash,)| hash).collect();
    Ok(hashes)
}
