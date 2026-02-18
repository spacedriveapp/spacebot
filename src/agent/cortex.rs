//! Cortex: System-level observer and memory bulletin generator.
//!
//! The cortex's primary responsibility is generating the **memory bulletin** — a
//! periodically refreshed, LLM-curated summary of the agent's current knowledge.
//! This bulletin is injected into every channel's system prompt, giving all
//! conversations ambient awareness of who the user is, what's been decided,
//! what happened recently, and what's going on.
//!
//! The cortex also observes system-wide activity via signals for future use in
//! health monitoring and memory consolidation.

use crate::error::Result;
use crate::hooks::CortexHook;
use crate::llm::SpacebotModel;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort};
use crate::memory::types::{Association, MemoryType, RelationType};
use crate::{AgentDeps, ProcessEvent, ProcessType};

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use serde::Serialize;
use sqlx::{Row as _, SqlitePool};

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// The cortex observes system-wide activity and maintains the memory bulletin.
pub struct Cortex {
    pub deps: AgentDeps,
    pub hook: CortexHook,
    /// Recent activity signals (rolling window).
    pub signal_buffer: Arc<RwLock<Vec<Signal>>>,
    /// System prompt loaded from prompts/CORTEX.md.
    pub system_prompt: String,
}

/// A high-level activity signal (not raw conversation).
#[derive(Debug, Clone)]
pub enum Signal {
    /// Channel started.
    ChannelStarted { channel_id: String },
    /// Channel ended.
    ChannelEnded { channel_id: String },
    /// Memory was saved.
    MemorySaved {
        memory_type: String,
        content_summary: String,
        importance: f32,
    },
    /// Worker completed.
    WorkerCompleted {
        task_summary: String,
        result_summary: String,
    },
    /// Compaction occurred.
    Compaction {
        channel_id: String,
        turns_compacted: i64,
    },
    /// Error occurred.
    Error {
        component: String,
        error_summary: String,
    },
}

/// A persisted cortex action record.
#[derive(Debug, Clone, Serialize)]
pub struct CortexEvent {
    pub id: String,
    pub event_type: String,
    pub summary: String,
    pub details: Option<serde_json::Value>,
    pub created_at: String,
}

/// Persists cortex actions to SQLite for audit and UI display.
///
/// All writes are fire-and-forget — they spawn a tokio task and return
/// immediately so the cortex never blocks on a DB write.
#[derive(Debug, Clone)]
pub struct CortexLogger {
    pool: SqlitePool,
}

