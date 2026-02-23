//! Distillation extraction engine.
//!
//! Converts raw episode data into durable, typed knowledge units called
//! distillations. Five types are supported: Policy, Playbook, SharpEdge,
//! Heuristic, and AntiPattern. Each distillation passes a quality gate before
//! being persisted, and confidence evolves over time through validation and
//! contradiction signals.

use super::store::LearningStore;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The five distillation archetypes.
///
/// Each type has a different priority (lower = higher priority during recall)
/// and a different initial confidence ceiling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DistillationType {
    Policy,
    Playbook,
    SharpEdge,
    Heuristic,
    AntiPattern,
}

impl DistillationType {
    /// Recall priority. Lower numbers surface first.
    pub fn priority(&self) -> u8 {
        match self {
            Self::Policy => 1,
            Self::Playbook => 2,
            Self::SharpEdge => 3,
            Self::Heuristic => 4,
            Self::AntiPattern => 5,
        }
    }

    /// Maximum confidence a distillation of this type may start with.
    ///
    /// New distillations are capped here and grow toward 1.0 only through
    /// repeated validation.
    pub fn initial_confidence_cap(&self) -> f64 {
        match self {
            Self::Policy => 0.40,
            Self::Playbook => 0.30,
            Self::SharpEdge => 0.35,
            Self::Heuristic => 0.40,
            Self::AntiPattern => 0.35,
        }
    }
}

impl std::fmt::Display for DistillationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Policy => write!(f, "policy"),
            Self::Playbook => write!(f, "playbook"),
            Self::SharpEdge => write!(f, "sharp_edge"),
            Self::Heuristic => write!(f, "heuristic"),
            Self::AntiPattern => write!(f, "anti_pattern"),
        }
    }
}

impl std::str::FromStr for DistillationType {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "policy" => Ok(Self::Policy),
            "playbook" => Ok(Self::Playbook),
            "sharp_edge" => Ok(Self::SharpEdge),
            "heuristic" => Ok(Self::Heuristic),
            "anti_pattern" => Ok(Self::AntiPattern),
            other => Err(anyhow::anyhow!("unknown distillation type: {other}")),
        }
    }
}

/// A single distillation unit: a typed, confidence-scored piece of durable
/// knowledge extracted from one or more episodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Distillation {
    pub id: String,
    pub distillation_type: DistillationType,
    pub statement: String,
    pub confidence: f64,
    pub triggers: Vec<String>,
    pub anti_triggers: Vec<String>,
    pub domains: Vec<String>,
    pub times_retrieved: i64,
    pub times_used: i64,
    pub times_helped: i64,
    pub validation_count: i64,
    pub contradiction_count: i64,
    pub source_episode_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Quality gate
// ---------------------------------------------------------------------------

const TAUTOLOGY_PHRASES: &[&str] = &["always check", "be careful", "make sure", "remember to"];

const VAGUE_WORDS: &[&str] = &["generally", "usually", "often", "typically", "sometimes"];

/// Return `true` if the statement is specific enough to be worth storing.
///
/// Rejects statements that are too short, tautological, or so hedged with
/// vague qualifiers that they carry no actionable information.
pub fn passes_quality_gate(statement: &str) -> bool {
    if statement.len() < 20 {
        return false;
    }

    let lower = statement.to_lowercase();

    for phrase in TAUTOLOGY_PHRASES {
        if lower.contains(phrase) {
            return false;
        }
    }

    let vague_count = VAGUE_WORDS
        .iter()
        .filter(|word| lower.contains(*word))
        .count();
    if vague_count >= 2 {
        return false;
    }

    true
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "it", "its", "of", "in", "to", "for", "on",
    "at", "by", "with", "from", "this", "that", "and", "or", "but",
];

fn tokenize_for_overlap(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    lower
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty() && !STOP_WORDS.contains(word))
        .map(String::from)
        .collect()
}

