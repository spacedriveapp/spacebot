//! Working memory event log and temporal context assembly.
//!
//! An append-only, structured event log scoped by day. Every significant thing
//! that happens across the agent is recorded as a timestamped event. Channels
//! get a progressively compressed view: today in detail, yesterday as a summary,
//! the week as a paragraph.
//!
//! User messages and agent responses are NOT stored here — they already live in
//! `conversation_messages`. Working memory events capture what happens *around*
//! conversations: worker lifecycle, branch conclusions, cron executions,
//! decisions, errors.

use crate::error::Result;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Typed event categories for working memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingMemoryEventType {
    /// A branch completed with a conclusion.
    BranchCompleted,
    /// A worker was spawned.
    WorkerSpawned,
    /// A worker completed (success or failure).
    WorkerCompleted,
    /// A cron job executed.
    CronExecuted,
    /// A memory was saved (by any path).
    MemorySaved,
    /// A decision was made (extracted from conversation).
    Decision,
    /// The user corrected prior instructions or assumptions.
    UserCorrection,
    /// A prior decision was revised.
    DecisionRevised,
    /// A concrete deadline or due date was set.
    DeadlineSet,
    /// Progress is currently blocked on an external dependency or prerequisite.
    BlockedOn,
    /// An explicit constraint was stated.
    Constraint,
    /// A task or branch reached a terminal result.
    Outcome,
    /// An error or failure occurred.
    Error,
    /// A task was created or updated.
    TaskUpdate,
    /// Cross-agent communication.
    AgentMessage,
    /// System event (startup, config change, maintenance).
    System,
    /// Reserved for tiered memory integration — graph memory promoted to working tier.
    MemoryPromoted,
    /// Reserved for tiered memory integration — working tier memory demoted to graph.
    MemoryDemoted,
}

impl WorkingMemoryEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BranchCompleted => "branch_completed",
            Self::WorkerSpawned => "worker_spawned",
            Self::WorkerCompleted => "worker_completed",
            Self::CronExecuted => "cron_executed",
            Self::MemorySaved => "memory_saved",
            Self::Decision => "decision",
            Self::UserCorrection => "user_correction",
            Self::DecisionRevised => "decision_revised",
            Self::DeadlineSet => "deadline_set",
            Self::BlockedOn => "blocked_on",
            Self::Constraint => "constraint",
            Self::Outcome => "outcome",
            Self::Error => "error",
            Self::TaskUpdate => "task_update",
            Self::AgentMessage => "agent_message",
            Self::System => "system",
            Self::MemoryPromoted => "memory_promoted",
            Self::MemoryDemoted => "memory_demoted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "branch_completed" => Some(Self::BranchCompleted),
            "worker_spawned" => Some(Self::WorkerSpawned),
            "worker_completed" => Some(Self::WorkerCompleted),
            "cron_executed" => Some(Self::CronExecuted),
            "memory_saved" => Some(Self::MemorySaved),
            "decision" => Some(Self::Decision),
            "user_correction" => Some(Self::UserCorrection),
            "decision_revised" => Some(Self::DecisionRevised),
            "deadline_set" => Some(Self::DeadlineSet),
            "blocked_on" => Some(Self::BlockedOn),
            "constraint" => Some(Self::Constraint),
            "outcome" => Some(Self::Outcome),
            "error" => Some(Self::Error),
            "task_update" => Some(Self::TaskUpdate),
            "agent_message" => Some(Self::AgentMessage),
            "system" => Some(Self::System),
            "memory_promoted" => Some(Self::MemoryPromoted),
            "memory_demoted" => Some(Self::MemoryDemoted),
            _ => None,
        }
    }
}

impl fmt::Display for WorkingMemoryEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single working memory event.
#[derive(Debug, Clone)]
pub struct WorkingMemoryEvent {
    pub id: String,
    pub event_type: WorkingMemoryEventType,
    pub timestamp: DateTime<Utc>,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,
    pub summary: String,
    pub detail: Option<String>,
    pub importance: f32,
    /// Denormalized date string (YYYY-MM-DD) in the agent's configured timezone.
    pub day: String,
}

/// A single intra-day synthesis batch (50-100 word paragraph covering a time range).
#[derive(Debug, Clone)]
pub struct IntradaySynthesis {
    pub id: String,
    pub day: String,
    pub time_range_start: DateTime<Utc>,
    pub time_range_end: DateTime<Utc>,
    pub summary: String,
    pub event_count: i64,
    pub created_at: DateTime<Utc>,
}

/// A cortex-synthesized daily narrative.
#[derive(Debug, Clone)]
pub struct DailySummary {
    pub day: String,
    pub summary: String,
    pub event_count: i64,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The append-only working memory event log.
pub struct WorkingMemoryStore {
    pool: SqlitePool,
    timezone: Tz,
}

impl fmt::Debug for WorkingMemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkingMemoryStore")
            .field("timezone", &self.timezone)
            .finish()
    }
}

impl WorkingMemoryStore {
    /// Create a new working memory store.
    ///
    /// The `timezone` is used to compute the `day` column on every event —
    /// a 23:30 UTC event for a UTC+2 agent must be recorded as the next
    /// calendar day.
    pub fn new(pool: SqlitePool, timezone: Tz) -> Arc<Self> {
        Arc::new(Self { pool, timezone })
    }

    /// Compute today's date string in the agent's timezone.
    pub fn today(&self) -> String {
        let now_local = Utc::now().with_timezone(&self.timezone);
        now_local.format("%Y-%m-%d").to_string()
    }

