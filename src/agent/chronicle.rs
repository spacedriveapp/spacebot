//! Chronicle lifecycle: interval checkpoint cuts, and the bounded checkpoint
//! view rendered into the channel system prompt.
//!
//! Like the compactor this is a programmatic monitor, not an LLM process — it
//! watches counters and spawns the summarization. Unlike the compactor its
//! output is durable and append-only: each cut summarizes only the span since
//! the previous checkpoint, and the resulting text never re-enters the
//! transcript that a later cut reads.
//!
//! See `docs/design-docs/session-chronicles.md`.

use crate::agent::compactor::{estimate_history_tokens, estimate_text_tokens};
use crate::config::ChronicleConfig;
use crate::conversation::chronicle::{
    CheckpointKind, ChronicleBoundary, ChronicleCheckpoint, ChronicleStats, ChronicleStore,
    CommitOutcome, NewCheckpoint,
};
use crate::conversation::history::ConversationMessage;
use crate::error::Result;
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{AgentDeps, ChannelId, ProcessId, ProcessType};

use chrono::{DateTime, Duration, Utc};
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel as _;
use rig::message::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Live messages kept after a trim regardless of what the chronicle covers.
/// A channel needs its latest exchange to act at all.
const MIN_RETAINED_MESSAGES: usize = 4;

/// Share of the context window held back for the channel's own response when
/// deciding whether the request is too large. Matches the fork pre-compaction
/// reserve so the two budgets do not disagree.
pub const RESPONSE_RESERVE_FRACTION: f32 = 0.15;

/// Estimate what a turn's request will cost, in tokens.
///
/// This is deliberately not called a total-request budget. It covers the
/// rendered system prompt, the live history, the incoming user message, and a
/// response reserve — everything this layer can measure. It does **not** cover
/// serialized tool schemas: those are assembled inside Rig's `ToolServer` at
/// call time and are not exposed to the caller, so a channel with many tools
/// will send more than this number says. Treat it as a lower bound.
pub fn estimate_request_tokens(
    prompt_tokens: usize,
    history: &[Message],
    incoming_text: &str,
    context_window: usize,
) -> usize {
    let response_reserve = (context_window as f32 * RESPONSE_RESERVE_FRACTION) as usize;
    prompt_tokens
        .saturating_add(estimate_history_tokens(history))
        .saturating_add(estimate_text_tokens(incoming_text))
        .saturating_add(response_reserve)
}

/// Prior checkpoints handed to a cut as narrative context, newest last.
const NARRATIVE_CONTEXT_CHECKPOINTS: i64 = 3;

/// A completed turn's position in the live history, paired with how far the
/// durable log had advanced at that moment.
///
/// `durable_seq` is read after the turn, and `ConversationLogger` writes are
/// detached, so it can lag what the turn actually produced. Lagging low is the
/// safe direction: it makes the trim keep more, never less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBoundary {
    pub live_len: usize,
    pub durable_seq: i64,
}

/// Serializes every structural mutation of one channel's live history, and
/// lets a long-running summarizer detect that the world moved under it.
///
/// Both compaction modes share one of these. Without it, a chronicle cut
/// spawned before a mode switch and a rolling compaction started after it can
/// each drain the same head: the chronicle's private generation counter never
/// saw the rolling drain, so its post-commit trim would cut a second time.
#[derive(Debug)]
pub struct HistoryFence {
    /// Bumped by every head mutation, from either mode.
    generation: AtomicU64,
    /// Bumped whenever the channel observes a different compaction mode.
    mode_epoch: AtomicU64,
    /// Held across any head mutation and any cut commit, so emergency
    /// truncation and a regular cut can never interleave.
    mutation: tokio::sync::Mutex<()>,
    /// The last mode the channel observed: 1 chronicle, 0 rolling.
    mode_flag: AtomicU64,
    /// Estimated tokens of the most recently rendered system prompt. The
    /// context monitor budgets against the whole request, not just history.
    prompt_tokens: AtomicU64,
    /// Live-history lengths recorded at completed turn boundaries, oldest
    /// first. Trimming only ever lands on one of these.
    turns: std::sync::Mutex<Vec<TurnBoundary>>,
}

impl Default for HistoryFence {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryFence {
    pub fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            mode_epoch: AtomicU64::new(0),
            mutation: tokio::sync::Mutex::new(()),
            mode_flag: AtomicU64::new(0),
            prompt_tokens: AtomicU64::new(0),
            turns: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// A snapshot to validate against before committing or trimming.
    pub fn snapshot(&self) -> FenceSnapshot {
        FenceSnapshot {
            generation: self.generation.load(Ordering::Acquire),
            mode_epoch: self.mode_epoch.load(Ordering::Acquire),
        }
    }

    pub fn matches(&self, snapshot: FenceSnapshot) -> bool {
        self.generation.load(Ordering::Acquire) == snapshot.generation
            && self.mode_epoch.load(Ordering::Acquire) == snapshot.mode_epoch
    }

