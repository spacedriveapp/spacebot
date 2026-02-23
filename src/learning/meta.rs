//! Meta-learning insight store: CRUD, scoring, promotion criteria, and type mapping.
//!
//! Insights are the highest-level artefacts in the learning system — distilled
//! knowledge that has been validated enough times to be worth promoting into the
//! long-term memory graph. This module owns:
//!
//! - [`InsightCategory`] — the eight knowledge domains.
//! - [`Insight`] — the persistent insight record.
//! - Pure scoring functions (`update_reliability`, `boost_confidence`,
//!   `compute_advisory_readiness`) that work on plain `f64` values so callers
//!   can unit-test them without a database.
//! - [`cold_start_factor`] — threshold relaxation during bootstrap.
//! - [`meets_promotion_criteria`] — single truth for promotion eligibility.
//! - [`promotion_mapping`] — maps an insight to the correct memory type and
//!   importance score before writing to the memory graph.
//! - Async CRUD helpers that operate directly on [`LearningStore`].

use super::store::LearningStore;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// InsightCategory
// ---------------------------------------------------------------------------

/// The eight knowledge domains that an insight can belong to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InsightCategory {
    /// Patterns about the agent's own reasoning and behaviour.
    SelfAwareness,
    /// Beliefs and preferences observed about the user.
    UserModel,
    /// Heuristics that improve the quality of reasoning chains.
    Reasoning,
    /// Situational patterns that are highly context-dependent.
    Context,
    /// Durable, broadly applicable principles.
    Wisdom,
    /// Patterns that improve how the agent communicates.
    Communication,
    /// Specialised knowledge about a subject-matter domain.
    DomainExpertise,
    /// Patterns specific to the user–agent relationship.
    Relationship,
}

impl InsightCategory {
    /// How many days until this category's insights decay to half their
    /// importance. `None` means the category never decays.
    pub fn decay_half_life_days(&self) -> Option<u64> {
        match self {
            Self::SelfAwareness => Some(90),
            Self::UserModel => None,
            Self::Reasoning => Some(60),
            Self::Context => Some(45),
            Self::Wisdom => None,
            Self::Communication => Some(60),
            Self::DomainExpertise => Some(90),
            Self::Relationship => None,
        }
    }
}

impl std::fmt::Display for InsightCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfAwareness => write!(f, "self_awareness"),
            Self::UserModel => write!(f, "user_model"),
            Self::Reasoning => write!(f, "reasoning"),
            Self::Context => write!(f, "context"),
            Self::Wisdom => write!(f, "wisdom"),
            Self::Communication => write!(f, "communication"),
            Self::DomainExpertise => write!(f, "domain_expertise"),
            Self::Relationship => write!(f, "relationship"),
        }
    }
}

// ---------------------------------------------------------------------------
// Insight
// ---------------------------------------------------------------------------

/// A validated meta-learning insight that may be promoted to the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    pub id: String,
    pub category: InsightCategory,
    pub content: String,
    /// Bayesian success rate: proportion of validations over total assessments.
    pub reliability: f64,
    /// Subjective confidence in the insight's applicability, 0.0–1.0.
    pub confidence: f64,
    /// Number of times external evidence has confirmed this insight.
    pub validation_count: i64,
    /// Number of times external evidence has contradicted this insight.
    pub contradiction_count: i64,
    /// Meta-Ralph quality score (0–12), absent until graded.
    pub quality_score: Option<f64>,
    /// Composite readiness score for retrieval as advisory context, 0.0–1.0.
    pub advisory_readiness: f64,
    /// Originating artefact type (e.g. `"heuristic"`, `"policy"`).
    pub source_type: Option<String>,
    /// ID of the originating artefact.
    pub source_id: Option<String>,
    /// Whether this insight has been promoted to the memory graph.
    pub promoted: bool,
    /// Memory graph ID assigned at promotion time.
    pub promoted_memory_id: Option<String>,
}

// ---------------------------------------------------------------------------
// PromotionMapping
// ---------------------------------------------------------------------------