/// Jaccard similarity between two texts after stopword removal.
pub fn word_overlap_score(text_a: &str, text_b: &str) -> f64 {
    let tokens_a = tokenize_for_overlap(text_a);
    let tokens_b = tokenize_for_overlap(text_b);
    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

const SEQUENTIAL_WORDS: &[&str] = &["first", "then", "next", "finally", "after", "second"];

/// Return `true` if the statement references at least two distinct sequential
/// step markers, which indicates it describes a procedure rather than a rule.
fn is_playbook_valid(statement: &str) -> bool {
    let lower = statement.to_lowercase();
    let count = SEQUENTIAL_WORDS
        .iter()
        .filter(|word| lower.contains(*word))
        .count();
    count >= 2
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

const ANTI_PATTERN_SIGNALS: &[&str] = &["avoid", "never", "don't"];
const POLICY_IF_PATTERNS: &[&str] = &["if ", "when "];
const POLICY_THEN_PATTERN: &str = "then";
const SHARP_EDGE_SIGNALS: &[&str] = &["watch out", "careful", "gotcha", "caveat", "edge case"];

/// Infer the most appropriate `DistillationType` from the statement text.
///
/// Classification is heuristic and intentionally conservative: it prefers
/// AntiPattern and Policy (high priority) when clear signals exist, and falls
/// back to Heuristic.
pub fn classify_type(statement: &str) -> DistillationType {
    let lower = statement.to_lowercase();

    if ANTI_PATTERN_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
    {
        return DistillationType::AntiPattern;
    }

    let has_if_or_when = POLICY_IF_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern));
    if has_if_or_when && lower.contains(POLICY_THEN_PATTERN) {
        return DistillationType::Policy;
    }

    if SEQUENTIAL_WORDS.iter().any(|word| lower.contains(word)) && is_playbook_valid(statement) {
        return DistillationType::Playbook;
    }

    if SHARP_EDGE_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
    {
        return DistillationType::SharpEdge;
    }

    DistillationType::Heuristic
}

// ---------------------------------------------------------------------------
// Trigger extraction
// ---------------------------------------------------------------------------

/// Extract likely trigger terms from a statement.
///
/// Pulls out quoted strings, path-like tokens (containing `/` or `.`), and
/// capitalized words that are probably tool names or identifiers.
pub fn extract_triggers(statement: &str) -> Vec<String> {
    let mut triggers: Vec<String> = Vec::new();

    // Quoted strings.
    let mut remaining = statement;
    while let Some(open) = remaining.find('"') {
        remaining = &remaining[open + 1..];
        if let Some(close) = remaining.find('"') {
            let quoted = &remaining[..close];
            if !quoted.is_empty() {
                triggers.push(quoted.to_owned());
            }
            remaining = &remaining[close + 1..];
        } else {
            break;
        }
    }

    // Path-like and capitalized tokens.
    for word in statement.split_whitespace() {
        let cleaned: String = word
            .chars()
            .filter(|character| character.is_alphanumeric() || *character == '.' || *character == '/' || *character == '_' || *character == '-')
            .collect();

        if cleaned.is_empty() {
            continue;
        }

        // Path-like: contains a dot or slash that isn't just punctuation.
        let has_path_char = cleaned.contains('/') || (cleaned.contains('.') && cleaned.len() > 2);
        if has_path_char {
            triggers.push(cleaned.clone());
            continue;
        }

        // Capitalized word that isn't sentence-starting (heuristic: longer than 3
        // chars and not a stop word).
        let lowercase_cleaned = cleaned.to_lowercase();
        if cleaned.len() > 3
            && !STOP_WORDS.contains(&lowercase_cleaned.as_str())
            && cleaned
                .chars()
                .next()
                .map(|character| character.is_uppercase())
                .unwrap_or(false)
        {
            triggers.push(cleaned);
        }
    }

    // Deduplicate while preserving order.
    let mut seen: HashSet<String> = HashSet::new();
    triggers.retain(|trigger| seen.insert(trigger.clone()));

    triggers
}