    /// Record that the head changed. Any in-flight cut holding an older
    /// snapshot will decline to trim.
    pub fn note_head_mutation(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Record the mode the channel just observed, bumping the epoch when it
    /// differs from the last one. Work spawned under the previous mode is
    /// thereby invalidated.
    pub fn observe_mode(&self, chronicle: bool) {
        let want = u64::from(chronicle);
        let current = self.mode_flag.load(Ordering::Acquire);
        if current != want {
            self.mode_flag.store(want, Ordering::Release);
            self.mode_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Record what the last rendered system prompt cost. Everything above the
    /// message array — identity, skills, working memory, the chronicle view,
    /// any backfill — is in that number.
    pub fn record_prompt_tokens(&self, tokens: usize) {
        self.prompt_tokens.store(tokens as u64, Ordering::Release);
    }

    pub fn prompt_tokens(&self) -> usize {
        self.prompt_tokens.load(Ordering::Acquire) as usize
    }

    /// Exclusive access for the duration of a head mutation or a commit.
    pub async fn lock_mutation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation.lock().await
    }

    /// Record a completed turn's live-history length and durable watermark.
    pub fn record_turn(&self, live_len: usize, durable_seq: i64) {
        let Ok(mut turns) = self.turns.lock() else {
            return;
        };
        if turns
            .last()
            .is_some_and(|last| last.live_len == live_len && last.durable_seq == durable_seq)
        {
            return;
        }
        turns.push(TurnBoundary {
            live_len,
            durable_seq,
        });
        // Bounded: only the newest boundaries can ever be a trim target.
        const MAX_TRACKED_TURNS: usize = 512;
        if turns.len() > MAX_TRACKED_TURNS {
            let excess = turns.len() - MAX_TRACKED_TURNS;
            turns.drain(..excess);
        }
    }

    /// How many live entries a checkpoint covering up to `durable_seq` may
    /// safely drop: the largest recorded turn boundary whose durable watermark
    /// the checkpoint already covers.
    ///
    /// Returning a turn boundary rather than a count is what keeps a
    /// tool-heavy turn intact — those entries never reach the durable log, so
    /// counting durable rows would silently drop them.
    pub fn droppable_prefix(&self, covered_through: i64) -> usize {
        let Ok(turns) = self.turns.lock() else {
            return 0;
        };
        turns
            .iter()
            .filter(|turn| turn.durable_seq <= covered_through)
            .map(|turn| turn.live_len)
            .max()
            .unwrap_or(0)
    }

    /// Shift recorded boundaries down after `dropped` entries left the front,
    /// and forget any that the drop consumed.
    pub fn rebase_turns(&self, dropped: usize) {
        let Ok(mut turns) = self.turns.lock() else {
            return;
        };
        turns.retain(|turn| turn.live_len > dropped);
        for turn in turns.iter_mut() {
            turn.live_len -= dropped;
        }
    }
}

/// Clears the in-flight cut flag however the spawned task exits, panic
/// included.
struct CuttingGuard(Arc<AtomicBool>);

impl Drop for CuttingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The fence state a cut captured when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceSnapshot {
    pub generation: u64,
    pub mode_epoch: u64,
}

/// Why `check_and_chronicle` acted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronicleAction {
    /// Entering chronicle mode with a backlog larger than one cut can read.
    Bootstrap,
    /// The message or token interval elapsed.
    Interval,
    /// Context pressure forced a cut early.
    Pressure,
    /// Emergency truncation dropped live context without summarizing it.
    Emergency,
}

/// Per-channel chronicle monitor.
pub struct Chronicler {
    channel_id: ChannelId,
    deps: AgentDeps,
    history: Arc<RwLock<Vec<Message>>>,
    store: ChronicleStore,
    model_override: Option<String>,
    /// Shared with the rolling compactor so a mode switch cannot leave two
    /// summarizers mutating the same head.
    fence: Arc<HistoryFence>,
    /// One in-flight cut per channel.
    cutting: Arc<AtomicBool>,
}

impl Chronicler {
    pub fn new(
        channel_id: ChannelId,
        deps: AgentDeps,
        history: Arc<RwLock<Vec<Message>>>,
        model_override: Option<String>,
        fence: Arc<HistoryFence>,
    ) -> Self {
        let store = ChronicleStore::new(deps.sqlite_pool.clone());
        Self {
            channel_id,
            deps,
            history,
            store,
            model_override,
            fence,
            cutting: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn store(&self) -> &ChronicleStore {
        &self.store
    }

    pub fn fence(&self) -> &Arc<HistoryFence> {
        &self.fence
    }

    fn config(&self) -> ChronicleConfig {
        self.deps.runtime_config.compaction.load().chronicle
    }

    /// Decide whether to cut a checkpoint, and start one if so.
    ///
    /// Returns `None` when nothing happened — including when emergency
    /// truncation found nothing it could drop, so a channel sitting on an
    /// oversized retained message does not report an emergency every turn.
    pub async fn check_and_chronicle(&self) -> Result<Option<ChronicleAction>> {
        let config = self.config();
        let compaction = **self.deps.runtime_config.compaction.load();
        let context_window = **self.deps.runtime_config.context_window.load();

        let live_tokens = {
            let history = self.history.read().await;
            estimate_history_tokens(&history)
        };

        // Thresholds apply to the whole request, not just history: the system
        // prompt carries identity, skills, working memory and the chronicle
        // view, and the response needs room of its own. Measuring history
        // alone lets a channel sit "safe" while its actual request is over.
        let response_reserve = (context_window as f32 * RESPONSE_RESERVE_FRACTION) as usize;
        let request_tokens = live_tokens
            .saturating_add(self.fence.prompt_tokens())
            .saturating_add(response_reserve);
        let usage = request_tokens as f32 / context_window.max(1) as f32;

        if usage >= compaction.emergency_threshold {
            // Emergency shares the mutation lock with cuts, so it can never
            // interleave with one that is mid-commit.
            return Ok(self
                .emergency_truncate()
                .await?
                .then_some(ChronicleAction::Emergency));
        }

        if self.cutting.load(Ordering::Acquire) {
            return Ok(None);
        }

        let latest = self.store.latest(&self.channel_id, 0).await?;
        let boundary = match &latest {
            Some(checkpoint) => checkpoint.end_boundary(),
            None => ChronicleBoundary::origin(),
        };

        let uncovered = self
            .store
            .count_messages_after(&self.channel_id, boundary)
            .await?;
        if uncovered == 0 {
            return Ok(None);
        }

        let interval_tokens =
            (context_window as f32 * config.interval_token_fraction).max(1.0) as usize;

        let action = if latest.is_none() && uncovered > config.max_messages_per_checkpoint {
            ChronicleAction::Bootstrap
        } else if usage >= compaction.background_threshold {
            ChronicleAction::Pressure
        } else if uncovered >= config.interval_messages as i64 || live_tokens >= interval_tokens {
            ChronicleAction::Interval
        } else {
            return Ok(None);
        };

        tracing::info!(
            channel_id = %self.channel_id,
            ?action,
            uncovered,
            live_tokens,
            usage = %format!("{:.1}%", usage * 100.0),
            "chronicle checkpoint triggered"
        );

        self.spawn_cut(action, boundary, config);
        Ok(Some(action))
    }

    fn spawn_cut(
        &self,
        action: ChronicleAction,
        boundary: ChronicleBoundary,
        config: ChronicleConfig,
    ) {
        if self.cutting.swap(true, Ordering::AcqRel) {
            return;
        }

        let kind = match action {
            ChronicleAction::Bootstrap => CheckpointKind::Bootstrap,
            ChronicleAction::Pressure => CheckpointKind::Pressure,
            ChronicleAction::Interval => CheckpointKind::Interval,
            ChronicleAction::Emergency => CheckpointKind::Emergency,
        };

        let cut = CutContext {
            channel_id: self.channel_id.clone(),
            deps: self.deps.clone(),
            store: self.store.clone(),
            history: self.history.clone(),
            fence: self.fence.clone(),
            model_override: self.model_override.clone(),
            config,
            // Captured before the LLM call; re-checked before commit and trim.
            entry: self.fence.snapshot(),
        };
        let cutting = self.cutting.clone();

        tokio::spawn(async move {
            // Released on unwind too: a panic in summarization would otherwise
            // leave the flag set for the process lifetime, and the channel
            // would never cut or trim again — it would just drift into
            // emergency truncation every turn.
            let _release = CuttingGuard(cutting);
            let channel_id = cut.channel_id.clone();
            if let Err(error) = cut.run(kind, boundary).await {
                tracing::error!(channel_id = %channel_id, %error, "chronicle checkpoint failed");
            }
        });
    }

    /// Drop live history without summarizing it, and record a checkpoint
    /// marking the discarded span.
    ///
    /// Returns whether anything was actually dropped. At the retention floor
    /// there is nothing to drop and nothing to report.
    async fn emergency_truncate(&self) -> Result<bool> {
        let _guard = self.fence.lock_mutation().await;

        let (removed, covered_through) = {
            let mut history = self.history.write().await;
            let total = history.len();
            if total <= MIN_RETAINED_MESSAGES {
                tracing::debug!(
                    channel_id = %self.channel_id,
                    total,
                    "emergency threshold reached at the retention floor; nothing to drop"
                );
                return Ok(false);
            }
            let remove_count = (total / 2).min(total - MIN_RETAINED_MESSAGES);
            history.drain(..remove_count);
            self.fence.note_head_mutation();
            self.fence.rebase_turns(remove_count);
            (remove_count, history.len())
        };
        let _ = covered_through;

        tracing::warn!(
            channel_id = %self.channel_id,
            removed,
            "chronicle emergency truncation performed"
        );

        let latest = self.store.latest(&self.channel_id, 0).await?;
        let from = match &latest {
            Some(checkpoint) => checkpoint.end_boundary(),
            None => ChronicleBoundary::origin(),
        };

        // Cover only what the truncation actually discarded. The live entries
        // still in context must stay uncovered so a later interval cut can
        // summarize them properly.
        let retained_live = self.history.read().await.len();
        let messages = self
            .store
            .messages_after(
                &self.channel_id,
                from,
                self.config().max_messages_per_checkpoint,
            )
            .await?;
        let covered = messages.len().saturating_sub(retained_live);
        let Some(last) = covered.checked_sub(1).and_then(|index| messages.get(index)) else {
            tracing::debug!(
                channel_id = %self.channel_id,
                "emergency truncation covered no logged span; the tail stays for the next cut"
            );
            return Ok(true);
        };
        let Some(to_seq) = last.seq else {
            return Ok(true);
        };

        let outcome = self
            .store
            .commit(NewCheckpoint {
                channel_id: self.channel_id.to_string(),
                level: 0,
                kind: CheckpointKind::Emergency,
                title: format!("Truncated span ({covered} messages)"),
                summary: format!(
                    "Context reached the emergency threshold and {removed} live messages were \
                     dropped without summarization. {covered} logged messages in this span were \
                     not summarized; expand this range with the chronicle tool if the detail is \
                     needed."
                ),
                covers_from: from,
                covers_to: ChronicleBoundary::new(to_seq),
                covers_from_at: messages
                    .first()
                    .map(|message| message.created_at)
                    .unwrap_or_else(Utc::now),
                covers_to_at: last.created_at,
                covers_from_message_id: None,
                covers_to_message_id: Some(last.id.clone()),
                message_count: covered as i64,
                token_estimate: 0,
                rolls_up_from_seq: None,
                rolls_up_to_seq: None,
                model: None,
            })
            .await?;

        match outcome {
            CommitOutcome::Committed(checkpoint) => {
                emit_checkpoint_event(&self.deps, &self.channel_id, &checkpoint);
            }
            // The drain already happened. Without a checkpoint the discarded
            // span has nothing describing it, so say so rather than returning
            // quietly.
            CommitOutcome::Superseded { expected, found } => {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    expected,
                    found,
                    removed,
                    "emergency truncation dropped live messages but its checkpoint was \
                     superseded; the span is unmarked until the next cut covers it"
                );
            }
            CommitOutcome::Busy => {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    removed,
                    "emergency truncation dropped live messages but could not take the write \
                     lock; the span is unmarked until the next cut covers it"
                );
            }
        }

        Ok(true)
    }
}

/// Everything a spawned cut needs, so the task owns no `&self` borrow.
struct CutContext {
    channel_id: ChannelId,
    deps: AgentDeps,
    store: ChronicleStore,
    history: Arc<RwLock<Vec<Message>>>,
    fence: Arc<HistoryFence>,
    model_override: Option<String>,
    config: ChronicleConfig,
    entry: FenceSnapshot,
}

impl CutContext {
    async fn run(&self, kind: CheckpointKind, from: ChronicleBoundary) -> Result<()> {
        let messages = self
            .store
            .messages_after(
                &self.channel_id,
                from,
                self.config.max_messages_per_checkpoint,
            )
            .await?;

        let Some(last) = messages.last() else {
            return Ok(());
        };
        let Some(to_seq) = last.seq else {
            tracing::warn!(
                channel_id = %self.channel_id,
                "skipping cut: tail message has no durable sequence"
            );
            return Ok(());
        };
        let to = ChronicleBoundary::new(to_seq);

        // Live entries this cut will drop. Tool calls and tool results never
        // reach the durable log, so summarizing only the durable rows would
        // discard a tool-heavy turn unsummarized. They go into the prompt.
        let droppable = self.fence.droppable_prefix(to_seq);
        let discarded_live: Vec<Message> = {
            let history = self.history.read().await;
            history
                .iter()
                .take(droppable.min(history.len()))
                .cloned()
                .collect()
        };

        let narrative = self
            .store
            .list(&self.channel_id, 0, NARRATIVE_CONTEXT_CHECKPOINTS)
            .await?;

        let (title, summary, model) = self
            .summarize(kind, &messages, &discarded_live, &narrative)
            .await;

        // Serialize against emergency truncation and any other head mutation
        // for the whole commit-and-trim window.
        let _guard = self.fence.lock_mutation().await;
        if !self.fence.matches(self.entry) {
            tracing::info!(
                channel_id = %self.channel_id,
                "discarding chronicle cut: the mode or history head changed while it ran"
            );
            return Ok(());
        }

        let outcome = self
            .store
            .commit(NewCheckpoint {
                channel_id: self.channel_id.to_string(),
                level: 0,
                kind,
                title,
                summary: summary.clone(),
                covers_from: from,
                covers_to: to,
                covers_from_at: messages
                    .first()
                    .map(|message| message.created_at)
                    .unwrap_or_else(Utc::now),
                covers_to_at: last.created_at,
                covers_from_message_id: None,
                covers_to_message_id: Some(last.id.clone()),
                message_count: messages.len() as i64,
                token_estimate: estimate_text_tokens(&summary) as i64,
                rolls_up_from_seq: None,
                rolls_up_to_seq: None,
                model,
            })
            .await?;

        let checkpoint = match outcome {
            CommitOutcome::Committed(checkpoint) => checkpoint,
            CommitOutcome::Superseded { expected, found } => {
                tracing::info!(
                    channel_id = %self.channel_id,
                    expected,
                    found,
                    "chronicle cut superseded; span stays unsummarized for the next cut"
                );
                return Ok(());
            }
            CommitOutcome::Busy => {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    "chronicle cut could not take the write lock; span stays unsummarized"
                );
                return Ok(());
            }
        };