/// Target memory-graph attributes for an insight being promoted.
pub struct PromotionMapping {
    /// The memory type string used when writing to the memory graph.
    pub memory_type: &'static str,
    /// Importance score (0.0–1.0) written alongside the memory.
    pub importance: f64,
}

// ---------------------------------------------------------------------------
// Scoring functions
// ---------------------------------------------------------------------------

/// Compute a Bayesian-smoothed reliability score.
///
/// Uses a Beta(1, 1) prior — equivalent to starting with one success and one
/// failure — so new insights begin at 0.5 rather than ±∞.
///
/// Formula: `(validated + 1) / (validated + contradicted + 2)`
pub fn update_reliability(validated: f64, contradicted: f64) -> f64 {
    (validated + 1.0) / (validated + contradicted + 2.0)
}

/// Nudge confidence upward by 25 % of the remaining headroom, capped at 0.99.
///
/// Each validation closes a quarter of the gap between current confidence
/// and the ceiling, producing diminishing returns without ever reaching 1.0.
pub fn boost_confidence(current: f64) -> f64 {
    (current + (1.0 - current) * 0.25).min(0.99)
}

/// Compute the composite advisory-readiness score for an insight.
///
/// Weights:
/// - 10 % base floor
/// - 45 % from quality score (normalised over max 12)
/// - 20 % from confidence
/// - up to 20 % from validation bonus (capped at 5 validations × 4 %)
/// - minus 8 % per contradiction
///
/// Result is clamped to `[0.0, 1.0]`.
pub fn compute_advisory_readiness(
    quality_score: f64,
    confidence: f64,
    validation_count: i64,
    contradiction_count: i64,
) -> f64 {
    let validation_bonus = (validation_count.min(5) as f64) * 0.04;
    let contradiction_penalty = contradiction_count as f64 * 0.08;

    let readiness = 0.10
        + 0.45 * (quality_score / 12.0)
        + 0.20 * confidence
        + validation_bonus
        - contradiction_penalty;

    readiness.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Cold-start threshold interpolation
// ---------------------------------------------------------------------------

/// Fraction of normal promotion thresholds to apply during cold-start.
///
/// - Below `cold_start_limit / 2` episodes: returns `0.5` (relaxed thresholds).
/// - At or above `cold_start_limit` episodes: returns `1.0` (full thresholds).
/// - Between those points: linear interpolation.
pub fn cold_start_factor(completed_episodes: i64, cold_start_limit: u64) -> f64 {
    let half = (cold_start_limit / 2) as i64;
    let full = cold_start_limit as i64;

    if completed_episodes < half {
        return 0.5;
    }
    if completed_episodes >= full {
        return 1.0;
    }

    let numerator = (completed_episodes - half) as f64;
    let denominator = (full - half) as f64;
    0.5 + 0.5 * (numerator / denominator)
}

// ---------------------------------------------------------------------------
// Promotion criteria
// ---------------------------------------------------------------------------

/// Whether an insight has accumulated enough evidence to be promoted.
///
/// The `cold_factor` (from [`cold_start_factor`]) scales each threshold
/// proportionally so that the system can promote insights sooner during
/// bootstrap without permanently lowering the bar.
///
/// Hard thresholds at `cold_factor = 1.0`:
/// - reliability ≥ 0.70
/// - validation_count ≥ 3
/// - quality_score ≥ 4.0
/// - not already promoted
pub fn meets_promotion_criteria(insight: &Insight, cold_factor: f64) -> bool {
    if insight.promoted {
        return false;
    }

    let reliability_threshold = 0.7 * cold_factor;
    let validation_threshold = (3.0 * cold_factor) as i64;
    let quality_threshold = 4.0 * cold_factor;

    insight.reliability >= reliability_threshold
        && insight.validation_count >= validation_threshold
        && insight.quality_score.unwrap_or(0.0) >= quality_threshold
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// Determine the memory graph type and importance score for a promoted insight.
///
/// Source type takes priority over category so that distillation artefacts
/// (which carry precise semantics) map consistently regardless of the insight
/// category they were derived from.
pub fn promotion_mapping(
    source_type: Option<&str>,
    category: &InsightCategory,
) -> PromotionMapping {
    match source_type {
        Some("heuristic") => PromotionMapping {
            memory_type: "observation",
            importance: 0.6,
        },
        Some("sharp_edge") => PromotionMapping {
            memory_type: "observation",
            importance: 0.7,
        },
        Some("anti_pattern") => PromotionMapping {
            memory_type: "observation",
            importance: 0.7,
        },
        Some("playbook") => PromotionMapping {
            memory_type: "observation",
            importance: 0.8,
        },
        Some("policy") => PromotionMapping {
            memory_type: "decision",
            importance: 0.9,
        },
        _ => match category {
            InsightCategory::UserModel => PromotionMapping {
                memory_type: "preference",
                importance: 0.7,
            },
            InsightCategory::SelfAwareness => PromotionMapping {
                memory_type: "observation",
                importance: 0.5,
            },
            InsightCategory::Reasoning => PromotionMapping {
                memory_type: "observation",
                importance: 0.6,
            },
            _ => PromotionMapping {
                memory_type: "observation",
                importance: 0.5,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// CRUD helpers
// ---------------------------------------------------------------------------

/// Persist a new insight to `learning.db`.
///
/// This is an INSERT; callers are responsible for generating a unique `id`
/// (e.g. via `uuid::Uuid::new_v4()`).
pub async fn save_insight(store: &LearningStore, insight: &Insight) -> anyhow::Result<()> {
    let category = insight.category.to_string();
    let promoted: i64 = if insight.promoted { 1 } else { 0 };

    sqlx::query(
        "INSERT INTO insights \
         (id, category, content, reliability, confidence, validation_count, \
          contradiction_count, quality_score, advisory_readiness, source_type, source_id, \
          promoted, promoted_memory_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))",
    )
    .bind(&insight.id)
    .bind(&category)
    .bind(&insight.content)
    .bind(insight.reliability)
    .bind(insight.confidence)
    .bind(insight.validation_count)
    .bind(insight.contradiction_count)
    .bind(insight.quality_score)
    .bind(insight.advisory_readiness)
    .bind(&insight.source_type)
    .bind(&insight.source_id)
    .bind(promoted)
    .bind(&insight.promoted_memory_id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Load a single insight by ID. Returns `None` if the row does not exist.
pub async fn load_insight(
    store: &LearningStore,
    id: &str,
) -> anyhow::Result<Option<Insight>> {
    let row: Option<InsightRow> = sqlx::query_as(
        "SELECT id, category, content, reliability, confidence, validation_count, \
         contradiction_count, quality_score, advisory_readiness, source_type, source_id, \
         promoted, promoted_memory_id \
         FROM insights WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await?;

    row.map(InsightRow::into_insight).transpose()
}

/// Load all insights that satisfy cold-factor-adjusted promotion thresholds
/// but have not yet been promoted.
///
/// SQL pre-filtering applies numeric comparisons against the adjusted
/// thresholds; [`meets_promotion_criteria`] is not re-evaluated here because
/// the quality floor is already embedded in the query.
pub async fn load_promotable(
    store: &LearningStore,
    cold_factor: f64,
) -> anyhow::Result<Vec<Insight>> {
    let reliability_threshold = 0.7 * cold_factor;
    let validation_threshold = (3.0 * cold_factor) as i64;
    let quality_threshold = 4.0 * cold_factor;

    let rows: Vec<InsightRow> = sqlx::query_as(
        "SELECT id, category, content, reliability, confidence, validation_count, \
         contradiction_count, quality_score, advisory_readiness, source_type, source_id, \
         promoted, promoted_memory_id \
         FROM insights \
         WHERE promoted = 0 \
           AND reliability >= ? \
           AND validation_count >= ? \
           AND quality_score >= ?",
    )
    .bind(reliability_threshold)
    .bind(validation_threshold)
    .bind(quality_threshold)
    .fetch_all(store.pool())
    .await?;

    rows.into_iter().map(InsightRow::into_insight).collect()
}

/// Load insights that have been promoted but whose reliability has since fallen
/// below the demotion floor (< 0.5), indicating they should be retracted.
pub async fn load_demotable(store: &LearningStore) -> anyhow::Result<Vec<Insight>> {
    let rows: Vec<InsightRow> = sqlx::query_as(
        "SELECT id, category, content, reliability, confidence, validation_count, \
         contradiction_count, quality_score, advisory_readiness, source_type, source_id, \
         promoted, promoted_memory_id \
         FROM insights \
         WHERE promoted = 1 AND reliability < 0.5",
    )
    .fetch_all(store.pool())
    .await?;

    rows.into_iter().map(InsightRow::into_insight).collect()
}

/// Record that an insight has been promoted and store the resulting memory ID.
pub async fn mark_promoted(
    store: &LearningStore,
    id: &str,
    memory_id: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE insights \
         SET promoted = 1, promoted_memory_id = ?, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(memory_id)
    .bind(id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Retract a previously promoted insight (set `promoted = 0`, clear memory ID).
pub async fn mark_demoted(store: &LearningStore, id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE insights \
         SET promoted = 0, promoted_memory_id = NULL, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Overwrite the reliability, confidence, and advisory_readiness columns for
/// an insight after a scoring update cycle.
pub async fn update_insight_reliability(
    store: &LearningStore,
    id: &str,
    reliability: f64,
    confidence: f64,
    advisory_readiness: f64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE insights \
         SET reliability = ?, confidence = ?, advisory_readiness = ?, \
             updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(reliability)
    .bind(confidence)
    .bind(advisory_readiness)
    .bind(id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Increment `validation_count` by one and stamp `last_validated_at`.
pub async fn increment_validation(store: &LearningStore, id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE insights \
         SET validation_count = validation_count + 1, \
             last_validated_at = datetime('now'), \
             updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(store.pool())
    .await?;

    Ok(())
}

/// Increment `contradiction_count` by one.
pub async fn increment_contradiction(store: &LearningStore, id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE insights \
         SET contradiction_count = contradiction_count + 1, \
             updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(store.pool())
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal row type
// ---------------------------------------------------------------------------

/// Flat SQLite projection used by `sqlx::query_as`. Converted to [`Insight`]
/// via [`InsightRow::into_insight`] so that the public API never exposes the
/// raw column layout.
#[derive(sqlx::FromRow)]
struct InsightRow {
    id: String,
    category: String,
    content: String,
    reliability: f64,
    confidence: f64,
    validation_count: i64,
    contradiction_count: i64,
    quality_score: Option<f64>,
    advisory_readiness: f64,
    source_type: Option<String>,
    source_id: Option<String>,
    promoted: i64,
    promoted_memory_id: Option<String>,
}

impl InsightRow {
    fn into_insight(self) -> anyhow::Result<Insight> {
        let category = parse_category(&self.category)?;
        Ok(Insight {
            id: self.id,
            category,
            content: self.content,
            reliability: self.reliability,
            confidence: self.confidence,
            validation_count: self.validation_count,
            contradiction_count: self.contradiction_count,
            quality_score: self.quality_score,
            advisory_readiness: self.advisory_readiness,
            source_type: self.source_type,
            source_id: self.source_id,
            promoted: self.promoted != 0,
            promoted_memory_id: self.promoted_memory_id,
        })
    }
}

/// Parse an `InsightCategory` from its snake_case database representation.
fn parse_category(value: &str) -> anyhow::Result<InsightCategory> {
    match value {
        "self_awareness" => Ok(InsightCategory::SelfAwareness),
        "user_model" => Ok(InsightCategory::UserModel),
        "reasoning" => Ok(InsightCategory::Reasoning),
        "context" => Ok(InsightCategory::Context),
        "wisdom" => Ok(InsightCategory::Wisdom),
        "communication" => Ok(InsightCategory::Communication),
        "domain_expertise" => Ok(InsightCategory::DomainExpertise),
        "relationship" => Ok(InsightCategory::Relationship),
        other => anyhow::bail!("unknown insight category: {other}"),
    }
}