impl CortexLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Log a cortex action. Fire-and-forget.
    pub fn log(&self, event_type: &str, summary: &str, details: Option<serde_json::Value>) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let event_type = event_type.to_string();
        let summary = summary.to_string();
        let details_json = details.map(|d| d.to_string());

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO cortex_events (id, event_type, summary, details) VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&event_type)
            .bind(&summary)
            .bind(&details_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, "failed to persist cortex event");
            }
        });
    }

    /// Load cortex events with optional type filter, newest first.
    pub async fn load_events(
        &self,
        limit: i64,
        offset: i64,
        event_type: Option<&str>,
    ) -> std::result::Result<Vec<CortexEvent>, sqlx::Error> {
        let rows = if let Some(event_type) = event_type {
            sqlx::query_as::<_, CortexEventRow>(
                "SELECT id, event_type, summary, details, created_at FROM cortex_events \
                 WHERE event_type = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(event_type)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, CortexEventRow>(
                "SELECT id, event_type, summary, details, created_at FROM cortex_events \
                 ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|row| row.into_event()).collect())
    }

    /// Count cortex events with optional type filter.
    pub async fn count_events(
        &self,
        event_type: Option<&str>,
    ) -> std::result::Result<i64, sqlx::Error> {
        let count: (i64,) = if let Some(event_type) = event_type {
            sqlx::query_as("SELECT COUNT(*) FROM cortex_events WHERE event_type = ?")
                .bind(event_type)
                .fetch_one(&self.pool)
                .await?
        } else {
            sqlx::query_as("SELECT COUNT(*) FROM cortex_events")
                .fetch_one(&self.pool)
                .await?
        };

        Ok(count.0)
    }
}

/// Internal row type for SQLite query mapping.
#[derive(sqlx::FromRow)]
struct CortexEventRow {
    id: String,
    event_type: String,
    summary: String,
    details: Option<String>,
    created_at: chrono::NaiveDateTime,
}

impl CortexEventRow {
    fn into_event(self) -> CortexEvent {
        CortexEvent {
            id: self.id,
            event_type: self.event_type,
            summary: self.summary,
            details: self.details.and_then(|d| serde_json::from_str(&d).ok()),
            created_at: self.created_at.and_utc().to_rfc3339(),
        }
    }
}

impl Cortex {
    /// Create a new cortex.
    pub fn new(deps: AgentDeps, system_prompt: impl Into<String>) -> Self {
        let hook = CortexHook::new();

        Self {
            deps,
            hook,
            signal_buffer: Arc::new(RwLock::new(Vec::with_capacity(100))),
            system_prompt: system_prompt.into(),
        }
    }

    /// Process a process event and extract signals.
    pub async fn observe(&self, event: ProcessEvent) {
        let signal = match &event {
            ProcessEvent::MemorySaved { memory_id, .. } => Some(Signal::MemorySaved {
                memory_type: "unknown".into(),
                content_summary: format!("memory {}", memory_id),
                importance: 0.5,
            }),
            ProcessEvent::WorkerComplete { result, .. } => Some(Signal::WorkerCompleted {
                task_summary: "completed task".into(),
                result_summary: result.lines().next().unwrap_or("done").into(),
            }),
            ProcessEvent::CompactionTriggered {
                channel_id,
                threshold_reached,
                ..
            } => Some(Signal::Compaction {
                channel_id: channel_id.to_string(),
                turns_compacted: (*threshold_reached * 100.0) as i64,
            }),
            _ => None,
        };

        if let Some(signal) = signal {
            let mut buffer = self.signal_buffer.write().await;
            buffer.push(signal);

            if buffer.len() > 100 {
                buffer.remove(0);
            }

            tracing::debug!("cortex received signal, buffer size: {}", buffer.len());
        }
    }

    /// Run periodic consolidation (future: health monitoring, memory maintenance).
    pub async fn run_consolidation(&self) -> Result<()> {
        tracing::info!("cortex running consolidation");
        Ok(())
    }
}

/// Spawn the cortex bulletin loop for an agent.
///
/// Generates a memory bulletin immediately on startup, then refreshes on a
/// configurable interval. The bulletin is stored in `RuntimeConfig::memory_bulletin`
/// and injected into every channel's system prompt.
pub fn spawn_bulletin_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_bulletin_loop(&deps, &logger).await {
            tracing::error!(%error, "cortex bulletin loop exited with error");
        }
    })
}

async fn run_bulletin_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex bulletin loop started");

    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_SECS: u64 = 15;

    // Run immediately on startup, with retries
    for attempt in 0..=MAX_RETRIES {
        if generate_bulletin(deps, logger).await {
            break;
        }
        if attempt < MAX_RETRIES {
            tracing::info!(
                attempt = attempt + 1,
                max = MAX_RETRIES,
                "retrying bulletin generation in {RETRY_DELAY_SECS}s"
            );
            logger.log(
                "bulletin_failed",
                &format!(
                    "Bulletin generation failed, retrying (attempt {}/{})",
                    attempt + 1,
                    MAX_RETRIES
                ),
                Some(serde_json::json!({ "attempt": attempt + 1, "max_retries": MAX_RETRIES })),
            );
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
        }
    }

    // Generate initial profile after bulletin
    generate_profile(deps, logger).await;

    loop {
        let cortex_config = **deps.runtime_config.cortex.load();
        let interval = cortex_config.bulletin_interval_secs;

        tokio::time::sleep(Duration::from_secs(interval)).await;

        generate_bulletin(deps, logger).await;
        generate_profile(deps, logger).await;
    }
}