// ---------------------------------------------------------------------------
// Confidence evolution
// ---------------------------------------------------------------------------

/// Nudge confidence upward after a positive validation signal.
///
/// Uses a dampened approach-to-ceiling formula so confidence grows quickly at
/// first and slows as it nears 1.0.
pub fn confidence_on_validation(current: f64) -> f64 {
    current + (1.0 - current) * 0.1
}

/// Reduce confidence after a contradiction signal.
pub fn confidence_on_contradiction(current: f64) -> f64 {
    current * 0.85
}

// ---------------------------------------------------------------------------
// CRUD operations
// ---------------------------------------------------------------------------

/// Persist a new distillation to the database.
pub async fn save_distillation(
    store: &LearningStore,
    distillation: &Distillation,
) -> anyhow::Result<()> {
    let distillation_type = distillation.distillation_type.to_string();
    let triggers_json = serde_json::to_string(&distillation.triggers)?;
    let anti_triggers_json = serde_json::to_string(&distillation.anti_triggers)?;
    let domains_json = serde_json::to_string(&distillation.domains)?;

    sqlx::query(
        "INSERT INTO distillations (
            id, distillation_type, statement, confidence,
            triggers, anti_triggers, domains,
            times_retrieved, times_used, times_helped,
            validation_count, contradiction_count,
            source_episode_id, created_at, updated_at
        ) VALUES (
            ?, ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?,
            ?, ?,
            ?, datetime('now'), datetime('now')
        )",
    )
    .bind(&distillation.id)
    .bind(&distillation_type)
    .bind(&distillation.statement)
    .bind(distillation.confidence)
    .bind(&triggers_json)
    .bind(&anti_triggers_json)
    .bind(&domains_json)
    .bind(distillation.times_retrieved)
    .bind(distillation.times_used)
    .bind(distillation.times_helped)
    .bind(distillation.validation_count)
    .bind(distillation.contradiction_count)
    .bind(&distillation.source_episode_id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Look for an existing distillation whose statement has > 0.5 word overlap
/// with the candidate statement.
///
/// Returns the ID of the first match, or `None` if no near-duplicate exists.
pub async fn find_duplicate(
    store: &LearningStore,
    statement: &str,
) -> anyhow::Result<Option<String>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, statement FROM distillations")
            .fetch_all(store.pool())
            .await?;

    for (existing_id, existing_statement) in &rows {
        if word_overlap_score(statement, existing_statement) > 0.5 {
            return Ok(Some(existing_id.clone()));
        }
    }

    Ok(None)
}

/// Load all distillations of a given type, ordered by confidence descending.
pub async fn load_by_type(
    store: &LearningStore,
    distillation_type: &DistillationType,
) -> anyhow::Result<Vec<Distillation>> {
    let type_string = distillation_type.to_string();

    let rows: Vec<(
        String, String, String, f64,
        String, String, String,
        i64, i64, i64, i64, i64,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, distillation_type, statement, confidence,
                triggers, anti_triggers, domains,
                times_retrieved, times_used, times_helped,
                validation_count, contradiction_count,
                source_episode_id
         FROM distillations
         WHERE distillation_type = ?
         ORDER BY confidence DESC",
    )
    .bind(&type_string)
    .fetch_all(store.pool())
    .await?;

    let mut distillations = Vec::with_capacity(rows.len());
    for row in rows {
        let (
            id, distillation_type_str, statement, confidence,
            triggers_json, anti_triggers_json, domains_json,
            times_retrieved, times_used, times_helped,
            validation_count, contradiction_count,
            source_episode_id,
        ) = row;

        let parsed_type: DistillationType = distillation_type_str.parse()?;
        let triggers: Vec<String> = serde_json::from_str(&triggers_json).unwrap_or_default();
        let anti_triggers: Vec<String> =
            serde_json::from_str(&anti_triggers_json).unwrap_or_default();
        let domains: Vec<String> = serde_json::from_str(&domains_json).unwrap_or_default();

        distillations.push(Distillation {
            id,
            distillation_type: parsed_type,
            statement,
            confidence,
            triggers,
            anti_triggers,
            domains,
            times_retrieved,
            times_used,
            times_helped,
            validation_count,
            contradiction_count,
            source_episode_id,
        });
    }

    Ok(distillations)
}