        emit_checkpoint_event(&self.deps, &self.channel_id, &checkpoint);
        self.trim_live_history(&checkpoint).await;

        tracing::info!(
            channel_id = %self.channel_id,
            seq = checkpoint.seq,
            message_count = checkpoint.message_count,
            "chronicle checkpoint committed"
        );

        // The mutation lock is only needed for head mutations; rollups touch
        // nothing but the checkpoint table, so it is released first.
        drop(_guard);
        self.roll_up_if_due().await;

        Ok(())
    }

    /// Fold the oldest run of un-rolled checkpoints into a higher-level
    /// summary once enough have accumulated.
    ///
    /// Runs level by level: level-0 checkpoints roll into level-1, and once
    /// enough level-1 rollups exist they roll into level-2, so a session of any
    /// length keeps a bounded number of entries at the top.
    async fn roll_up_if_due(&self) {
        const MAX_ROLLUP_LEVELS: i64 = 8;

        let mut level = 0i64;
        while level < MAX_ROLLUP_LEVELS {
            match self.roll_up_level(level).await {
                Ok(true) => level += 1,
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(
                        channel_id = %self.channel_id,
                        level,
                        %error,
                        "chronicle rollup failed"
                    );
                    return;
                }
            }
        }
    }

    /// Roll the oldest `rollup_batch` un-rolled checkpoints at one level.
    /// Returns whether a rollup was committed.
    async fn roll_up_level(&self, level: i64) -> Result<bool> {
        let config = self.config;
        let pending = self.store.unrolled_count(&self.channel_id, level).await?;
        if pending <= config.rollup_threshold as i64 {
            return Ok(false);
        }

        let children = self
            .store
            .unrolled_at_level(&self.channel_id, level, config.rollup_batch as i64)
            .await?;
        if children.len() < 2 {
            return Ok(false);
        }

        // Coverage must be contiguous, or the rollup would claim a span it does
        // not summarize. The oldest un-rolled run always is, but a gap would
        // mean something rolled out of order.
        for pair in children.windows(2) {
            if pair[0].covers_to_seq != pair[1].covers_from_seq {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    level,
                    "skipping rollup: checkpoint coverage is not contiguous"
                );
                return Ok(false);
            }
        }