/// Bulletin sections: each defines a search mode + config, and how to label the
/// results when presenting them to the synthesis LLM.
struct BulletinSection {
    label: &'static str,
    mode: SearchMode,
    memory_type: Option<MemoryType>,
    sort_by: SearchSort,
    max_results: usize,
}

const BULLETIN_SECTIONS: &[BulletinSection] = &[
    BulletinSection {
        label: "Identity & Core Facts",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Identity),
        sort_by: SearchSort::Importance,
        max_results: 15,
    },
    BulletinSection {
        label: "Recent Memories",
        mode: SearchMode::Recent,
        memory_type: None,
        sort_by: SearchSort::Recent,
        max_results: 15,
    },
    BulletinSection {
        label: "Decisions",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Decision),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "High-Importance Context",
        mode: SearchMode::Important,
        memory_type: None,
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Preferences & Patterns",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Preference),
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Active Goals",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Goal),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Recent Events",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Event),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Observations",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Observation),
        sort_by: SearchSort::Recent,
        max_results: 5,
    },
];

/// Gather raw memory data for each bulletin section by querying the store directly.
/// Returns formatted sections ready for LLM synthesis.
async fn gather_bulletin_sections(deps: &AgentDeps) -> String {
    let mut output = String::new();

    for section in BULLETIN_SECTIONS {
        let config = SearchConfig {
            mode: section.mode,
            memory_type: section.memory_type,
            sort_by: section.sort_by,
            max_results: section.max_results,
            ..Default::default()
        };

        let results = match deps.memory_search.search("", &config).await {
            Ok(results) => results,
            Err(error) => {
                tracing::warn!(
                    section = section.label,
                    %error,
                    "bulletin section query failed"
                );
                continue;
            }
        };

        if results.is_empty() {
            continue;
        }

        output.push_str(&format!("### {}\n\n", section.label));
        for result in &results {
            output.push_str(&format!(
                "- [{}] (importance: {:.1}) {}\n",
                result.memory.memory_type,
                result.memory.importance,
                result
                    .memory
                    .content
                    .lines()
                    .next()
                    .unwrap_or(&result.memory.content),
            ));
        }
        output.push('\n');
    }

    output
}