    /// Compute a date string for a UTC timestamp in the agent's timezone.
    fn day_for_timestamp(&self, timestamp: DateTime<Utc>) -> String {
        let local = timestamp.with_timezone(&self.timezone);
        local.format("%Y-%m-%d").to_string()
    }

    /// Compute yesterday's date string in the agent's timezone.
    pub fn yesterday(&self) -> String {
        let now_local = Utc::now().with_timezone(&self.timezone);
        let yesterday = now_local.date_naive() - chrono::Duration::days(1);
        yesterday.format("%Y-%m-%d").to_string()
    }

    /// Get the configured timezone.
    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    // -----------------------------------------------------------------------
    // Fire-and-forget recording
    // -----------------------------------------------------------------------

    /// Fire-and-forget event recording. Spawns a task, never blocks the caller.
    pub fn record(&self, event: WorkingMemoryEvent) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(error) = insert_event(&pool, &event).await {
                tracing::warn!(%error, event_type = %event.event_type, "failed to record working memory event");
            }
        });
    }

    // -----------------------------------------------------------------------
    // Event queries
    // -----------------------------------------------------------------------

    /// Get events for a specific day, ordered by timestamp.
    pub async fn get_events_for_day(&self, day: &str) -> Result<Vec<WorkingMemoryEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
             FROM working_memory_events WHERE day = ? ORDER BY timestamp ASC",
        )
        .bind(day)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_event).collect())
    }

    /// Get recent events for a channel, used for context injection.
    pub async fn get_events_for_channel(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkingMemoryEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
             FROM working_memory_events WHERE channel_id = ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(channel_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        // Reverse so oldest-first for rendering.
        let mut events: Vec<WorkingMemoryEvent> = rows.iter().map(row_to_event).collect();
        events.reverse();
        Ok(events)
    }

    /// Get recent events across all channels, with importance filter.
    pub async fn get_recent_events(
        &self,
        limit: usize,
        min_importance: f32,
    ) -> Result<Vec<WorkingMemoryEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
             FROM working_memory_events WHERE importance >= ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(min_importance)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events: Vec<WorkingMemoryEvent> = rows.iter().map(row_to_event).collect();
        events.reverse();
        Ok(events)
    }

    /// Get recent events for a specific user (for participant context).
    pub async fn get_user_recent_events(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkingMemoryEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
             FROM working_memory_events WHERE user_id = ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(user_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events: Vec<WorkingMemoryEvent> = rows.iter().map(row_to_event).collect();
        events.reverse();
        Ok(events)
    }

    /// Get events after a timestamp for a given day (unsynthesized events).
    pub async fn get_events_after(
        &self,
        day: &str,
        after: Option<DateTime<Utc>>,
    ) -> Result<Vec<WorkingMemoryEvent>> {
        let rows = match after {
            Some(after_ts) => {
                sqlx::query(
                    "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
                     FROM working_memory_events WHERE day = ? AND timestamp > ? ORDER BY timestamp ASC",
                )
                .bind(day)
                .bind(after_ts)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day \
                     FROM working_memory_events WHERE day = ? ORDER BY timestamp ASC",
                )
                .bind(day)
                .fetch_all(&self.pool)
                .await?
            }
        };

        Ok(rows.iter().map(row_to_event).collect())
    }

    /// Count events for a channel since a timestamp (for density trigger).
    pub async fn count_events_since(&self, channel_id: &str, since: DateTime<Utc>) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) as count FROM working_memory_events WHERE channel_id = ? AND timestamp > ?",
        )
        .bind(channel_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    // -----------------------------------------------------------------------
    // Intra-day synthesis
    // -----------------------------------------------------------------------

    /// Get the end timestamp of the last intra-day synthesis for a day.
    pub async fn get_last_intraday_synthesis_end(
        &self,
        day: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let row = sqlx::query(
            "SELECT time_range_end FROM working_memory_intraday_syntheses \
             WHERE day = ? ORDER BY time_range_start DESC LIMIT 1",
        )
        .bind(day)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.get::<DateTime<Utc>, _>("time_range_end")))
    }

    /// Save an intra-day synthesis batch.
    pub async fn save_intraday_synthesis(
        &self,
        day: &str,
        time_start: DateTime<Utc>,
        time_end: DateTime<Utc>,
        summary: &str,
        event_count: usize,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO working_memory_intraday_syntheses \
             (id, day, time_range_start, time_range_end, summary, event_count) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(day)
        .bind(time_start)
        .bind(time_end)
        .bind(summary)
        .bind(event_count as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get all intra-day syntheses for a day (for context rendering and daily rollup).
    pub async fn get_intraday_syntheses(&self, day: &str) -> Result<Vec<IntradaySynthesis>> {
        let rows = sqlx::query(
            "SELECT id, day, time_range_start, time_range_end, summary, event_count, created_at \
             FROM working_memory_intraday_syntheses WHERE day = ? ORDER BY time_range_start ASC",
        )
        .bind(day)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_intraday_synthesis).collect())
    }

    // -----------------------------------------------------------------------
    // Daily summaries
    // -----------------------------------------------------------------------

    /// Check if a daily summary exists.
    pub async fn has_daily_summary(&self, day: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as count FROM working_memory_daily_summaries WHERE day = ?",
        )
        .bind(day)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get::<i64, _>("count") > 0)
    }

    /// Save a daily summary.
    pub async fn save_daily_summary(
        &self,
        day: &str,
        summary: &str,
        event_count: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO working_memory_daily_summaries (day, summary, event_count) \
             VALUES (?, ?, ?)",
        )
        .bind(day)
        .bind(summary)
        .bind(event_count)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get daily summary for a specific day.
    pub async fn get_daily_summary(&self, day: &str) -> Result<Option<DailySummary>> {
        let row = sqlx::query(
            "SELECT day, summary, event_count, created_at \
             FROM working_memory_daily_summaries WHERE day = ?",
        )
        .bind(day)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.as_ref().map(row_to_daily_summary))
    }

    /// Get daily summaries for a date range (for week rendering).
    pub async fn get_daily_summaries_range(
        &self,
        from_day: &str,
        to_day: &str,
    ) -> Result<Vec<DailySummary>> {
        let rows = sqlx::query(
            "SELECT day, summary, event_count, created_at \
             FROM working_memory_daily_summaries \
             WHERE day >= ? AND day <= ? ORDER BY day ASC",
        )
        .bind(from_day)
        .bind(to_day)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_daily_summary).collect())
    }

    // -----------------------------------------------------------------------
    // Pruning
    // -----------------------------------------------------------------------

    /// Prune raw events and intra-day syntheses older than N days.
    /// Daily summaries are never pruned (they are small and serve as permanent history).
    pub async fn prune_old_events(&self, retention_days: i64) -> Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        let cutoff_day = self.day_for_timestamp(cutoff);

        let events_result = sqlx::query("DELETE FROM working_memory_events WHERE day < ?")
            .bind(&cutoff_day)
            .execute(&self.pool)
            .await?;

        let syntheses_result =
            sqlx::query("DELETE FROM working_memory_intraday_syntheses WHERE day < ?")
                .bind(&cutoff_day)
                .execute(&self.pool)
                .await?;

        let total = events_result.rows_affected() + syntheses_result.rows_affected();
        if total > 0 {
            tracing::info!(
                events_pruned = events_result.rows_affected(),
                syntheses_pruned = syntheses_result.rows_affected(),
                cutoff_day = %cutoff_day,
                "pruned old working memory data"
            );
        }
        Ok(total)
    }

    // -----------------------------------------------------------------------
    // Builder API
    // -----------------------------------------------------------------------

    /// Convenience builder for common event emission.
    pub fn emit(
        self: &Arc<Self>,
        event_type: WorkingMemoryEventType,
        summary: impl Into<String>,
    ) -> WorkingMemoryEventBuilder {
        WorkingMemoryEventBuilder::new(Arc::clone(self), event_type, summary.into())
    }
}