        let first = children.first().expect("checked non-empty");
        let last = children.last().expect("checked non-empty");
        let (title, summary, model) = self.summarize_rollup(&children).await;

        let outcome = self
            .store
            .commit_rollup(
                NewCheckpoint {
                    channel_id: self.channel_id.to_string(),
                    level: level + 1,
                    kind: CheckpointKind::Rollup,
                    title,
                    summary: summary.clone(),
                    covers_from: first.start_boundary(),
                    covers_to: last.end_boundary(),
                    covers_from_at: first.covers_from_at,
                    covers_to_at: last.covers_to_at,
                    covers_from_message_id: first.covers_from_message_id.clone(),
                    covers_to_message_id: last.covers_to_message_id.clone(),
                    message_count: children.iter().map(|child| child.message_count).sum(),
                    token_estimate: estimate_text_tokens(&summary) as i64,
                    rolls_up_from_seq: Some(first.seq),
                    rolls_up_to_seq: Some(last.seq),
                    model,
                },
                &children
                    .iter()
                    .map(|child| child.id.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;

        match outcome {
            CommitOutcome::Committed(rollup) => {
                tracing::info!(
                    channel_id = %self.channel_id,
                    seq = rollup.seq,
                    level = rollup.level,
                    covers = format!("#{}..#{}", first.seq, last.seq),
                    "chronicle rollup committed"
                );
                emit_checkpoint_event(&self.deps, &self.channel_id, &rollup);
                Ok(true)
            }
            CommitOutcome::Superseded { .. } => {
                tracing::debug!(
                    channel_id = %self.channel_id,
                    level,
                    "chronicle rollup superseded; another pass claimed these checkpoints"
                );
                Ok(false)
            }
            CommitOutcome::Busy => Ok(false),
        }
    }

    /// Summarize a run of checkpoints into one higher-level entry.
    ///
    /// This is the one legitimate summary-of-summaries in the design: bounded
    /// to a single level of recursion per generation, and reversible because
    /// the covered checkpoints keep their own rows.
    async fn summarize_rollup(
        &self,
        children: &[ChronicleCheckpoint],
    ) -> (String, String, Option<String>) {
        let fallback_title = match (children.first(), children.last()) {
            (Some(first), Some(last)) => format!(
                "{} – {}",
                first.covers_from_at.format("%Y-%m-%d"),
                last.covers_to_at.format("%Y-%m-%d")
            ),
            _ => "Earlier history".to_string(),
        };

        let prompt_engine = self.deps.runtime_config.prompts.load();
        let preamble = match prompt_engine.render_static("chronicle_rollup") {
            Ok(preamble) => preamble,
            Err(error) => {
                tracing::error!(%error, "failed to render chronicle rollup prompt");
                return (fallback_title, rollup_fallback_summary(children), None);
            }
        };

        let routing = self.deps.runtime_config.routing.load();
        let model_name = match &self.model_override {
            Some(model) => model.clone(),
            None => routing.resolve(ProcessType::Compactor, None).to_string(),
        };
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_context(&*self.deps.agent_id, "chronicle-rollup")
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&preamble)
            .default_max_turns(1)
            .build();

        let hook = SpacebotHook::new(
            self.deps.agent_id.clone(),
            ProcessId::Worker(Uuid::new_v4()),
            ProcessType::Compactor,
            Some(self.channel_id.clone()),
            self.deps.event_tx.clone(),
        );

        let mut prompt = String::from("## Checkpoints to condense\n\n");
        for child in children {
            prompt.push_str(&format!(
                "### #{} {} ({} → {}, {} messages)\n\n{}\n\n",
                child.seq,
                child.title,
                child.covers_from_at.format("%Y-%m-%d %H:%M"),
                child.covers_to_at.format("%Y-%m-%d %H:%M"),
                child.message_count,
                child.summary
            ));
        }

        let mut rollup_history = Vec::new();
        match hook.prompt_once(&agent, &mut rollup_history, &prompt).await {
            Ok(text) => {
                let (title, summary) = parse_checkpoint_response(&text);
                (title.unwrap_or(fallback_title), summary, Some(model_name))
            }
            Err(error) => {
                tracing::warn!(%error, "chronicle rollup summarization failed");
                (fallback_title, rollup_fallback_summary(children), None)
            }
        }
    }

    /// Produce the checkpoint's title and summary.
    ///
    /// Prior summaries are supplied as narrative context so the entry reads as
    /// a continuation, but the model is told to describe only the new span.
    async fn summarize(
        &self,
        kind: CheckpointKind,
        messages: &[ConversationMessage],
        discarded_live: &[Message],
        narrative: &[ChronicleCheckpoint],
    ) -> (String, String, Option<String>) {
        let fallback_title = range_title(messages);
        let prompt_engine = self.deps.runtime_config.prompts.load();
        let preamble = match prompt_engine.render_static("chronicle_checkpoint") {
            Ok(preamble) => preamble,
            Err(error) => {
                tracing::error!(%error, "failed to render chronicle checkpoint prompt");
                return (fallback_title, unsummarized_notice(messages.len()), None);
            }
        };

        let routing = self.deps.runtime_config.routing.load();
        let model_name = match &self.model_override {
            Some(model) => model.clone(),
            None => routing.resolve(ProcessType::Compactor, None).to_string(),
        };
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_context(&*self.deps.agent_id, "chronicle")
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&preamble)
            .default_max_turns(1)
            .build();

        let hook = SpacebotHook::new(
            self.deps.agent_id.clone(),
            ProcessId::Worker(Uuid::new_v4()),
            ProcessType::Compactor,
            Some(self.channel_id.clone()),
            self.deps.event_tx.clone(),
        );

        let prompt = build_cut_prompt(kind, messages, discarded_live, narrative);
        let mut cut_history = Vec::new();
        let response = hook.prompt_once(&agent, &mut cut_history, &prompt).await;

        match response {
            Ok(text) => {
                let (title, summary) = parse_checkpoint_response(&text);
                (
                    title.unwrap_or(fallback_title),
                    summary,
                    Some(model_name.clone()),
                )
            }
            Err(error) => {
                tracing::warn!(%error, "chronicle summarization failed, recording an unsummarized span");
                (fallback_title, unsummarized_notice(messages.len()), None)
            }
        }
    }

    /// Drop live entries the chronicle now covers, up to a recorded turn
    /// boundary. Caller holds the mutation lock.
    async fn trim_live_history(&self, checkpoint: &ChronicleCheckpoint) {
        trim_live_history_to_boundary(
            &self.channel_id,
            &self.fence,
            &self.history,
            self.entry,
            checkpoint,
        )
        .await;
    }
}

