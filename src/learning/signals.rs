//! Cognitive Signal Extraction from user messages.
//!
//! Analyzes incoming message content to detect topical domains (e.g. coding,
//! finance, health) and cognitive patterns (preferences, corrections, decisions)
//! that may carry information worth learning. Signals that pass detection are
//! persisted to the `cognitive_signals` table for downstream processing by the
//! Meta-Ralph quality pipeline.

use super::store::LearningStore;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json;

use std::fmt;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Domain detection
// ---------------------------------------------------------------------------

/// Keyword-based domain taxonomy.
///
/// Each entry is `(domain_name, keywords)`. A message matches a domain when
/// any keyword appears as a substring of the lowercased text.
const DOMAINS: &[(&str, &[&str])] = &[
    (
        "coding",
        &["code", "function", "bug", "compile", "deploy", "git", "rust", "python"],
    ),
    (
        "research",
        &["search", "find", "look up", "source", "reference", "paper"],
    ),
    (
        "productivity",
        &["task", "deadline", "schedule", "priority", "organize"],
    ),
    (
        "communication",
        &["email", "message", "reply", "draft", "tone"],
    ),
    (
        "health",
        &["exercise", "sleep", "diet", "wellness", "mental"],
    ),
    (
        "finance",
        &["budget", "expense", "invest", "save", "cost"],
    ),
    (
        "learning",
        &["study", "course", "practice", "understand", "concept"],
    ),
    (
        "creative",
        &["design", "write", "create", "idea", "brainstorm"],
    ),
    (
        "social",
        &["relationship", "friend", "family", "network", "community"],
    ),
    (
        "maintenance",
        &["fix", "repair", "clean", "organize", "maintain"],
    ),
];

// ---------------------------------------------------------------------------
// Cognitive pattern taxonomy
// ---------------------------------------------------------------------------

/// A structural category of cognitive information expressed in a user message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CognitivePattern {
    Remember,
    Preference,
    Decision,
    Correction,
    Reasoning,
}

impl fmt::Display for CognitivePattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            CognitivePattern::Remember => "remember",
            CognitivePattern::Preference => "preference",
            CognitivePattern::Decision => "decision",
            CognitivePattern::Correction => "correction",
            CognitivePattern::Reasoning => "reasoning",
        };
        formatter.write_str(label)
    }
}

// ---------------------------------------------------------------------------
// Pattern regexes
// ---------------------------------------------------------------------------

/// Compiled regular expressions for each [`CognitivePattern`].
///
/// Uses [`LazyLock`] so compilation happens once on first access.
static PATTERN_REGEXES: LazyLock<Vec<(CognitivePattern, Regex)>> = LazyLock::new(|| {
    vec![
        (
            CognitivePattern::Remember,
            Regex::new(r"(?i)\b(remember that|keep in mind|don't forget|note that)\b").unwrap(),
        ),
        (
            CognitivePattern::Preference,
            Regex::new(r"(?i)\b(i prefer|i like|i don't like|i hate|always use|never use)\b")
                .unwrap(),
        ),
        (
            CognitivePattern::Decision,
            Regex::new(r"(?i)\b(let's go with|i decided|we'll use|going with|chose to)\b").unwrap(),
        ),
        (
            CognitivePattern::Correction,
            Regex::new(r"(?i)\b(that's wrong|actually|no,? i meant|fix that|not what i)\b")
                .unwrap(),
        ),
        (
            CognitivePattern::Reasoning,
            Regex::new(r"(?i)\b(because|the reason is|this works because|since|therefore)\b")
                .unwrap(),
        ),
    ]
});

// ---------------------------------------------------------------------------
// Signal type
// ---------------------------------------------------------------------------

/// A structured representation of a single analyzed user message.
#[derive(Debug, Clone)]
pub struct CognitiveSignal {
    /// Unique identifier (UUIDv4).
    pub id: String,
    /// The original message text that was analyzed.
    pub message_content: String,
    /// Domain names matched by keyword detection.
    pub detected_domains: Vec<String>,
    /// Cognitive patterns matched by regex detection.
    pub detected_patterns: Vec<CognitivePattern>,
    /// The first sentence that carried a recognized pattern, capped at 500 chars.
    pub extracted_candidate: Option<String>,
}