// ---------------------------------------------------------------------------
// Rendering (Layers 2 + 3)
// ---------------------------------------------------------------------------

/// Rough token estimate: ~0.75 tokens per character (conservative for English).
fn estimate_tokens(text: &str) -> usize {
    text.len() * 3 / 4
}

/// Render Layer 2: Working Memory section for the channel system prompt.
///
/// Produces a markdown block with today's intra-day synthesis paragraphs,
/// an unsynthesized event tail, yesterday's summary, and this week's summaries.
/// All rendering is programmatic — no LLM calls on this path.
pub async fn render_working_memory(
    store: &WorkingMemoryStore,
    channel_id: &str,
    config: &crate::config::WorkingMemoryConfig,
    timezone: chrono_tz::Tz,
) -> Result<String> {
    use std::fmt::Write;

    if !config.enabled {
        return Ok(String::new());
    }

    let today = store.today();
    let yesterday = store.yesterday();
    let budget = config.context_token_budget;

    let mut output = String::with_capacity(2048);
    let mut tokens_used: usize = 0;
    let today_budget = budget * 60 / 100; // 60% for today
    let yesterday_budget = budget * 20 / 100; // next 20% for yesterday
    let week_budget = budget * 20 / 100; // remaining 20% for this week

    // --- Today header ---
    let now_local = Utc::now().with_timezone(&timezone);
    let day_name = now_local.format("%A, %B %-d").to_string();
    writeln!(output, "## Working Memory\n").ok();
    writeln!(output, "### Today ({day_name})").ok();

    // 1. Intra-day synthesis paragraphs for today.
    let syntheses = store.get_intraday_syntheses(&today).await?;
    for synthesis in &syntheses {
        let time_label = synthesis
            .time_range_start
            .with_timezone(&timezone)
            .format("%H:%M")
            .to_string();
        let block = format!("[{time_label}] {}\n", synthesis.summary);
        let block_tokens = estimate_tokens(&block);
        if tokens_used + block_tokens > today_budget {
            break;
        }
        write!(output, "{block}").ok();
        tokens_used += block_tokens;
    }

    // 2. Unsynthesized event tail (raw events since last synthesis).
    let last_synthesis_end = store.get_last_intraday_synthesis_end(&today).await?;
    let unsynthesized = store.get_events_after(&today, last_synthesis_end).await?;
    let max_tail = config.today_max_unsynthesized_events;

    if !unsynthesized.is_empty() {
        // Only show the header if there are also synthesis blocks above.
        if !syntheses.is_empty() {
            let since_label = last_synthesis_end
                .map(|t| t.with_timezone(&timezone).format("%H:%M").to_string())
                .unwrap_or_else(|| "start".to_string());
            writeln!(output, "\n**Since {since_label}:**").ok();
        }

        let tail_events: Vec<&WorkingMemoryEvent> = if unsynthesized.len() > max_tail {
            // Under token pressure, filter by importance.
            let mut sorted: Vec<&WorkingMemoryEvent> = unsynthesized.iter().collect();
            sorted.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted.truncate(max_tail);
            // Re-sort by timestamp for chronological display.
            sorted.sort_by_key(|e| e.timestamp);
            sorted
        } else {
            unsynthesized.iter().collect()
        };

        // Boost events from the current channel: always include them.
        for event in &tail_events {
            let line = format_event_line(event, channel_id);
            let line_tokens = estimate_tokens(&line);
            if tokens_used + line_tokens > today_budget {
                break;
            }
            writeln!(output, "- {line}").ok();
            tokens_used += line_tokens;
        }
    }

    // If today is completely empty, note it.
    if syntheses.is_empty() && unsynthesized.is_empty() {
        writeln!(output, "*No activity yet today.*").ok();
    }

    // --- Yesterday ---
    let yesterday_summary = store.get_daily_summary(&yesterday).await?;
    if let Some(summary) = yesterday_summary {
        let summary_tokens = estimate_tokens(&summary.summary);
        if summary_tokens <= yesterday_budget {
            let yesterday_local = (Utc::now() - chrono::Duration::days(1)).with_timezone(&timezone);
            let yesterday_name = yesterday_local.format("%A, %B %-d").to_string();
            writeln!(output, "\n### Yesterday ({yesterday_name})").ok();
            writeln!(output, "{}", summary.summary).ok();
            #[allow(unused_assignments)]
            {
                tokens_used += summary_tokens;
            }
        }
    }

    // --- This week (past 5 days, excluding today and yesterday) ---
    let week_start_local = now_local.date_naive() - chrono::Duration::days(6);
    let yesterday_date = now_local.date_naive() - chrono::Duration::days(1);
    // Only fetch days before yesterday.
    let week_end = (yesterday_date - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let week_start = week_start_local.format("%Y-%m-%d").to_string();

    if week_start <= week_end {
        let week_summaries = store
            .get_daily_summaries_range(&week_start, &week_end)
            .await?;
        if !week_summaries.is_empty() {
            let mut week_text = String::new();
            for summary in week_summaries.iter().rev() {
                // Most recent first within the week section.
                let candidate = format!("**{}:** {}\n", summary.day, summary.summary);
                let candidate_tokens = estimate_tokens(&candidate);
                if estimate_tokens(&week_text) + candidate_tokens > week_budget {
                    break;
                }
                week_text.push_str(&candidate);
            }
            if !week_text.is_empty() {
                writeln!(output, "\n### Earlier This Week").ok();
                write!(output, "{week_text}").ok();
            }
        }
    }

    Ok(output)
}

/// Render Layer 3: Channel Activity Map for the system prompt.
///
/// Shows what's happening in other channels. Uses a single SQL query with a
/// correlated subquery to get the last message per channel, plus a batch
/// query for topic hints from working memory events.
pub async fn render_channel_activity_map(
    pool: &sqlx::SqlitePool,
    _working_memory: &WorkingMemoryStore,
    exclude_channel_id: &str,
    config: &crate::config::WorkingMemoryConfig,
    _timezone: chrono_tz::Tz,
) -> Result<String> {
    use std::fmt::Write;

    let inactive_threshold = format!("-{} hours", config.channel_map_inactive_hours);
    let max_channels = config.channel_map_max_channels as i64;

    // Single query: last message per channel, excluding current channel.
    let rows = sqlx::query(
        "SELECT \
            c.id, \
            c.display_name, \
            c.platform, \
            m.sender_name AS last_sender_name, \
            m.created_at AS last_message_at \
         FROM channels c \
         LEFT JOIN conversation_messages m ON m.id = ( \
            SELECT id FROM conversation_messages \
            WHERE channel_id = c.id \
            ORDER BY created_at DESC \
            LIMIT 1 \
         ) \
         WHERE c.id != ? \
           AND c.is_active = 1 \
           AND (m.created_at IS NULL OR m.created_at > datetime('now', ?)) \
         ORDER BY m.created_at DESC NULLS LAST \
         LIMIT ?",
    )
    .bind(exclude_channel_id)
    .bind(&inactive_threshold)
    .bind(max_channels)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(String::new());
    }

    // Collect channel IDs for batch topic hint query.
    let channel_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get::<Option<String>, _>("id"))
        .collect();

    // Batch query: most recent BranchCompleted event per channel for topic hints.
    let topic_hints = get_topic_hints(pool, &channel_ids).await?;

    let now = Utc::now();
    let mut output = String::with_capacity(512);
    writeln!(output, "## Other Channels\n").ok();

    for row in &rows {
        let channel_id: String = row.get("id");
        let display_name: Option<String> = row.get("display_name");
        let _platform: String = row.get("platform");
        let last_sender: Option<String> = row.get("last_sender_name");
        let last_message_at: Option<DateTime<Utc>> = row.get("last_message_at");

        let name = display_name.as_deref().unwrap_or(&channel_id);

        let time_ago = match last_message_at {
            Some(at) => format_time_ago(now, at),
            None => "no messages".to_string(),
        };

        let sender = last_sender.as_deref().unwrap_or("unknown");
        let topic = topic_hints.get(&channel_id);

        let mut line = format!("{name} -- {time_ago}, {sender}");
        if let Some(topic_summary) = topic {
            // Truncate topic to keep the map compact.
            let truncated = if topic_summary.len() > 80 {
                let boundary = topic_summary.floor_char_boundary(80);
                format!("{}...", &topic_summary[..boundary])
            } else {
                topic_summary.clone()
            };
            write!(line, ": {truncated}").ok();
        }
        writeln!(output, "{line}").ok();
    }

    Ok(output)
}