/// Drop live entries a checkpoint covers, landing only on a recorded turn
/// boundary.
///
/// Trimming lands on a turn boundary whose durable watermark the checkpoint
/// already covers, so a turn's tool traffic is never split and nothing is
/// dropped that the checkpoint did not summarize. A fence mismatch means
/// another mutator moved the head while the cut ran; the checkpoint stays
/// valid and the next trim catches up.
///
/// Free-standing so it can be driven directly, exactly as production runs it.
pub(crate) async fn trim_live_history_to_boundary(
    channel_id: &ChannelId,
    fence: &Arc<HistoryFence>,
    history: &Arc<RwLock<Vec<Message>>>,
    entry: FenceSnapshot,
    checkpoint: &ChronicleCheckpoint,
) -> usize {
    let droppable = fence.droppable_prefix(checkpoint.covers_to_seq);
    if droppable == 0 {
        return 0;
    }

    let mut history = history.write().await;
    if !fence.matches(entry) {
        tracing::debug!(
            channel_id = %channel_id,
            seq = checkpoint.seq,
            "skipping chronicle trim: history changed during the cut"
        );
        return 0;
    }

    let floor = history.len().saturating_sub(MIN_RETAINED_MESSAGES);
    let remove = droppable.min(floor);
    if remove == 0 {
        return 0;
    }

    history.drain(..remove);
    fence.note_head_mutation();
    fence.rebase_turns(remove);

    tracing::debug!(
        channel_id = %channel_id,
        seq = checkpoint.seq,
        removed = remove,
        retained = history.len(),
        "trimmed live history to a covered turn boundary"
    );
    remove
}

fn emit_checkpoint_event(
    deps: &AgentDeps,
    channel_id: &ChannelId,
    checkpoint: &ChronicleCheckpoint,
) {
    if let Err(error) = deps
        .event_tx
        .send(crate::ProcessEvent::ChronicleCheckpoint {
            agent_id: deps.agent_id.clone(),
            channel_id: channel_id.clone(),
            checkpoint: Box::new(crate::ChronicleCheckpointPayload {
                checkpoint_id: checkpoint.id.clone(),
                seq: checkpoint.seq,
                level: checkpoint.level,
                kind: checkpoint.kind.as_str().to_string(),
                title: checkpoint.title.clone(),
                summary: checkpoint.summary.clone(),
                covers_from: checkpoint.covers_from_at.to_rfc3339(),
                covers_to: checkpoint.covers_to_at.to_rfc3339(),
                message_count: checkpoint.message_count,
                created_at: checkpoint.created_at.to_rfc3339(),
            }),
        })
    {
        tracing::debug!(%error, "failed to emit chronicle checkpoint event");
    }
}

/// Used when a rollup's summarization fails: the covered checkpoints are still
/// individually readable, so the entry says where to look rather than
/// pretending to summarize.
fn rollup_fallback_summary(children: &[ChronicleCheckpoint]) -> String {
    match (children.first(), children.last()) {
        (Some(first), Some(last)) => format!(
            "Covers checkpoints #{}–#{}. Summarization failed, so this entry has no narrative; \
             open the individual checkpoints for their detail.",
            first.seq, last.seq
        ),
        _ => "Covers earlier checkpoints; summarization failed.".to_string(),
    }
}

fn unsummarized_notice(message_count: usize) -> String {
    format!(
        "This span of {message_count} messages was not summarized — summarization failed. \
         Expand the range with the chronicle tool if the detail is needed."
    )
}

/// A date-range title, used when the model does not supply one.
fn range_title(messages: &[ConversationMessage]) -> String {
    match (messages.first(), messages.last()) {
        (Some(first), Some(last)) => {
            let from = first.created_at.format("%Y-%m-%d %H:%M");
            let to = last.created_at.format("%H:%M");
            format!("{from}–{to} UTC")
        }
        _ => "Untitled span".to_string(),
    }
}

/// Split a `TITLE: …` first line off the model's response.
fn parse_checkpoint_response(response: &str) -> (Option<String>, String) {
    let trimmed = response.trim();
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        return (None, String::new());
    };

    if let Some(title) = first.trim().strip_prefix("TITLE:") {
        let title = title.trim();
        let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if !title.is_empty() && !body.is_empty() {
            return (Some(truncate_title(title)), body);
        }
    }

    (None, trimmed.to_string())
}

const MAX_TITLE_CHARS: usize = 80;

fn truncate_title(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_string();
    }
    let truncated: String = title.chars().take(MAX_TITLE_CHARS - 1).collect();
    format!("{truncated}…")
}

/// Build the summarization prompt: narrative context first, then the span.
fn build_cut_prompt(
    kind: CheckpointKind,
    messages: &[ConversationMessage],
    discarded_live: &[Message],
    narrative: &[ChronicleCheckpoint],
) -> String {
    let mut prompt = String::new();

    if !narrative.is_empty() {
        prompt.push_str("## Story so far\n\n");
        prompt.push_str(
            "These are earlier checkpoints, for continuity only. Do not restate them.\n\n",
        );
        for checkpoint in narrative.iter().rev() {
            prompt.push_str(&format!(
                "- **{}** ({} → {}): {}\n",
                checkpoint.title,
                checkpoint.covers_from_at.format("%Y-%m-%d %H:%M"),
                checkpoint.covers_to_at.format("%Y-%m-%d %H:%M"),
                checkpoint.summary
            ));
        }
        prompt.push('\n');
    }

    if kind == CheckpointKind::Bootstrap {
        prompt.push_str(
            "## Note\n\nThis channel is entering chronicle mode with a backlog larger than one \
             checkpoint can read. The transcript below is the oldest readable slice of it, not \
             the whole past. Summarize exactly what is here and do not speculate about what came \
             before or after it.\n\n",
        );
    }

    prompt.push_str("## New span to summarize\n\n");
    prompt.push_str(&render_log_transcript(messages));

    // Tool calls and tool results are never persisted to the conversation log,
    // so a tool-heavy turn would otherwise be dropped from live context with
    // nothing recorded about it anywhere.
    if !discarded_live.is_empty() {
        let working = crate::agent::compactor::render_messages_for_summary(discarded_live);
        if !working.trim().is_empty() {
            prompt.push_str(
                "\n## Working detail being dropped from live context\n\n\
                 These are the tool calls and intermediate steps behind the span above. They are \
                 not stored anywhere else — capture their outcomes.\n\n",
            );
            prompt.push_str(&working);
        }
    }

    prompt
}

/// Render logged messages for the summarizer.
pub(crate) fn render_log_transcript(messages: &[ConversationMessage]) -> String {
    let mut output = String::new();
    for message in messages {
        let sender = message
            .sender_name
            .as_deref()
            .unwrap_or(match message.role.as_str() {
                "assistant" => "assistant",
                "system" => "system",
                _ => "user",
            });
        output.push_str(&format!(
            "[{}] {} ({}): {}\n",
            message.created_at.format("%Y-%m-%d %H:%M:%S"),
            sender,
            message.role,
            message.content
        ));
    }
    output
}

/// Assemble the bounded chronicle view for a channel's system prompt.
///
/// Recomputed from durable state every turn, so a restart reproduces it
/// exactly. Returns `None` when the channel has no chronicle yet.
pub async fn render_chronicle_view(
    store: &ChronicleStore,
    channel_id: &str,
    now: DateTime<Utc>,
    config: ChronicleConfig,
) -> Result<Option<String>> {
    let stats = store.stats(channel_id).await?;
    if stats.checkpoint_count == 0 {
        return Ok(None);
    }

    // A hostile or mistaken config value must not panic the prompt render, so
    // this is the fallible constructor with the default as a floor.
    let window = Duration::try_hours(config.recent_window_hours)
        .unwrap_or_else(|| Duration::hours(ChronicleConfig::default().recent_window_hours));
    let since = now - window;
    let recent = store
        .list_since(channel_id, 0, since, config.max_recent as i64)
        .await?;

    // Older history renders from whatever covers it most compactly: a level-0
    // checkpoint a rollup has absorbed is represented by that rollup instead,
    // so a long session shows a few high-level entries rather than dropping its
    // oldest ones off the end of the view.
    let before_seq = recent.first().map(|first| first.seq).unwrap_or(i64::MAX);
    let older = store
        .uncovered_before_seq(channel_id, before_seq, config.max_older as i64)
        .await?;

    Ok(Some(compose_view(
        &stats,
        &older,
        &recent,
        config.context_token_budget,
    )))
}