/// Generate a memory bulletin and store it in RuntimeConfig.
///
/// Programmatically queries the memory store across multiple dimensions
/// (identity, recent, decisions, importance, preferences, goals, events,
/// observations), then asks an LLM to synthesize the raw results into a
/// concise briefing.
///
/// On failure, the previous bulletin is preserved (not blanked out).
/// Returns `true` if the bulletin was successfully generated.
#[tracing::instrument(skip(deps, logger), fields(agent_id = %deps.agent_id))]
pub async fn generate_bulletin(deps: &AgentDeps, logger: &CortexLogger) -> bool {
    tracing::info!("cortex generating memory bulletin");
    let started = Instant::now();

    // Phase 1: Programmatically gather raw memory sections (no LLM needed)
    let raw_sections = gather_bulletin_sections(deps).await;
    let section_count = raw_sections.matches("### ").count();

    if raw_sections.is_empty() {
        tracing::info!("no memories found, skipping bulletin synthesis");
        deps.runtime_config
            .memory_bulletin
            .store(Arc::new(String::new()));
        logger.log(
            "bulletin_generated",
            "Bulletin skipped: no memories in graph",
            Some(serde_json::json!({
                "word_count": 0,
                "sections": 0,
                "duration_ms": started.elapsed().as_millis() as u64,
                "skipped": true,
            })),
        );
        return true;
    }

    // Phase 2: LLM synthesis of raw sections into a cohesive bulletin
    let cortex_config = **deps.runtime_config.cortex.load();
    let prompt_engine = deps.runtime_config.prompts.load();
    let bulletin_prompt = prompt_engine
        .render_static("cortex_bulletin")
        .expect("failed to render cortex bulletin prompt");

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_routing((**routing).clone());

    // No tools needed — the LLM just synthesizes the pre-gathered data
    let agent = AgentBuilder::new(model).preamble(&bulletin_prompt).build();

    let synthesis_prompt = prompt_engine
        .render_system_cortex_synthesis(cortex_config.bulletin_max_words, &raw_sections)
        .expect("failed to render cortex synthesis prompt");

    match agent.prompt(&synthesis_prompt).await {
        Ok(bulletin) => {
            let word_count = bulletin.split_whitespace().count();
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::info!(
                words = word_count,
                bulletin = %bulletin,
                "cortex bulletin generated"
            );
            deps.runtime_config
                .memory_bulletin
                .store(Arc::new(bulletin));
            logger.log(
                "bulletin_generated",
                &format!("Bulletin generated: {word_count} words, {section_count} sections, {duration_ms}ms"),
                Some(serde_json::json!({
                    "word_count": word_count,
                    "sections": section_count,
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
            true
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::error!(%error, "cortex bulletin synthesis failed, keeping previous bulletin");
            logger.log(
                "bulletin_failed",
                &format!("Bulletin synthesis failed after {duration_ms}ms: {error}"),
                Some(serde_json::json!({
                    "error": error.to_string(),
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
            false
        }
    }
}

// -- Agent Profile --

/// Persisted agent profile generated by the cortex.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AgentProfile {
    pub agent_id: String,
    pub display_name: Option<String>,
    pub status: Option<String>,
    pub bio: Option<String>,
    pub avatar_seed: Option<String>,
    pub generated_at: String,
    pub updated_at: String,
}

/// Load the current profile for an agent, if one exists.
pub async fn load_profile(pool: &SqlitePool, agent_id: &str) -> Option<AgentProfile> {
    sqlx::query_as::<_, AgentProfileRow>(
        "SELECT agent_id, display_name, status, bio, avatar_seed, generated_at, updated_at FROM agent_profile WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|row| row.into_profile())
}

#[derive(sqlx::FromRow)]
struct AgentProfileRow {
    agent_id: String,
    display_name: Option<String>,
    status: Option<String>,
    bio: Option<String>,
    avatar_seed: Option<String>,
    generated_at: chrono::NaiveDateTime,
    updated_at: chrono::NaiveDateTime,
}

impl AgentProfileRow {
    fn into_profile(self) -> AgentProfile {
        AgentProfile {
            agent_id: self.agent_id,
            display_name: self.display_name,
            status: self.status,
            bio: self.bio,
            avatar_seed: self.avatar_seed,
            generated_at: self.generated_at.and_utc().to_rfc3339(),
            updated_at: self.updated_at.and_utc().to_rfc3339(),
        }
    }
}

/// LLM response shape for profile generation.
#[derive(serde::Deserialize)]
struct ProfileLlmResponse {
    display_name: Option<String>,
    status: Option<String>,
    bio: Option<String>,
}

/// Generate an agent profile card and persist it to SQLite.
///
/// Uses the current memory bulletin and identity files as context, then asks
/// an LLM to produce a display name, status line, and short bio.
#[tracing::instrument(skip(deps, logger), fields(agent_id = %deps.agent_id))]
async fn generate_profile(deps: &AgentDeps, logger: &CortexLogger) {
    tracing::info!("cortex generating agent profile");
    let started = Instant::now();

    let prompt_engine = deps.runtime_config.prompts.load();
    let profile_prompt = match prompt_engine.render_static("cortex_profile") {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(%error, "failed to render cortex_profile prompt");
            return;
        }
    };

    // Gather context: identity + current bulletin
    let identity_context = {
        let rendered = deps.runtime_config.identity.load().render();
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    };
    let memory_bulletin = {
        let bulletin = deps.runtime_config.memory_bulletin.load();
        if bulletin.is_empty() {
            None
        } else {
            Some(bulletin.as_ref().clone())
        }
    };

    let synthesis_prompt = match prompt_engine
        .render_system_profile_synthesis(identity_context.as_deref(), memory_bulletin.as_deref())
    {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(%error, "failed to render profile synthesis prompt");
            return;
        }
    };

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_routing((**routing).clone());

    let agent = AgentBuilder::new(model).preamble(&profile_prompt).build();

    match agent.prompt(&synthesis_prompt).await {
        Ok(response) => {
            // Strip markdown code fences if the LLM wraps the JSON
            let cleaned = response
                .trim()
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();

            match serde_json::from_str::<ProfileLlmResponse>(cleaned) {
                Ok(profile_data) => {
                    let duration_ms = started.elapsed().as_millis() as u64;
                    let agent_id = &deps.agent_id;

                    // Use the agent ID as a stable avatar seed
                    let avatar_seed = agent_id.to_string();

                    if let Err(error) = sqlx::query(
                        "INSERT INTO agent_profile (agent_id, display_name, status, bio, avatar_seed, generated_at, updated_at) \
                         VALUES (?, ?, ?, ?, ?, datetime('now'), datetime('now')) \
                         ON CONFLICT(agent_id) DO UPDATE SET \
                         display_name = excluded.display_name, \
                         status = excluded.status, \
                         bio = excluded.bio, \
                         avatar_seed = excluded.avatar_seed, \
                         updated_at = datetime('now')",
                    )
                    .bind(agent_id.as_ref())
                    .bind(&profile_data.display_name)
                    .bind(&profile_data.status)
                    .bind(&profile_data.bio)
                    .bind(&avatar_seed)
                    .execute(&deps.sqlite_pool)
                    .await
                    {
                        tracing::warn!(%error, "failed to persist agent profile");
                        return;
                    }

                    tracing::info!(
                        display_name = ?profile_data.display_name,
                        status = ?profile_data.status,
                        duration_ms,
                        "agent profile generated"
                    );
                    logger.log(
                        "profile_generated",
                        &format!(
                            "Profile generated: {} — \"{}\" ({duration_ms}ms)",
                            profile_data.display_name.as_deref().unwrap_or("unnamed"),
                            profile_data.status.as_deref().unwrap_or("no status"),
                        ),
                        Some(serde_json::json!({
                            "display_name": profile_data.display_name,
                            "status": profile_data.status,
                            "duration_ms": duration_ms,
                            "model": model_name,
                        })),
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, raw = %cleaned, "failed to parse profile LLM response as JSON");
                    logger.log(
                        "profile_failed",
                        &format!(
                            "Profile generation failed: could not parse LLM response — {error}"
                        ),
                        Some(serde_json::json!({
                            "error": error.to_string(),
                            "raw_response": cleaned,
                        })),
                    );
                }
            }
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::warn!(%error, "profile generation LLM call failed");
            logger.log(
                "profile_failed",
                &format!("Profile generation failed after {duration_ms}ms: {error}"),
                Some(serde_json::json!({
                    "error": error.to_string(),
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
        }
    }
}

// -- Association loop --

/// Spawn the association loop for an agent.
///
/// Scans memories for embedding similarity and creates association edges
/// between related memories. On first run, backfills all existing memories.
/// Subsequent runs only process memories created since the last pass.
pub fn spawn_association_loop(
    deps: AgentDeps,
    logger: CortexLogger,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_association_loop(&deps, &logger).await {
            tracing::error!(%error, "cortex association loop exited with error");
        }
    })
}

async fn run_association_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex association loop started");

    // Short delay on startup to let the bulletin and embeddings settle
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Backfill: process all existing memories on first run
    let backfill_count = run_association_pass(deps, logger, None).await;
    tracing::info!(
        associations_created = backfill_count,
        "association backfill complete"
    );

    let mut last_pass_at = chrono::Utc::now();

    loop {
        let cortex_config = **deps.runtime_config.cortex.load();
        let interval = cortex_config.association_interval_secs;

        tokio::time::sleep(Duration::from_secs(interval)).await;

        let since = Some(last_pass_at);
        last_pass_at = chrono::Utc::now();

        let count = run_association_pass(deps, logger, since).await;
        if count > 0 {
            tracing::info!(associations_created = count, "association pass complete");
        }
    }
}

/// Run a single association pass.
///
/// If `since` is None, processes all non-forgotten memories (backfill).
/// If `since` is Some, only processes memories created/updated after that time.
/// Returns the number of associations created.
async fn run_association_pass(
    deps: &AgentDeps,
    logger: &CortexLogger,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> usize {
    let cortex_config = **deps.runtime_config.cortex.load();
    let similarity_threshold = cortex_config.association_similarity_threshold;
    let updates_threshold = cortex_config.association_updates_threshold;
    let max_per_pass = cortex_config.association_max_per_pass;
    let is_backfill = since.is_none();

    let store = deps.memory_search.store();
    let embedding_table = deps.memory_search.embedding_table();

    // Get the memories to process
    let memories = match fetch_memories_for_association(&deps.sqlite_pool, since).await {
        Ok(memories) => memories,
        Err(error) => {
            tracing::warn!(%error, "failed to fetch memories for association pass");
            return 0;
        }
    };

    if memories.is_empty() {
        return 0;
    }

    let memory_count = memories.len();
    let mut created = 0_usize;

    for memory_id in &memories {
        if created >= max_per_pass {
            break;
        }

        // Find similar memories via embedding search
        let similar = match embedding_table
            .find_similar(memory_id, similarity_threshold, 10)
            .await
        {
            Ok(results) => results,
            Err(error) => {
                tracing::debug!(memory_id, %error, "similarity search failed for memory");
                continue;
            }
        };

        for (target_id, similarity) in similar {
            if created >= max_per_pass {
                break;
            }

            // Determine relation type based on similarity
            let relation_type = if similarity >= updates_threshold {
                RelationType::Updates
            } else {
                RelationType::RelatedTo
            };

            // Weight: map similarity range to 0.5-1.0
            let weight =
                0.5 + (similarity - similarity_threshold) / (1.0 - similarity_threshold) * 0.5;

            let association = Association::new(memory_id, &target_id, relation_type)
                .with_weight(weight.clamp(0.0, 1.0));

            if let Err(error) = store.create_association(&association).await {
                tracing::debug!(%error, "failed to create association");
                continue;
            }

            created += 1;
        }
    }

    if created > 0 {
        let summary = if is_backfill {
            format!("Backfill: created {created} associations from {memory_count} memories")
        } else {
            format!("Created {created} associations from {memory_count} new memories")
        };

        logger.log(
            "association_created",
            &summary,
            Some(serde_json::json!({
                "associations_created": created,
                "memories_processed": memory_count,
                "backfill": is_backfill,
                "similarity_threshold": similarity_threshold,
                "updates_threshold": updates_threshold,
            })),
        );
    }

    created
}

/// Fetch memory IDs to process for association.
/// If `since` is None, returns all non-forgotten memory IDs (backfill).
/// If `since` is Some, returns IDs of memories created or updated since that time.
async fn fetch_memories_for_association(
    pool: &SqlitePool,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Vec<String>> {
    let rows = if let Some(since) = since {
        sqlx::query(
            "SELECT id FROM memories WHERE forgotten = 0 AND (created_at > ? OR updated_at > ?) ORDER BY created_at DESC",
        )
        .bind(since)
        .bind(since)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id FROM memories WHERE forgotten = 0 ORDER BY importance DESC, created_at DESC",
        )
        .fetch_all(pool)
        .await?
    };

    Ok(rows.iter().map(|row| row.get("id")).collect())
}