/// Format a single event as a one-line summary for the raw tail.
fn format_event_line(event: &WorkingMemoryEvent, current_channel_id: &str) -> String {
    let type_label = match event.event_type {
        WorkingMemoryEventType::BranchCompleted => "Branch completed",
        WorkingMemoryEventType::WorkerSpawned => "Worker spawned",
        WorkingMemoryEventType::WorkerCompleted => "Worker completed",
        WorkingMemoryEventType::CronExecuted => "Cron executed",
        WorkingMemoryEventType::MemorySaved => "Memory saved",
        WorkingMemoryEventType::Decision => "Decision",
        WorkingMemoryEventType::UserCorrection => "User correction",
        WorkingMemoryEventType::DecisionRevised => "Decision revised",
        WorkingMemoryEventType::DeadlineSet => "Deadline set",
        WorkingMemoryEventType::BlockedOn => "Blocked on",
        WorkingMemoryEventType::Constraint => "Constraint",
        WorkingMemoryEventType::Outcome => "Outcome",
        WorkingMemoryEventType::Error => "Error",
        WorkingMemoryEventType::TaskUpdate => "Task update",
        WorkingMemoryEventType::AgentMessage => "Agent message",
        WorkingMemoryEventType::System => "System",
        WorkingMemoryEventType::MemoryPromoted => "Memory promoted",
        WorkingMemoryEventType::MemoryDemoted => "Memory demoted",
    };

    // Prefix with channel name if the event is from a different channel.
    let channel_prefix = match &event.channel_id {
        Some(cid) if cid != current_channel_id => format!("[{cid}] "),
        _ => String::new(),
    };

    format!("{channel_prefix}{type_label}: {}", event.summary)
}