// ---------------------------------------------------------------------------
// Detection functions
// ---------------------------------------------------------------------------

/// Return the domain names whose keywords appear in `text`.
///
/// Matching is performed on the lowercased text so keyword casing is
/// irrelevant. Each domain is included at most once.
pub fn detect_domains(text: &str) -> Vec<String> {
    let lowercased = text.to_lowercase();
    DOMAINS
        .iter()
        .filter(|(_, keywords)| keywords.iter().any(|keyword| lowercased.contains(keyword)))
        .map(|(domain_name, _)| domain_name.to_string())
        .collect()
}

/// Return the [`CognitivePattern`] variants whose regexes match `text`.
///
/// Each pattern is included at most once, in the same order as
/// [`PATTERN_REGEXES`].
pub fn detect_patterns(text: &str) -> Vec<CognitivePattern> {
    PATTERN_REGEXES
        .iter()
        .filter(|(_, regex)| regex.is_match(text))
        .map(|(pattern, _)| pattern.clone())
        .collect()
}

/// Extract the first sentence in `text` that matches any pattern regex.
///
/// Sentences are split on `.`, `!`, and `?`. The returned string is trimmed
/// and capped at 500 characters. Returns `None` when no sentence matches.
pub fn extract_candidate(text: &str, patterns: &[CognitivePattern]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }

    let sentences: Vec<&str> = text
        .split(|character: char| matches!(character, '.' | '!' | '?'))
        .collect();

    for sentence in sentences {
        let trimmed = sentence.trim();
        if trimmed.is_empty() {
            continue;
        }

        let matches_any = PATTERN_REGEXES
            .iter()
            .filter(|(pattern, _)| patterns.contains(pattern))
            .any(|(_, regex)| regex.is_match(trimmed));

        if matches_any {
            let candidate = if trimmed.len() > 500 {
                trimmed[..500].to_string()
            } else {
                trimmed.to_string()
            };
            return Some(candidate);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Analyze a user message and produce a [`CognitiveSignal`] if meaningful
/// content is detected.
///
/// Returns `None` when neither domains nor patterns are found — indicating the
/// message carries no learnable signal.
pub fn analyze_message(message_content: &str) -> Option<CognitiveSignal> {
    let detected_domains = detect_domains(message_content);
    let detected_patterns = detect_patterns(message_content);

    if detected_domains.is_empty() && detected_patterns.is_empty() {
        return None;
    }

    let extracted_candidate = extract_candidate(message_content, &detected_patterns);

    Some(CognitiveSignal {
        id: uuid::Uuid::new_v4().to_string(),
        message_content: message_content.to_string(),
        detected_domains,
        detected_patterns,
        extracted_candidate,
    })
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Persist a [`CognitiveSignal`] to the `cognitive_signals` table.
///
/// `ralph_verdict` is an optional quality label supplied by the Meta-Ralph
/// pipeline after downstream evaluation. Pass `None` when saving the signal
/// before evaluation has run.
pub async fn save_signal(
    store: &LearningStore,
    signal: &CognitiveSignal,
    ralph_verdict: Option<&str>,
) -> anyhow::Result<()> {
    let detected_domains_json = serde_json::to_string(&signal.detected_domains)?;
    let detected_patterns_json = serde_json::to_string(&signal.detected_patterns)?;

    sqlx::query(
        "INSERT INTO cognitive_signals \
         (id, message_content, detected_domains, detected_patterns, extracted_candidate, ralph_verdict, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
    )
    .bind(&signal.id)
    .bind(&signal.message_content)
    .bind(&detected_domains_json)
    .bind(&detected_patterns_json)
    .bind(&signal.extracted_candidate)
    .bind(ralph_verdict)
    .execute(store.pool())
    .await?;

    Ok(())
}