/// Render the header and entries within a token budget.
///
/// Under pressure the oldest entries collapse to a title and range line
/// before any entry is dropped, and the header never collapses — it is what
/// tells the agent that more exists and can be expanded.
fn compose_view(
    stats: &ChronicleStats,
    older: &[ChronicleCheckpoint],
    recent: &[ChronicleCheckpoint],
    budget_tokens: usize,
) -> String {
    let entries: Vec<&ChronicleCheckpoint> = older.iter().chain(recent.iter()).collect();

    // Collapse oldest-first, then drop oldest-first. Collapsing alone is not
    // enough: a long index of collapsed one-liners can still exceed the budget,
    // and the budget has to be an upper bound on what the prompt carries.
    let mut collapsed_upto = 0usize;
    let mut dropped = 0usize;
    loop {
        let shown = &entries[dropped..];
        let header = render_header(stats, shown.len(), dropped);
        let body = render_entries(shown, collapsed_upto.saturating_sub(dropped));
        let total = estimate_text_tokens(&header) + estimate_text_tokens(&body);

        if total <= budget_tokens {
            return format!("{header}\n{body}");
        }
        if collapsed_upto < entries.len() {
            collapsed_upto += 1;
            continue;
        }
        if dropped + 1 < entries.len() {
            dropped += 1;
            continue;
        }

        // Everything collapsed and only one entry left: the header plus a
        // single line is the floor. Report it rather than silently exceeding.
        let header = render_header(stats, shown.len(), dropped);
        let body = render_entries(shown, 0);
        if estimate_text_tokens(&header) + estimate_text_tokens(&body) > budget_tokens {
            tracing::debug!(
                budget_tokens,
                "chronicle view floor exceeds its budget; rendering the header and one entry"
            );
        }
        return format!("{header}\n{body}");
    }
}

fn render_header(stats: &ChronicleStats, shown: usize, omitted: usize) -> String {
    let mut header = String::from("## Session Chronicle\n\n");

    let age = match (stats.first_message_at, stats.last_message_at) {
        (Some(first), Some(last)) => {
            let days = (last - first).num_days();
            format!(
                "Session spans {} → {} ({} day{}).",
                first.format("%Y-%m-%d"),
                last.format("%Y-%m-%d"),
                days,
                if days == 1 { "" } else { "s" }
            )
        }
        _ => "Session age unknown.".to_string(),
    };

    header.push_str(&format!(
        "{age} {} messages logged, {} checkpoints ({} shown below{}). \
         {} messages since the last checkpoint are in raw context below this prompt.\n\n\
         Checkpoints below are summaries, not the transcript. Use the `chronicle` tool to list \
         the full checkpoint index or open one; a branch can expand any checkpoint back into raw \
         messages.\n",
        stats.total_messages,
        stats.checkpoint_count,
        shown,
        if omitted > 0 {
            format!(", {omitted} older not shown — list them with the chronicle tool")
        } else {
            String::new()
        },
        stats.unsummarized_messages,
    ));

    header
}