/// Fetch the most recent BranchCompleted topic hint per channel.
async fn get_topic_hints(
    pool: &sqlx::SqlitePool,
    channel_ids: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    use std::collections::HashMap;

    if channel_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Build a parameterized IN clause. sqlx doesn't support binding Vec<String>
    // to IN directly, so we construct the query dynamically.
    let placeholders: Vec<&str> = (0..channel_ids.len()).map(|_| "?").collect();
    let in_clause = placeholders.join(", ");

    // We need the most recent BranchCompleted per channel. Use a window
    // function approach that works in a single pass.
    let query = format!(
        "SELECT channel_id, summary FROM ( \
            SELECT channel_id, summary, \
                   ROW_NUMBER() OVER (PARTITION BY channel_id ORDER BY timestamp DESC) AS rn \
            FROM working_memory_events \
            WHERE event_type = 'branch_completed' \
              AND channel_id IN ({in_clause}) \
        ) WHERE rn = 1"
    );

    let mut query_builder = sqlx::query(&query);
    for id in channel_ids {
        query_builder = query_builder.bind(id);
    }

    let rows = query_builder.fetch_all(pool).await?;

    let mut hints = HashMap::new();
    for row in &rows {
        let channel_id: String = row.get("channel_id");
        let summary: String = row.get("summary");
        hints.insert(channel_id, summary);
    }

    Ok(hints)
}