/// Overwrite the stored confidence for a distillation.
pub async fn update_confidence(
    store: &LearningStore,
    id: &str,
    new_confidence: f64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE distillations SET confidence = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(new_confidence)
    .bind(id)
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Increment the `times_retrieved` counter for a distillation.
pub async fn increment_retrieved(store: &LearningStore, id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE distillations SET times_retrieved = times_retrieved + 1, \
         updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Delete distillations whose help rate has fallen below 15% after at least
/// ten uses.
///
/// Returns the number of rows deleted.
pub async fn prune_low_performers(store: &LearningStore) -> anyhow::Result<usize> {
    let result = sqlx::query(
        "DELETE FROM distillations
         WHERE (times_used + times_helped) > 0
           AND CAST(times_helped AS REAL) / (times_used + times_helped) < 0.15
           AND (times_used + times_helped) >= 10",
    )
    .execute(store.pool())
    .await?;

    Ok(result.rows_affected() as usize)
}

// ---------------------------------------------------------------------------
// Batch extraction entry point
// ---------------------------------------------------------------------------

/// Heuristic summary derived from raw episode + step rows.
struct EpisodeSummary {
    task: String,
    actual_outcome: String,
    step_count: usize,
    tool_names: Vec<String>,
    step_lessons: Vec<String>,
    retry_depth: usize,
}

/// Load an episode and its steps, then build a flat summary for extraction.
async fn load_episode_summary(
    store: &LearningStore,
    episode_id: &str,
) -> anyhow::Result<Option<EpisodeSummary>> {
    let episode_row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT task, actual_outcome FROM episodes WHERE id = ?")
            .bind(episode_id)
            .fetch_optional(store.pool())
            .await?;

    let (task, actual_outcome) = match episode_row {
        Some((task, outcome)) => (task, outcome.unwrap_or_else(|| "unknown".to_owned())),
        None => return Ok(None),
    };

    let step_rows: Vec<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT tool_name, lesson FROM steps WHERE episode_id = ? ORDER BY created_at")
            .bind(episode_id)
            .fetch_all(store.pool())
            .await?;

    let step_count = step_rows.len();

    let mut tool_names: Vec<String> = Vec::new();
    let mut step_lessons: Vec<String> = Vec::new();

    for (tool_name, lesson) in &step_rows {
        if let Some(name) = tool_name {
            tool_names.push(name.clone());
        }
        if let Some(lesson_text) = lesson {
            if !lesson_text.is_empty() {
                step_lessons.push(lesson_text.clone());
            }
        }
    }

    // Retry depth: maximum number of consecutive invocations of the same tool.
    let retry_depth = compute_max_tool_run(&tool_names);

    Ok(Some(EpisodeSummary {
        task,
        actual_outcome,
        step_count,
        tool_names,
        step_lessons,
        retry_depth,
    }))
}

/// Count the longest consecutive run of the same tool name.
fn compute_max_tool_run(tool_names: &[String]) -> usize {
    let mut max_run = 0usize;
    let mut current_run = 0usize;
    let mut previous: Option<&str> = None;

    for name in tool_names {
        if previous == Some(name.as_str()) {
            current_run += 1;
        } else {
            current_run = 1;
        }
        if current_run > max_run {
            max_run = current_run;
        }
        previous = Some(name.as_str());
    }

    max_run
}

/// Build a candidate statement from an episode summary using heuristic rules.
///
/// Does not call any LLM — all extraction is pattern-based. Returns `None`
/// when the episode data is too sparse to produce a useful statement.
fn heuristic_statement(summary: &EpisodeSummary) -> Option<(String, DistillationType)> {
    let outcome_lower = summary.actual_outcome.to_lowercase();

    // Failed episode: record what went wrong as a sharp edge.
    if outcome_lower == "failure" {
        let lesson = summary.step_lessons.last().cloned().unwrap_or_else(|| {
            format!("Task failed: {}", summary.task)
        });
        let statement = format!("Watch out: {lesson}");
        return Some((statement, DistillationType::SharpEdge));
    }

    // Success with heavy retries: extract a heuristic about the approach.
    if outcome_lower == "success" && summary.retry_depth > 3 {
        let dominant_tool = most_common_tool(&summary.tool_names).unwrap_or("the tool");
        let statement = format!(
            "When working on tasks like \"{}\", expect to call {} multiple times before converging.",
            summary.task, dominant_tool,
        );
        return Some((statement, DistillationType::Heuristic));
    }

    // Success with clear sequential steps: extract a playbook.
    if outcome_lower == "success" && summary.step_count >= 4 {
        let tools: Vec<&str> = unique_ordered_tools(&summary.tool_names);
        if tools.len() >= 2 {
            let step_list = tools.join(", then ");
            let statement = format!(
                "For tasks like \"{}\", first {} to complete the work.",
                summary.task, step_list,
            );
            return Some((statement, DistillationType::Playbook));
        }
    }

    // Straightforward success: extract a basic heuristic.
    if outcome_lower == "success" || outcome_lower == "partial" {
        if let Some(lesson) = summary.step_lessons.first() {
            let statement = format!("When doing \"{}\", note: {lesson}", summary.task);
            return Some((statement, DistillationType::Heuristic));
        }
        let statement = format!(
            "Tasks described as \"{}\" can typically be completed in {} steps.",
            summary.task, summary.step_count,
        );
        return Some((statement, DistillationType::Heuristic));
    }

    None
}

/// Return the tool name that appears most often in the list.
fn most_common_tool<'names>(tool_names: &'names [String]) -> Option<&'names str> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for name in tool_names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(name, _)| name)
}