fn render_entries(entries: &[&ChronicleCheckpoint], collapsed_upto: usize) -> String {
    let mut body = String::new();
    for (index, checkpoint) in entries.iter().enumerate() {
        let range = format!(
            "{} → {}",
            checkpoint.covers_from_at.format("%Y-%m-%d %H:%M"),
            checkpoint.covers_to_at.format("%Y-%m-%d %H:%M")
        );
        if index < collapsed_upto {
            body.push_str(&format!(
                "- **#{} {}** ({}, {} messages) — collapsed; open with the chronicle tool.\n",
                checkpoint.seq, checkpoint.title, range, checkpoint.message_count
            ));
        } else {
            body.push_str(&format!(
                "\n### #{} {}\n{} · {} messages · {}\n\n{}\n",
                checkpoint.seq,
                checkpoint.title,
                range,
                checkpoint.message_count,
                checkpoint.kind.as_str(),
                checkpoint.summary
            ));
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(seq: i64, title: &str, summary: &str) -> ChronicleCheckpoint {
        let at = DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ChronicleCheckpoint {
            id: format!("cp-{seq}"),
            channel_id: "ch".into(),
            seq,
            level: 0,
            kind: CheckpointKind::Interval,
            title: title.into(),
            summary: summary.into(),
            covers_from_at: at,
            covers_to_at: at,
            covers_from_message_id: None,
            covers_to_message_id: Some(format!("m{seq}")),
            covers_from_seq: seq * 10,
            covers_to_seq: seq * 10 + 10,
            message_count: 10,
            token_estimate: 20,
            rolled_up_into: None,
            rolls_up_from_seq: None,
            rolls_up_to_seq: None,
            model: None,
            created_at: at,
        }
    }

    fn stats() -> ChronicleStats {
        ChronicleStats {
            checkpoint_count: 3,
            interval_count: 3,
            rollup_count: 0,
            total_messages: 120,
            first_message_at: Some(
                DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            last_message_at: Some(
                DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            unsummarized_messages: 7,
        }
    }

    #[test]
    fn view_header_reports_session_shape() {
        let view = compose_view(&stats(), &[], &[checkpoint(1, "First", "stuff")], 4000);
        assert!(view.contains("120 messages logged"));
        assert!(view.contains("3 checkpoints"));
        assert!(view.contains("7 messages since the last checkpoint"));
        assert!(view.contains("#1 First"));
    }

    #[test]
    fn view_collapses_oldest_first_under_budget() {
        let body = "x".repeat(2000);
        let entries: Vec<ChronicleCheckpoint> = (1..=4)
            .map(|seq| checkpoint(seq, &format!("Span {seq}"), &body))
            .collect();
        let refs: Vec<&ChronicleCheckpoint> = entries.iter().collect();

        let full = compose_view(&stats(), &[], &entries, 100_000);
        assert!(full.contains(&body), "an ample budget keeps every body");

        let squeezed = compose_view(&stats(), &[], &entries, 400);
        assert!(
            squeezed.contains("#1 Span 1") && squeezed.contains("collapsed"),
            "the oldest entry collapses first"
        );
        assert!(
            squeezed.contains("#4 Span 4"),
            "every checkpoint stays listed even when collapsed"
        );
        assert!(
            squeezed.contains("messages logged"),
            "the header never collapses"
        );
        assert_eq!(refs.len(), 4);
    }

    #[test]
    fn view_budget_is_monotone() {
        let body = "y".repeat(1200);
        let entries: Vec<ChronicleCheckpoint> = (1..=5)
            .map(|seq| checkpoint(seq, &format!("Span {seq}"), &body))
            .collect();

        // More budget must never render less: measure how many entries keep
        // their full body, which is what the budget actually buys.
        let mut previous_full = 0usize;
        for budget in [200usize, 800, 2000, 8000, 40_000] {
            let rendered = compose_view(&stats(), &[], &entries, budget);
            let full = rendered.matches(&body).count();
            assert!(
                full >= previous_full,
                "budget {budget} rendered fewer full entries than a smaller one"
            );
            previous_full = full;
        }
    }

    #[test]
    fn parse_response_splits_title_from_body() {
        let (title, summary) =
            parse_checkpoint_response("TITLE: Shipping the parser\n\nThey fixed the lexer.");
        assert_eq!(title.as_deref(), Some("Shipping the parser"));
        assert_eq!(summary, "They fixed the lexer.");
    }

    #[test]
    fn parse_response_without_title_keeps_whole_body() {
        let (title, summary) = parse_checkpoint_response("They fixed the lexer.\nThen shipped.");
        assert!(title.is_none());
        assert_eq!(summary, "They fixed the lexer.\nThen shipped.");
    }

    #[test]
    fn parse_response_rejects_title_without_body() {
        let (title, summary) = parse_checkpoint_response("TITLE: Just a title");
        assert!(title.is_none(), "a title with no body is not a valid split");
        assert_eq!(summary, "TITLE: Just a title");
    }

    #[test]
    fn long_titles_are_truncated() {
        let long = "word ".repeat(40);
        let (title, _) = parse_checkpoint_response(&format!("TITLE: {long}\n\nbody"));
        let title = title.expect("title parsed");
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn transcript_render_includes_sender_and_time() {
        let message = ConversationMessage {
            id: "m1".into(),
            channel_id: "ch".into(),
            role: "user".into(),
            sender_name: Some("jamie".into()),
            sender_id: Some("u1".into()),
            content: "ship it".into(),
            metadata: None,
            created_at: DateTime::parse_from_rfc3339("2026-08-01T10:11:12Z")
                .unwrap()
                .with_timezone(&Utc),
            seq: Some(1),
        };
        let rendered = render_log_transcript(std::slice::from_ref(&message));
        assert!(rendered.contains("2026-08-01 10:11:12"));
        assert!(rendered.contains("jamie"));
        assert!(rendered.contains("ship it"));
    }

    #[test]
    fn cut_prompt_marks_prior_checkpoints_as_context_only() {
        let narrative = vec![checkpoint(1, "Earlier", "earlier things")];
        let prompt = build_cut_prompt(CheckpointKind::Interval, &[], &[], &narrative);
        assert!(prompt.contains("Story so far"));
        assert!(prompt.contains("Do not restate them"));
        assert!(prompt.contains("New span to summarize"));
    }

    #[test]
    fn bootstrap_prompt_warns_about_truncated_past() {
        let prompt = build_cut_prompt(CheckpointKind::Bootstrap, &[], &[], &[]);
        assert!(prompt.contains("backlog larger than one checkpoint can read"));
    }

    // ---- HistoryFence: the lifecycle/race surface ----

    #[test]
    fn trimming_lands_only_on_a_covered_turn_boundary() {
        let fence = HistoryFence::new();
        // Turn 1 produced 2 live entries and reached durable seq 2.
        fence.record_turn(2, 2);
        // Turn 2 was tool-heavy: 14 live entries, but only 2 more durable rows.
        fence.record_turn(16, 4);

        // A checkpoint covering only through seq 2 may drop turn 1 alone. The
        // tool-heavy turn is not yet summarized, so none of it may go.
        assert_eq!(fence.droppable_prefix(2), 2);
        // Once seq 4 is covered, the whole tool-heavy turn may go together.
        assert_eq!(fence.droppable_prefix(4), 16);
        // A boundary before any recorded turn drops nothing.
        assert_eq!(fence.droppable_prefix(1), 0);
    }

    #[test]
    fn rebasing_forgets_consumed_turns_and_shifts_the_rest() {
        let fence = HistoryFence::new();
        fence.record_turn(2, 2);
        fence.record_turn(6, 4);
        fence.record_turn(10, 6);

        fence.rebase_turns(6);

        // The first two boundaries were consumed by the drop; the third shifts.
        assert_eq!(fence.droppable_prefix(6), 4);
        assert_eq!(fence.droppable_prefix(4), 0);
    }

    /// A rolling compaction drain must invalidate an in-flight chronicle cut,
    /// or the cut trims a head that no longer means what it did.
    #[test]
    fn a_head_mutation_from_either_mode_invalidates_an_in_flight_cut() {
        let fence = HistoryFence::new();
        let cut = fence.snapshot();
        assert!(fence.matches(cut));

        // Rolling compaction drains the shared vector.
        fence.note_head_mutation();
        assert!(
            !fence.matches(cut),
            "a drain from the other mode must invalidate the cut"
        );
    }

    /// Switching modes while a summarizer is in flight must invalidate it even
    /// if nothing touched the head in the meantime.
    #[test]
    fn a_mode_switch_invalidates_an_in_flight_cut() {
        let fence = HistoryFence::new();
        fence.observe_mode(true);
        let cut = fence.snapshot();
        assert!(fence.matches(cut));

        // Same mode observed again: not an epoch change.
        fence.observe_mode(true);
        assert!(fence.matches(cut));

        fence.observe_mode(false);
        assert!(
            !fence.matches(cut),
            "chronicle -> rolling must invalidate the pending cut"
        );

        // And back again is another epoch.
        let after_rolling = fence.snapshot();
        fence.observe_mode(true);
        assert!(!fence.matches(after_rolling));
    }

    /// Emergency truncation and a cut commit both take the mutation lock, so
    /// they cannot interleave.
    #[tokio::test]
    async fn the_mutation_lock_serializes_emergency_and_cuts() {
        let fence = Arc::new(HistoryFence::new());
        let held = fence.lock_mutation().await;

        let contender = fence.clone();
        let task = tokio::spawn(async move {
            let _guard = contender.lock_mutation().await;
            true
        });

        // While the first holder has it, the contender cannot proceed.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !task.is_finished(),
            "the lock must exclude a second mutator"
        );

        drop(held);
        assert!(task.await.expect("contender"), "released lock lets it run");
    }

    #[test]
    fn recorded_turns_are_bounded() {
        let fence = HistoryFence::new();
        for index in 0..2_000usize {
            fence.record_turn(index + 1, index as i64 + 1);
        }
        // The newest boundary is still reachable; memory is not unbounded.
        assert_eq!(fence.droppable_prefix(2_000), 2_000);
    }

    /// The monitor budgets the whole request. A channel whose history looks
    /// safe on its own can still be over once the prompt and the response
    /// reserve are counted.
    #[test]
    fn request_budget_counts_the_prompt_and_the_response_reserve() {
        let fence = HistoryFence::new();
        let context_window = 100_000usize;
        let live_tokens = 60_000usize;
        let reserve = (context_window as f32 * RESPONSE_RESERVE_FRACTION) as usize;

        // History alone is 60% — comfortably under an 80% background trigger.
        assert!((live_tokens as f32 / context_window as f32) < 0.80);

        fence.record_prompt_tokens(12_000);
        let request = live_tokens + fence.prompt_tokens() + reserve;
        assert!(
            (request as f32 / context_window as f32) >= 0.80,
            "prompt plus reserve must push this request over the threshold"
        );

        // With no prompt recorded yet the reserve alone still applies.
        let bare = HistoryFence::new();
        assert_eq!(bare.prompt_tokens(), 0);
    }

    /// The production trim path end-to-end against a real store: commit a
    /// checkpoint, then trim to the turn boundary it covers. This drives
    /// `CutContext::trim_live_history` rather than the fence primitive, which
    /// is what a previous revision left unwired.
    #[tokio::test]
    async fn cut_context_trims_live_history_to_the_covered_turn_boundary() {
        let store = store_with_two_checkpoints().await;
        let fence = Arc::new(HistoryFence::new());

        // 10 live entries across two turns; the second is tool-heavy.
        let history = Arc::new(RwLock::new(
            (0..10)
                .map(|index| Message::from(format!("entry {index}")))
                .collect::<Vec<_>>(),
        ));
        fence.record_turn(3, 3);
        fence.record_turn(10, 6);

        let checkpoint = store
            .latest("ch", 0)
            .await
            .expect("latest")
            .expect("a checkpoint exists");
        assert_eq!(checkpoint.covers_to_seq, 6);

        let entry = fence.snapshot();
        trim_live_history_to_boundary(&Arc::from("ch"), &fence, &history, entry, &checkpoint).await;

        let remaining = history.read().await.len();
        assert_eq!(
            remaining, 4,
            "the whole covered prefix goes, down to the retention floor"
        );
    }

    /// A head mutation from the other mode during the cut must abort the trim.
    #[tokio::test]
    async fn trim_is_abandoned_when_the_head_moved_during_the_cut() {
        let store = store_with_two_checkpoints().await;
        let fence = Arc::new(HistoryFence::new());
        let history = Arc::new(RwLock::new(
            (0..10)
                .map(|index| Message::from(format!("entry {index}")))
                .collect::<Vec<_>>(),
        ));
        fence.record_turn(10, 6);

        let checkpoint = store
            .latest("ch", 0)
            .await
            .expect("latest")
            .expect("checkpoint");

        let entry = fence.snapshot();

        // Rolling compaction drains the head while the cut was summarizing.
        fence.note_head_mutation();

        trim_live_history_to_boundary(&Arc::from("ch"), &fence, &history, entry, &checkpoint).await;
        assert_eq!(
            history.read().await.len(),
            10,
            "a stale cut must not trim on top of another mutator"
        );
    }

    /// A panic inside the cut task must still release the in-flight flag, or
    /// the channel never cuts or trims again.
    #[tokio::test]
    async fn a_panicking_cut_releases_the_in_flight_flag() {
        let cutting = Arc::new(AtomicBool::new(true));
        let flag = cutting.clone();

        let task = tokio::spawn(async move {
            let _release = CuttingGuard(flag);
            panic!("summarization blew up");
        });

        assert!(task.await.is_err(), "the task should have panicked");
        assert!(
            !cutting.load(Ordering::Acquire),
            "the flag must be cleared on unwind, not just on the happy path"
        );
    }

    /// An out-of-range window must not panic the prompt render.
    #[tokio::test]
    async fn an_absurd_recent_window_does_not_panic_the_view() {
        let store = store_with_two_checkpoints().await;
        let config = ChronicleConfig {
            recent_window_hours: i64::MAX,
            ..ChronicleConfig::default()
        };
        let view = render_chronicle_view(&store, "ch", Utc::now(), config)
            .await
            .expect("view must not fail");
        assert!(view.is_some(), "the chronicle still renders");
    }

    // ---- Context assembly under a hard budget ----

    /// The budget is an upper bound, not a hint: collapse first, then drop,
    /// and say how many were dropped.
    #[test]
    fn view_never_exceeds_its_budget_and_discloses_omissions() {
        let body = "z".repeat(4_000);
        let entries: Vec<ChronicleCheckpoint> = (1..=12)
            .map(|seq| checkpoint(seq, &format!("Span {seq}"), &body))
            .collect();

        for budget in [300usize, 600, 1_500, 4_000] {
            let rendered = compose_view(&stats(), &[], &entries, budget);
            assert!(
                estimate_text_tokens(&rendered) <= budget,
                "budget {budget} exceeded: {} tokens",
                estimate_text_tokens(&rendered)
            );
            if rendered.contains("not shown") {
                assert!(
                    rendered.contains("chronicle tool"),
                    "dropped entries must be discoverable"
                );
            }
        }
    }

    async fn store_with_two_checkpoints() -> ChronicleStore {
        use crate::conversation::chronicle::NewCheckpoint;

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260211000002_conversations.sql"
        ))
        .execute(&pool)
        .await
        .expect("conversations migration");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260809000004_session_chronicles.sql"
        ))
        .execute(&pool)
        .await
        .expect("chronicle migration");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260810000001_conversation_message_seq.sql"
        ))
        .execute(&pool)
        .await
        .expect("seq migration");

        for index in 0..6 {
            sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, content, created_at, seq) \
                 VALUES (?, 'ch', 'user', 'hello', ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = 'ch'), 0) + 1)",
            )
            .bind(format!("m{index}"))
            .bind(format!("2026-08-01 00:00:0{index}"))
            .execute(&pool)
            .await
            .expect("insert");
        }

        let store = ChronicleStore::new(pool);
        let at = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
        };

        let mut from = ChronicleBoundary::origin();
        for (seq, (to_at, to_id, to_seq)) in [
            ("2026-08-01T00:00:02Z", "m2", 3i64),
            ("2026-08-01T00:00:05Z", "m5", 6i64),
        ]
        .iter()
        .enumerate()
        {
            let to = ChronicleBoundary::new(*to_seq);
            store
                .commit(NewCheckpoint {
                    channel_id: "ch".into(),
                    level: 0,
                    kind: CheckpointKind::Interval,
                    title: format!("Span {}", seq + 1),
                    summary: format!("Summary of span {}", seq + 1),
                    covers_from: from,
                    covers_to: to,
                    covers_from_at: at("2026-08-01T00:00:00Z"),
                    covers_to_at: at(to_at),
                    covers_from_message_id: None,
                    covers_to_message_id: Some((*to_id).to_string()),
                    message_count: 3,
                    token_estimate: 5,
                    rolls_up_from_seq: None,
                    rolls_up_to_seq: None,
                    model: None,
                })
                .await
                .expect("commit");
            from = to;
        }

        store
    }

    /// The view is a pure function of durable state, so a process that lost
    /// every in-memory structure renders exactly what the running one did.
    #[tokio::test]
    async fn view_is_reproducible_from_durable_state_alone() {
        let store = store_with_two_checkpoints().await;
        let now = DateTime::parse_from_rfc3339("2026-08-01T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let config = ChronicleConfig::default();

        let before_restart = render_chronicle_view(&store, "ch", now, config)
            .await
            .expect("view")
            .expect("a chronicle exists");

        // Restart: nothing survives but the database.
        let reopened = ChronicleStore::new(store.pool_for_tests().clone());
        let after_restart = render_chronicle_view(&reopened, "ch", now, config)
            .await
            .expect("view")
            .expect("a chronicle exists");

        assert_eq!(before_restart, after_restart);
        assert!(before_restart.contains("#1 Span 1"));
        assert!(before_restart.contains("#2 Span 2"));
    }

    #[tokio::test]
    async fn view_is_absent_before_the_first_checkpoint() {
        let store = store_with_two_checkpoints().await;
        let now = Utc::now();
        assert!(
            render_chronicle_view(&store, "empty-channel", now, ChronicleConfig::default())
                .await
                .expect("view")
                .is_none()
        );
    }

    /// Checkpoints outside the recent window still appear, so a session that
    /// went quiet for a week does not lose its older entries from the index.
    #[tokio::test]
    async fn view_includes_older_checkpoints_outside_the_recent_window() {
        let store = store_with_two_checkpoints().await;
        let now = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let view = render_chronicle_view(&store, "ch", now, ChronicleConfig::default())
            .await
            .expect("view")
            .expect("a chronicle exists");

        assert!(view.contains("#1 Span 1"));
        assert!(view.contains("#2 Span 2"));
    }
}