/// Format a duration as a human-readable "time ago" string.
fn format_time_ago(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let delta = now - then;
    let minutes = delta.num_minutes();
    if minutes < 1 {
        "just now".to_string()
    } else if minutes < 60 {
        format!("{minutes}m ago")
    } else if minutes < 1440 {
        let hours = minutes / 60;
        format!("{hours}h ago")
    } else {
        let days = minutes / 1440;
        format!("{days}d ago")
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Ergonomic builder for constructing and recording working memory events.
pub struct WorkingMemoryEventBuilder {
    store: Arc<WorkingMemoryStore>,
    event_type: WorkingMemoryEventType,
    summary: String,
    channel_id: Option<String>,
    user_id: Option<String>,
    detail: Option<String>,
    importance: f32,
}

impl WorkingMemoryEventBuilder {
    fn new(
        store: Arc<WorkingMemoryStore>,
        event_type: WorkingMemoryEventType,
        summary: String,
    ) -> Self {
        Self {
            store,
            event_type,
            summary,
            channel_id: None,
            user_id: None,
            detail: None,
            importance: 0.5,
        }
    }

    pub fn channel(mut self, channel_id: impl Into<String>) -> Self {
        self.channel_id = Some(channel_id.into());
        self
    }

    pub fn user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn importance(mut self, importance: f32) -> Self {
        self.importance = importance;
        self
    }

    /// Fire-and-forget: record the event without blocking.
    pub fn record(self) {
        let now = Utc::now();
        let day = self.store.day_for_timestamp(now);

        let event = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: self.event_type,
            timestamp: now,
            channel_id: self.channel_id,
            user_id: self.user_id,
            summary: self.summary,
            detail: self.detail,
            importance: self.importance,
            day,
        };

        self.store.record(event);
    }
}

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> WorkingMemoryEvent {
    let event_type_str: String = row.get("event_type");
    WorkingMemoryEvent {
        id: row.get("id"),
        event_type: WorkingMemoryEventType::parse(&event_type_str)
            .unwrap_or(WorkingMemoryEventType::System),
        timestamp: row.get("timestamp"),
        channel_id: row.get("channel_id"),
        user_id: row.get("user_id"),
        summary: row.get("summary"),
        detail: row.get("detail"),
        importance: row.get("importance"),
        day: row.get("day"),
    }
}

fn row_to_intraday_synthesis(row: &sqlx::sqlite::SqliteRow) -> IntradaySynthesis {
    IntradaySynthesis {
        id: row.get("id"),
        day: row.get("day"),
        time_range_start: row.get("time_range_start"),
        time_range_end: row.get("time_range_end"),
        summary: row.get("summary"),
        event_count: row.get("event_count"),
        created_at: row.get("created_at"),
    }
}

fn row_to_daily_summary(row: &sqlx::sqlite::SqliteRow) -> DailySummary {
    DailySummary {
        day: row.get("day"),
        summary: row.get("summary"),
        event_count: row.get("event_count"),
        created_at: row.get("created_at"),
    }
}

async fn insert_event(pool: &SqlitePool, event: &WorkingMemoryEvent) -> Result<()> {
    sqlx::query(
        "INSERT INTO working_memory_events \
         (id, event_type, timestamp, channel_id, user_id, summary, detail, importance, day) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(event.event_type.as_str())
    .bind(event.timestamp)
    .bind(&event.channel_id)
    .bind(&event.user_id)
    .bind(&event.summary)
    .bind(&event.detail)
    .bind(event.importance)
    .bind(&event.day)
    .execute(pool)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_store() -> Arc<WorkingMemoryStore> {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        WorkingMemoryStore::new(pool, Tz::UTC)
    }

    #[tokio::test]
    async fn test_record_and_query_events() {
        let store = setup_test_store().await;

        let today = store.today();

        // Record a few events directly (bypassing fire-and-forget for test determinism).
        let event1 = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: WorkingMemoryEventType::WorkerSpawned,
            timestamp: Utc::now(),
            channel_id: Some("chan-1".to_string()),
            user_id: None,
            summary: "Worker spawned: compile project".to_string(),
            detail: None,
            importance: 0.6,
            day: today.clone(),
        };
        insert_event(&store.pool, &event1).await.unwrap();

        let event2 = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: WorkingMemoryEventType::BranchCompleted,
            timestamp: Utc::now(),
            channel_id: Some("chan-1".to_string()),
            user_id: Some("user-1".to_string()),
            summary: "Branch concluded: auth module needs refactor".to_string(),
            detail: Some("Full analysis of the auth module...".to_string()),
            importance: 0.8,
            day: today.clone(),
        };
        insert_event(&store.pool, &event2).await.unwrap();

        // Query by day.
        let events = store.get_events_for_day(&today).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, WorkingMemoryEventType::WorkerSpawned);
        assert_eq!(
            events[1].event_type,
            WorkingMemoryEventType::BranchCompleted
        );

        // Query by channel.
        let channel_events = store.get_events_for_channel("chan-1", 10).await.unwrap();
        assert_eq!(channel_events.len(), 2);

        // Query by user.
        let user_events = store.get_user_recent_events("user-1", 10).await.unwrap();
        assert_eq!(user_events.len(), 1);
        assert_eq!(
            user_events[0].event_type,
            WorkingMemoryEventType::BranchCompleted
        );

        // Count since.
        let count = store
            .count_events_since("chan-1", Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_event_type_roundtrip() {
        let store = setup_test_store().await;
        let today = store.today();

        for event_type in [
            WorkingMemoryEventType::BranchCompleted,
            WorkingMemoryEventType::WorkerSpawned,
            WorkingMemoryEventType::WorkerCompleted,
            WorkingMemoryEventType::CronExecuted,
            WorkingMemoryEventType::MemorySaved,
            WorkingMemoryEventType::Decision,
            WorkingMemoryEventType::UserCorrection,
            WorkingMemoryEventType::DecisionRevised,
            WorkingMemoryEventType::DeadlineSet,
            WorkingMemoryEventType::BlockedOn,
            WorkingMemoryEventType::Constraint,
            WorkingMemoryEventType::Outcome,
            WorkingMemoryEventType::Error,
            WorkingMemoryEventType::TaskUpdate,
            WorkingMemoryEventType::AgentMessage,
            WorkingMemoryEventType::System,
            WorkingMemoryEventType::MemoryPromoted,
            WorkingMemoryEventType::MemoryDemoted,
        ] {
            let event = WorkingMemoryEvent {
                id: Uuid::new_v4().to_string(),
                event_type,
                timestamp: Utc::now(),
                channel_id: None,
                user_id: None,
                summary: format!("test {}", event_type.as_str()),
                detail: None,
                importance: 0.5,
                day: today.clone(),
            };
            insert_event(&store.pool, &event).await.unwrap();
        }

        let events = store.get_events_for_day(&today).await.unwrap();
        assert_eq!(events.len(), 18);

        // Verify all types survived the roundtrip.
        let types: Vec<WorkingMemoryEventType> = events.iter().map(|e| e.event_type).collect();
        assert!(types.contains(&WorkingMemoryEventType::BranchCompleted));
        assert!(types.contains(&WorkingMemoryEventType::MemoryDemoted));
    }

    #[tokio::test]
    async fn test_daily_summary_crud() {
        let store = setup_test_store().await;

        assert!(!store.has_daily_summary("2026-03-18").await.unwrap());

        store
            .save_daily_summary("2026-03-18", "Busy day. Shipped 3 features.", 42)
            .await
            .unwrap();

        assert!(store.has_daily_summary("2026-03-18").await.unwrap());

        let summary = store
            .get_daily_summary("2026-03-18")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.summary, "Busy day. Shipped 3 features.");
        assert_eq!(summary.event_count, 42);

        // Idempotent — save again should replace.
        store
            .save_daily_summary("2026-03-18", "Updated summary.", 43)
            .await
            .unwrap();
        let updated = store
            .get_daily_summary("2026-03-18")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.summary, "Updated summary.");
    }

    #[tokio::test]
    async fn test_intraday_synthesis() {
        let store = setup_test_store().await;
        let today = store.today();

        let start = Utc::now() - chrono::Duration::hours(2);
        let end = Utc::now() - chrono::Duration::hours(1);

        store
            .save_intraday_synthesis(&today, start, end, "Morning batch: compiled, tested.", 8)
            .await
            .unwrap();

        let syntheses = store.get_intraday_syntheses(&today).await.unwrap();
        assert_eq!(syntheses.len(), 1);
        assert_eq!(syntheses[0].event_count, 8);

        let last_end = store.get_last_intraday_synthesis_end(&today).await.unwrap();
        assert!(last_end.is_some());
    }

    #[tokio::test]
    async fn test_builder_api() {
        let store = setup_test_store().await;

        // Use the builder to emit an event. Since `record()` is fire-and-forget
        // via tokio::spawn, we need a small sleep for the write to complete.
        store
            .emit(WorkingMemoryEventType::System, "Agent started")
            .importance(0.3)
            .record();

        store
            .emit(
                WorkingMemoryEventType::WorkerSpawned,
                "Worker: compile check",
            )
            .channel("test-channel")
            .importance(0.6)
            .record();

        // Allow the spawned tasks to complete.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let today = store.today();
        let events = store.get_events_for_day(&today).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn test_pruning() {
        let store = setup_test_store().await;

        // Insert an event with an old day.
        let old_event = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: WorkingMemoryEventType::System,
            timestamp: Utc::now() - chrono::Duration::days(60),
            channel_id: None,
            user_id: None,
            summary: "Old event".to_string(),
            detail: None,
            importance: 0.5,
            day: "2026-01-01".to_string(),
        };
        insert_event(&store.pool, &old_event).await.unwrap();

        // Insert a recent event.
        let recent_event = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: WorkingMemoryEventType::System,
            timestamp: Utc::now(),
            channel_id: None,
            user_id: None,
            summary: "Recent event".to_string(),
            detail: None,
            importance: 0.5,
            day: store.today(),
        };
        insert_event(&store.pool, &recent_event).await.unwrap();

        let pruned = store.prune_old_events(30).await.unwrap();
        assert!(pruned >= 1);

        // Recent event should survive.
        let today_events = store.get_events_for_day(&store.today()).await.unwrap();
        assert_eq!(today_events.len(), 1);
    }

    #[tokio::test]
    async fn test_get_events_after() {
        let store = setup_test_store().await;
        let today = store.today();

        let t1 = Utc::now() - chrono::Duration::minutes(30);
        let t2 = Utc::now() - chrono::Duration::minutes(15);
        let t3 = Utc::now();

        for (i, ts) in [t1, t2, t3].iter().enumerate() {
            let event = WorkingMemoryEvent {
                id: Uuid::new_v4().to_string(),
                event_type: WorkingMemoryEventType::System,
                timestamp: *ts,
                channel_id: None,
                user_id: None,
                summary: format!("event {i}"),
                detail: None,
                importance: 0.5,
                day: today.clone(),
            };
            insert_event(&store.pool, &event).await.unwrap();
        }

        // All events for today (no after filter).
        let all = store.get_events_after(&today, None).await.unwrap();
        assert_eq!(all.len(), 3);

        // Events after t1 should be t2 and t3.
        let after_t1 = store.get_events_after(&today, Some(t1)).await.unwrap();
        assert_eq!(after_t1.len(), 2);
    }

    #[tokio::test]
    async fn test_daily_summaries_range() {
        let store = setup_test_store().await;

        store
            .save_daily_summary("2026-03-15", "Day 1", 10)
            .await
            .unwrap();
        store
            .save_daily_summary("2026-03-16", "Day 2", 20)
            .await
            .unwrap();
        store
            .save_daily_summary("2026-03-17", "Day 3", 30)
            .await
            .unwrap();
        store
            .save_daily_summary("2026-03-18", "Day 4", 40)
            .await
            .unwrap();

        let range = store
            .get_daily_summaries_range("2026-03-16", "2026-03-18")
            .await
            .unwrap();
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].day, "2026-03-16");
        assert_eq!(range[2].day, "2026-03-18");
    }

    fn test_config() -> crate::config::WorkingMemoryConfig {
        crate::config::WorkingMemoryConfig::default()
    }

    #[tokio::test]
    async fn test_render_working_memory_empty() {
        let store = setup_test_store().await;
        let config = test_config();

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        assert!(rendered.contains("## Working Memory"), "should have header");
        assert!(
            rendered.contains("No activity yet today"),
            "should note empty day"
        );
    }

    #[tokio::test]
    async fn test_render_working_memory_with_events() {
        let store = setup_test_store().await;
        let config = test_config();
        let today = store.today();

        // Insert some events.
        for i in 0..3 {
            let event = WorkingMemoryEvent {
                id: Uuid::new_v4().to_string(),
                event_type: WorkingMemoryEventType::WorkerCompleted,
                timestamp: Utc::now(),
                channel_id: Some("chan-1".to_string()),
                user_id: None,
                summary: format!("Worker {i} completed successfully"),
                detail: None,
                importance: 0.6,
                day: today.clone(),
            };
            insert_event(&store.pool, &event).await.unwrap();
        }

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        assert!(rendered.contains("## Working Memory"), "should have header");
        assert!(
            rendered.contains("Worker 0 completed"),
            "should contain events"
        );
        assert!(
            rendered.contains("Worker 2 completed"),
            "should contain all events"
        );
        // Should NOT say "No activity" since we have events.
        assert!(!rendered.contains("No activity yet today"));
    }

    #[tokio::test]
    async fn test_render_working_memory_with_synthesis_and_tail() {
        let store = setup_test_store().await;
        let config = test_config();
        let today = store.today();

        // Add an intra-day synthesis.
        let start = Utc::now() - chrono::Duration::hours(3);
        let end = Utc::now() - chrono::Duration::hours(2);
        store
            .save_intraday_synthesis(
                &today,
                start,
                end,
                "Morning: deployed v1.2.0, fixed 3 bugs, ran full test suite.",
                8,
            )
            .await
            .unwrap();

        // Add an unsynthesized event after the synthesis.
        let event = WorkingMemoryEvent {
            id: Uuid::new_v4().to_string(),
            event_type: WorkingMemoryEventType::BranchCompleted,
            timestamp: Utc::now(),
            channel_id: Some("chan-1".to_string()),
            user_id: None,
            summary: "Analyzed auth module for refactoring".to_string(),
            detail: None,
            importance: 0.7,
            day: today.clone(),
        };
        insert_event(&store.pool, &event).await.unwrap();

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        assert!(
            rendered.contains("Morning: deployed v1.2.0"),
            "should contain synthesis"
        );
        assert!(
            rendered.contains("auth module"),
            "should contain tail event"
        );
        assert!(
            rendered.contains("Since"),
            "should have 'Since' label for tail"
        );
    }

    #[tokio::test]
    async fn test_render_working_memory_with_yesterday() {
        let store = setup_test_store().await;
        let config = test_config();

        let yesterday = store.yesterday();
        store
            .save_daily_summary(&yesterday, "Quiet day. Fixed auth bug. Reviewed 3 PRs.", 12)
            .await
            .unwrap();

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        assert!(
            rendered.contains("### Yesterday"),
            "should have yesterday section"
        );
        assert!(
            rendered.contains("auth bug"),
            "should contain yesterday content"
        );
    }

    #[tokio::test]
    async fn test_render_working_memory_respects_token_budget() {
        let store = setup_test_store().await;
        let today = store.today();
        let mut config = test_config();
        config.context_token_budget = 200; // Very tight budget.

        // Add many events to exceed the budget.
        for i in 0..50 {
            let event = WorkingMemoryEvent {
                id: Uuid::new_v4().to_string(),
                event_type: WorkingMemoryEventType::WorkerCompleted,
                timestamp: Utc::now(),
                channel_id: Some("chan-1".to_string()),
                user_id: None,
                summary: format!("Worker {i} completed with a long description of what was done"),
                detail: None,
                importance: 0.6,
                day: today.clone(),
            };
            insert_event(&store.pool, &event).await.unwrap();
        }

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        // Should not contain all 50 events — budget should cap it.
        let event_lines = rendered.lines().filter(|l| l.starts_with("- ")).count();
        assert!(
            event_lines < 50,
            "should be capped by token budget, got {event_lines}"
        );
    }

    #[tokio::test]
    async fn test_render_working_memory_disabled() {
        let store = setup_test_store().await;
        let mut config = test_config();
        config.enabled = false;

        let rendered = render_working_memory(&store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        assert!(rendered.is_empty(), "should be empty when disabled");
    }

    #[tokio::test]
    async fn test_render_channel_activity_map_empty() {
        let store = setup_test_store().await;
        let config = test_config();

        let rendered = render_channel_activity_map(&store.pool, &store, "chan-1", &config, Tz::UTC)
            .await
            .unwrap();

        // No channels in DB, so should be empty.
        assert!(rendered.is_empty(), "should be empty with no channels");
    }

    #[test]
    fn test_format_time_ago() {
        let now = Utc::now();
        assert_eq!(format_time_ago(now, now), "just now");
        assert_eq!(
            format_time_ago(now, now - chrono::Duration::minutes(5)),
            "5m ago"
        );
        assert_eq!(
            format_time_ago(now, now - chrono::Duration::hours(2)),
            "2h ago"
        );
        assert_eq!(
            format_time_ago(now, now - chrono::Duration::days(3)),
            "3d ago"
        );
    }

    #[test]
    fn test_format_event_line() {
        let event = WorkingMemoryEvent {
            id: "test".to_string(),
            event_type: WorkingMemoryEventType::WorkerCompleted,
            timestamp: Utc::now(),
            channel_id: Some("chan-1".to_string()),
            user_id: None,
            summary: "Built the project".to_string(),
            detail: None,
            importance: 0.6,
            day: "2026-03-18".to_string(),
        };

        // Same channel — no prefix.
        let line = format_event_line(&event, "chan-1");
        assert_eq!(line, "Worker completed: Built the project");

        // Different channel — prefix with channel ID.
        let line = format_event_line(&event, "chan-2");
        assert_eq!(line, "[chan-1] Worker completed: Built the project");
    }
}