/// Return tool names in first-seen order, deduplicated.
fn unique_ordered_tools(tool_names: &[String]) -> Vec<&str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut ordered: Vec<&str> = Vec::new();
    for name in tool_names {
        if seen.insert(name.as_str()) {
            ordered.push(name.as_str());
        }
    }
    ordered
}

/// Extract distillations from a completed episode using heuristic rules.
///
/// Loads the episode and its steps, builds candidate statements, runs each
/// through the quality gate and duplicate check, then persists and returns
/// every accepted distillation.
pub async fn extract_from_episode(
    store: &LearningStore,
    episode_id: &str,
) -> anyhow::Result<Vec<Distillation>> {
    let summary = match load_episode_summary(store, episode_id).await? {
        Some(summary) => summary,
        None => return Ok(Vec::new()),
    };

    let candidates: Vec<(String, Option<DistillationType>)> =
        if let Some((statement, suggested_type)) = heuristic_statement(&summary) {
            vec![(statement, Some(suggested_type))]
        } else {
            Vec::new()
        };

    let mut accepted: Vec<Distillation> = Vec::new();

    for (statement, suggested_type) in candidates {
        if !passes_quality_gate(&statement) {
            continue;
        }

        if find_duplicate(store, &statement).await?.is_some() {
            continue;
        }

        let distillation_type = suggested_type.unwrap_or_else(|| classify_type(&statement));
        let triggers = extract_triggers(&statement);
        let confidence_cap = distillation_type.initial_confidence_cap();

        let distillation = Distillation {
            id: Uuid::new_v4().to_string(),
            distillation_type,
            statement,
            confidence: confidence_cap,
            triggers,
            anti_triggers: Vec::new(),
            domains: Vec::new(),
            times_retrieved: 0,
            times_used: 0,
            times_helped: 0,
            validation_count: 0,
            contradiction_count: 0,
            source_episode_id: Some(episode_id.to_owned()),
        };

        save_distillation(store, &distillation).await?;
        accepted.push(distillation);
    }

    Ok(accepted)
}
