//! Cron scheduler: timer management and execution.
//!
//! Each cron job gets its own tokio task that fires on an interval.
//! When a job fires, it creates a fresh short-lived channel,
//! runs the job's prompt through the LLM, and delivers the result
//! to the delivery target via the messaging system.

use crate::agent::channel::Channel;
use crate::cron::store::CronStore;
use crate::error::Result;
use crate::messaging::MessagingManager;
use crate::messaging::target::{BroadcastTarget, parse_delivery_target};
use crate::{AgentDeps, InboundMessage, MessageContent, OutboundResponse};
use chrono::Timelike;
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

/// A cron job definition loaded from the database.
#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    pub prompt: String,
    /// Optional wall-clock cron expression (5-field syntax).
    pub cron_expr: Option<String>,
    pub interval_secs: u64,
    pub delivery_target: BroadcastTarget,
    pub active_hours: Option<(u8, u8)>,
    pub enabled: bool,
    pub run_once: bool,
    pub consecutive_failures: u32,
    /// Maximum wall-clock seconds to wait for the job to complete.
    /// `None` uses the default of 120 seconds.
    pub timeout_secs: Option<u64>,
}

/// Serializable cron job config (for storage and TOML parsing).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CronConfig {
    pub id: String,
    pub prompt: String,
    /// Optional wall-clock cron expression (5-field syntax).
    pub cron_expr: Option<String>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// Delivery target in "adapter:target" format (e.g. "discord:123456789").
    pub delivery_target: String,
    pub active_hours: Option<(u8, u8)>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub run_once: bool,
    /// Maximum wall-clock seconds to wait for the job to complete.
    /// `None` uses the default of 120 seconds.
    pub timeout_secs: Option<u64>,
}

fn default_interval() -> u64 {
    3600
}

fn default_true() -> bool {
    true
}

/// Context needed to execute a cron job (agent resources + messaging).
///
/// Prompts, identity, browser config, and skills are read from
/// `deps.runtime_config` on each job firing so changes propagate
/// without restarting the scheduler.
#[derive(Clone)]
pub struct CronContext {
    pub deps: AgentDeps,
    pub screenshot_dir: std::path::PathBuf,
    pub logs_dir: std::path::PathBuf,
    pub messaging_manager: Arc<MessagingManager>,
    pub store: Arc<CronStore>,
}

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// RAII guard that clears an `AtomicBool` on drop, ensuring the flag is
/// released even if the holding task panics.
struct ExecutionGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
const SYSTEM_TIMEZONE_LABEL: &str = "system";

/// Scheduler that manages cron job timers and execution.
pub struct Scheduler {
    jobs: Arc<RwLock<HashMap<String, CronJob>>>,
    timers: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    context: CronContext,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(context: CronContext) -> Self {
        let tz_label = cron_timezone_label(&context);
        if tz_label == SYSTEM_TIMEZONE_LABEL {
            tracing::warn!(
                agent_id = %context.deps.agent_id,
                "no cron_timezone configured; active_hours will use the host system's local time, \
                 which is often UTC in Docker/containerized environments — set [defaults] \
                 cron_timezone to an IANA timezone like \"America/New_York\" if jobs are \
                 skipping their active window"
            );
        }

        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            timers: Arc::new(RwLock::new(HashMap::new())),
            context,
        }
    }

    pub fn cron_timezone_label(&self) -> String {
        cron_timezone_label(&self.context)
    }

    /// Register and start a cron job from config.
    pub async fn register(&self, config: CronConfig) -> Result<()> {
        let delivery_target = parse_delivery_target(&config.delivery_target).ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!(
                "invalid delivery target '{}': expected format 'adapter:target'",
                config.delivery_target
            ))
        })?;

        let cron_expr = normalize_cron_expr(config.cron_expr.clone())?;
        let job = CronJob {
            id: config.id.clone(),
            prompt: config.prompt,
            cron_expr,
            interval_secs: config.interval_secs,
            delivery_target,
            active_hours: normalize_active_hours(config.active_hours),
            enabled: config.enabled,
            run_once: config.run_once,
            consecutive_failures: 0,
            timeout_secs: config.timeout_secs,
        };

        {
            let mut jobs = self.jobs.write().await;
            jobs.insert(config.id.clone(), job);
        }

        if config.enabled {
            self.start_timer(&config.id).await;
        }

        tracing::info!(
            cron_id = %config.id,
            interval_secs = config.interval_secs,
            cron_expr = ?config.cron_expr,
            run_once = config.run_once,
            "cron job registered"
        );
        Ok(())
    }

    /// Start a timer loop for a cron job.
    ///
    /// Idempotent: if a timer is already running for this job, it is aborted before
    /// starting a new one. This prevents timer leaks when a job is re-registered via API.
    async fn start_timer(&self, job_id: &str) {
        let job_id_for_map = job_id.to_string();
        let job_id = job_id.to_string();
        let jobs = self.jobs.clone();
        let context = self.context.clone();

        // Abort any existing timer for this job before starting a new one.
        // Dropping a JoinHandle only detaches it — we must abort explicitly.
        {
            let mut timers = self.timers.write().await;
            if let Some(old_handle) = timers.remove(&job_id) {
                old_handle.abort();
                tracing::debug!(cron_id = %job_id, "aborted existing timer before re-registering");
            }
        }

        let handle = tokio::spawn(async move {
            let execution_lock = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut interval_first_tick = true;

            loop {
                let job = {
                    let j = jobs.read().await;
                    match j.get(&job_id) {
                        Some(j) if !j.enabled => {
                            tracing::debug!(cron_id = %job_id, "cron job disabled, stopping timer");
                            break;
                        }
                        Some(j) => j.clone(),
                        None => {
                            tracing::debug!(cron_id = %job_id, "cron job removed, stopping timer");
                            break;
                        }
                    }
                };

                let sleep_duration = if let Some(cron_expr) = job.cron_expr.as_deref() {
                    match next_fire_duration(&context, &job_id, cron_expr) {
                        Some((duration, next_fire_utc, timezone)) => {
                            tracing::debug!(
                                cron_id = %job_id,
                                cron_expr,
                                cron_timezone = %timezone,
                                next_fire_utc = %next_fire_utc.to_rfc3339(),
                                sleep_secs = duration.as_secs(),
                                "wall-clock cron next fire computed"
                            );
                            duration
                        }
                        None => {
                            tracing::warn!(
                                cron_id = %job_id,
                                cron_expr,
                                "failed to compute next wall-clock fire; retrying in 60s"
                            );
                            Duration::from_secs(60)
                        }
                    }
                } else {
                    let interval_secs = job.interval_secs;
                    let delay = if interval_first_tick {
                        interval_first_tick = false;
                        interval_initial_delay(interval_secs)
                    } else {
                        Duration::from_secs(interval_secs)
                    };
                    tracing::debug!(
                        cron_id = %job_id,
                        interval_secs,
                        sleep_secs = delay.as_secs(),
                        "interval cron next fire computed"
                    );
                    delay
                };

                tokio::time::sleep(sleep_duration).await;

                let job = {
                    let j = jobs.read().await;
                    match j.get(&job_id) {
                        Some(j) if !j.enabled => {
                            tracing::debug!(cron_id = %job_id, "cron job disabled, stopping timer");
                            break;
                        }
                        Some(j) => j.clone(),
                        None => {
                            tracing::debug!(cron_id = %job_id, "cron job removed, stopping timer");
                            break;
                        }
                    }
                };

                // Check active hours window
                if let Some((start, end)) = job.active_hours {
                    let (current_hour, timezone) = current_hour_and_timezone(&context, &job_id);
                    let in_window = hour_in_active_window(current_hour, start, end);
                    if !in_window {
                        tracing::debug!(
                            cron_id = %job_id,
                            cron_timezone = %timezone,
                            current_hour,
                            start,
                            end,
                            "outside active hours, skipping"
                        );
                        continue;
                    }
                }

                if execution_lock.load(std::sync::atomic::Ordering::Acquire) {
                    tracing::debug!(cron_id = %job_id, "previous execution still running, skipping tick");
                    continue;
                }

                tracing::info!(cron_id = %job_id, "cron job firing");
                execution_lock.store(true, std::sync::atomic::Ordering::Release);

                let exec_jobs = jobs.clone();
                let exec_context = context.clone();
                let exec_job_id = job_id.clone();
                let guard = ExecutionGuard(execution_lock.clone());

                tokio::spawn(async move {
                    let _guard = guard;
                    match run_cron_job(&job, &exec_context).await {
                        Ok(()) => {
                            let mut j = exec_jobs.write().await;
                            if let Some(j) = j.get_mut(&exec_job_id) {
                                j.consecutive_failures = 0;
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                cron_id = %exec_job_id,
                                %error,
                                "cron job execution failed"
                            );

                            let should_disable = {
                                let mut j = exec_jobs.write().await;
                                if let Some(j) = j.get_mut(&exec_job_id) {
                                    j.consecutive_failures += 1;
                                    j.consecutive_failures >= MAX_CONSECUTIVE_FAILURES
                                } else {
                                    false
                                }
                            };

                            if should_disable {
                                tracing::warn!(
                                    cron_id = %exec_job_id,
                                    "circuit breaker tripped after {MAX_CONSECUTIVE_FAILURES} consecutive failures, disabling"
                                );

                                {
                                    let mut j = exec_jobs.write().await;
                                    if let Some(j) = j.get_mut(&exec_job_id) {
                                        j.enabled = false;
                                    }
                                }

                                if let Err(error) =
                                    exec_context.store.update_enabled(&exec_job_id, false).await
                                {
                                    tracing::error!(%error, "failed to persist cron job disabled state");
                                }
                            }
                        }
                    }

                    if job.run_once {
                        tracing::info!(cron_id = %exec_job_id, "run-once cron completed, disabling");

                        {
                            let mut j = exec_jobs.write().await;
                            if let Some(j) = j.get_mut(&exec_job_id) {
                                j.enabled = false;
                            }
                        }

                        if let Err(error) =
                            exec_context.store.update_enabled(&exec_job_id, false).await
                        {
                            tracing::error!(%error, "failed to persist run-once cron disabled state");
                        }
                    }
                });
            }
        });

        // Insert the new handle. Any previously existing handle was already aborted above.
        let mut timers = self.timers.write().await;
        timers.insert(job_id_for_map, handle);
    }

    /// Shutdown all cron job timers and wait for them to finish.
    pub async fn shutdown(&self) {
        let handles: Vec<(String, tokio::task::JoinHandle<()>)> = {
            let mut timers = self.timers.write().await;
            timers.drain().collect()
        };

        for (id, handle) in handles {
            handle.abort();
            let _ = handle.await;
            tracing::debug!(cron_id = %id, "cron timer stopped");
        }
    }

    /// Unregister and stop a cron job.
    pub async fn unregister(&self, job_id: &str) {
        // Remove the timer handle and abort it
        let handle = {
            let mut timers = self.timers.write().await;
            timers.remove(job_id)
        };

        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
            tracing::debug!(cron_id = %job_id, "cron timer stopped");
        }

        // Remove the job from the jobs map
        let removed = {
            let mut jobs = self.jobs.write().await;
            jobs.remove(job_id).is_some()
        };

        if removed {
            tracing::info!(cron_id = %job_id, "cron job unregistered");
        }
    }

    /// Check if a job is currently registered.
    pub async fn is_registered(&self, job_id: &str) -> bool {
        let jobs = self.jobs.read().await;
        jobs.contains_key(job_id)
    }

    /// Trigger a cron job immediately, outside the timer loop.
    pub async fn trigger_now(&self, job_id: &str) -> Result<()> {
        let job = {
            let jobs = self.jobs.read().await;
            jobs.get(job_id).cloned()
        };

        if let Some(job) = job {
            if !job.enabled {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "cron job is disabled"
                )));
            }

            tracing::info!(cron_id = %job_id, "cron job triggered manually");
            run_cron_job(&job, &self.context).await
        } else {
            Err(crate::error::Error::Other(anyhow::anyhow!(
                "cron job not found"
            )))
        }
    }

    /// Update a job's enabled state and manage its timer accordingly.
    ///
    /// Handles three cases:
    /// - Enabling a job that is in the HashMap (normal re-enable): update flag, start timer.
    /// - Enabling a job NOT in the HashMap (cold re-enable after restart with job disabled):
    ///   reload config from the CronStore, insert into HashMap, start timer.
    /// - Disabling: update flag and abort the timer immediately rather than waiting up to
    ///   one full interval for the loop to notice.
    pub async fn set_enabled(&self, job_id: &str, enabled: bool) -> Result<()> {
        // Try to find the job in the in-memory HashMap.
        let in_memory = {
            let jobs = self.jobs.read().await;
            jobs.contains_key(job_id)
        };

        if !in_memory {
            if !enabled {
                // Disabling something that isn't running — nothing to do.
                tracing::debug!(cron_id = %job_id, "set_enabled(false): job not in scheduler, nothing to do");
                return Ok(());
            }

            // Cold re-enable: job was disabled at startup so was never loaded into the scheduler.
            // Reload from the store, insert, then start the timer.
            tracing::info!(cron_id = %job_id, "cold re-enable: reloading config from store");
            let configs = self.context.store.load_all_unfiltered().await?;
            let config = configs
                .into_iter()
                .find(|c| c.id == job_id)
                .ok_or_else(|| {
                    crate::error::Error::Other(anyhow::anyhow!("cron job not found in store"))
                })?;

            let delivery_target =
                parse_delivery_target(&config.delivery_target).ok_or_else(|| {
                    crate::error::Error::Other(anyhow::anyhow!(
                        "invalid delivery target '{}': expected format 'adapter:target'",
                        config.delivery_target
                    ))
                })?;

            {
                let mut jobs = self.jobs.write().await;
                jobs.insert(
                    job_id.to_string(),
                    CronJob {
                        id: config.id.clone(),
                        prompt: config.prompt,
                        cron_expr: normalize_cron_expr(config.cron_expr)?,
                        interval_secs: config.interval_secs,
                        delivery_target,
                        active_hours: normalize_active_hours(config.active_hours),
                        enabled: true,
                        run_once: config.run_once,
                        consecutive_failures: 0,
                        timeout_secs: config.timeout_secs,
                    },
                );
            }

            self.start_timer(job_id).await;
            tracing::info!(cron_id = %job_id, "cron job cold-re-enabled and timer started");
            return Ok(());
        }

        // Job is in the HashMap — normal path.
        let was_enabled = {
            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.get_mut(job_id) {
                let old = job.enabled;
                job.enabled = enabled;
                old
            } else {
                // Should not happen (we checked above), but be defensive.
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "cron job not found"
                )));
            }
        };

        if enabled && !was_enabled {
            self.start_timer(job_id).await;
            tracing::info!(cron_id = %job_id, "cron job enabled and timer started");
        }

        if !enabled && was_enabled {
            // Abort the timer immediately rather than waiting up to one full interval.
            let handle = {
                let mut timers = self.timers.write().await;
                timers.remove(job_id)
            };
            if let Some(handle) = handle {
                handle.abort();
                tracing::info!(cron_id = %job_id, "cron job disabled, timer aborted immediately");
            }
        }

        Ok(())
    }
}

fn cron_timezone_label(context: &CronContext) -> String {
    let timezone = context.deps.runtime_config.cron_timezone.load();
    match timezone.as_deref() {
        Some(name) if name.parse::<Tz>().is_ok() => name.to_string(),
        _ => SYSTEM_TIMEZONE_LABEL.to_string(),
    }
}

fn current_hour_and_timezone(context: &CronContext, cron_id: &str) -> (u8, String) {
    let timezone = context.deps.runtime_config.cron_timezone.load();
    match timezone.as_deref() {
        Some(name) => match name.parse::<Tz>() {
            Ok(timezone) => (
                chrono::Utc::now().with_timezone(&timezone).hour() as u8,
                name.into(),
            ),
            Err(error) => {
                tracing::warn!(
                    agent_id = %context.deps.agent_id,
                    cron_id,
                    cron_timezone = %name,
                    %error,
                    "invalid cron timezone in runtime config, falling back to system timezone"
                );
                (
                    chrono::Local::now().hour() as u8,
                    SYSTEM_TIMEZONE_LABEL.into(),
                )
            }
        },
        None => (
            chrono::Local::now().hour() as u8,
            SYSTEM_TIMEZONE_LABEL.into(),
        ),
    }
}

fn hour_in_active_window(current_hour: u8, start_hour: u8, end_hour: u8) -> bool {
    if start_hour == end_hour {
        return true;
    }
    if start_hour < end_hour {
        current_hour >= start_hour && current_hour < end_hour
    } else {
        current_hour >= start_hour || current_hour < end_hour
    }
}

/// Normalize degenerate active_hours where start == end to None (always active).
fn normalize_active_hours(active_hours: Option<(u8, u8)>) -> Option<(u8, u8)> {
    active_hours.filter(|(start, end)| start != end)
}

fn normalize_cron_expr(cron_expr: Option<String>) -> Result<Option<String>> {
    let Some(expr) = cron_expr else {
        return Ok(None);
    };

    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let field_count = trimmed.split_whitespace().count();
    if field_count != 5 {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cron expression must have exactly 5 fields (got {field_count}): '{trimmed}'"
        )));
    }

    Schedule::from_str(trimmed).map_err(|error| {
        crate::error::Error::Other(anyhow::anyhow!(
            "invalid cron expression '{trimmed}': {error}"
        ))
    })?;

    Ok(Some(trimmed.to_string()))
}

fn interval_initial_delay(interval_secs: u64) -> Duration {
    if interval_secs < 86400 && 86400 % interval_secs == 0 {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let remainder = now_unix % interval_secs;
        let secs_until = if remainder == 0 {
            interval_secs
        } else {
            interval_secs - remainder
        };
        Duration::from_secs(secs_until)
    } else {
        Duration::from_secs(interval_secs)
    }
}

fn resolve_cron_timezone(context: &CronContext) -> (Option<chrono_tz::Tz>, String) {
    let timezone = context.deps.runtime_config.cron_timezone.load();
    match timezone.as_deref() {
        Some(name) => match name.parse::<Tz>() {
            Ok(timezone) => (Some(timezone), name.to_string()),
            Err(error) => {
                tracing::warn!(
                    agent_id = %context.deps.agent_id,
                    cron_timezone = %name,
                    %error,
                    "invalid cron timezone in runtime config, falling back to system timezone"
                );
                (None, SYSTEM_TIMEZONE_LABEL.to_string())
            }
        },
        None => (None, SYSTEM_TIMEZONE_LABEL.to_string()),
    }
}

fn next_fire_duration(
    context: &CronContext,
    cron_id: &str,
    cron_expr: &str,
) -> Option<(Duration, chrono::DateTime<chrono::Utc>, String)> {
    let schedule = match Schedule::from_str(cron_expr) {
        Ok(schedule) => schedule,
        Err(error) => {
            tracing::warn!(cron_id = %cron_id, cron_expr, %error, "invalid cron expression");
            return None;
        }
    };

    let now_utc = chrono::Utc::now();
    let (timezone, timezone_label) = resolve_cron_timezone(context);
    let next_utc = if let Some(timezone) = timezone {
        let now_local = now_utc.with_timezone(&timezone);
        schedule
            .after(&now_local)
            .next()?
            .with_timezone(&chrono::Utc)
    } else {
        let now_local = chrono::Local::now();
        schedule
            .after(&now_local)
            .next()?
            .with_timezone(&chrono::Utc)
    };
    let delay_ms = (next_utc - now_utc).num_milliseconds().max(1) as u64;

    Some((Duration::from_millis(delay_ms), next_utc, timezone_label))
}

fn ensure_cron_dispatch_readiness(context: &CronContext, cron_id: &str) {
    let readiness = context.deps.runtime_config.work_readiness();
    if readiness.ready {
        return;
    }

    let reason = readiness
        .reason
        .map(|value| value.as_str())
        .unwrap_or("unknown");
    tracing::warn!(
        agent_id = %context.deps.agent_id,
        cron_id,
        dispatch_type = "cron",
        reason,
        warmup_state = ?readiness.warmup_state,
        embedding_ready = readiness.embedding_ready,
        bulletin_age_secs = ?readiness.bulletin_age_secs,
        stale_after_secs = readiness.stale_after_secs,
        "cron dispatch requested before readiness contract was satisfied"
    );

    #[cfg(feature = "metrics")]
    crate::telemetry::Metrics::global()
        .dispatch_while_cold_count
        .with_label_values(&[&*context.deps.agent_id, "cron", reason])
        .inc();

    let warmup_config = **context.deps.runtime_config.warmup.load();
    let should_trigger = readiness.warmup_state != crate::config::WarmupState::Warming
        && (readiness.reason != Some(crate::config::WorkReadinessReason::EmbeddingNotReady)
            || warmup_config.eager_embedding_load);

    if should_trigger {
        crate::agent::cortex::trigger_forced_warmup(context.deps.clone(), "cron");
    }
}

/// Execute a single cron job: create a fresh channel, run the prompt, deliver the result.
#[tracing::instrument(skip(context), fields(cron_id = %job.id, agent_id = %context.deps.agent_id))]
async fn run_cron_job(job: &CronJob, context: &CronContext) -> Result<()> {
    ensure_cron_dispatch_readiness(context, &job.id);
    let channel_id: crate::ChannelId = Arc::from(format!("cron:{}", job.id).as_str());

    // Create the outbound response channel to collect whatever the channel produces
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<OutboundResponse>(32);

    // Subscribe to the agent's event bus (the channel needs this for branch/worker events)
    let event_rx = context.deps.event_tx.subscribe();

    let (channel, channel_tx) = Channel::new(
        channel_id.clone(),
        context.deps.clone(),
        response_tx,
        event_rx,
        context.screenshot_dir.clone(),
        context.logs_dir.clone(),
    );

    // Spawn the channel's event loop
    let channel_handle = tokio::spawn(async move {
        if let Err(error) = channel.run().await {
            tracing::error!(%error, "cron channel failed");
        }
    });

    // Send the cron job prompt as a synthetic message
    let message = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "cron".into(),
        adapter: None,
        conversation_id: format!("cron:{}", job.id),
        sender_id: "system".into(),
        agent_id: Some(context.deps.agent_id.clone()),
        content: MessageContent::Text(job.prompt.clone()),
        timestamp: chrono::Utc::now(),
        metadata: HashMap::new(),
        formatted_author: None,
    };

    channel_tx
        .send(message)
        .await
        .map_err(|error| anyhow::anyhow!("failed to send cron prompt to channel: {error}"))?;

    // Collect responses with a timeout. The channel may produce multiple messages
    // (e.g. status updates, then text). We only care about text responses.
    let mut collected_text = Vec::new();
    let timeout = Duration::from_secs(job.timeout_secs.unwrap_or(120));

    // Drop the sender so the channel knows no more messages are coming.
    // The channel will process the one message and then its event loop will end
    // when the sender is dropped (message_rx returns None).
    drop(channel_tx);

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(cron_id = %job.id, "cron job timed out after {timeout:?}");
            channel_handle.abort();
            break;
        }
        match tokio::time::timeout(remaining, response_rx.recv()).await {
            Ok(Some(OutboundResponse::Text(text))) => {
                collected_text.push(text);
            }
            Ok(Some(OutboundResponse::RichMessage { text, .. })) => {
                collected_text.push(text);
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                break;
            }
            Err(_) => {
                tracing::warn!(cron_id = %job.id, "cron job timed out after {timeout:?}");
                channel_handle.abort();
                break;
            }
        }
    }

    // Wait for the channel task to finish (it should already be done since we dropped channel_tx)
    let _ = channel_handle.await;

    let result_text = collected_text.join("\n\n");
    let has_result = !result_text.trim().is_empty();

    // Deliver result to target (only if there's something to say)
    if has_result {
        if let Err(error) = context
            .messaging_manager
            .broadcast(
                &job.delivery_target.adapter,
                &job.delivery_target.target,
                OutboundResponse::Text(result_text.clone()),
            )
            .await
        {
            tracing::error!(
                cron_id = %job.id,
                target = %job.delivery_target,
                %error,
                "failed to deliver cron result"
            );
            if let Err(log_error) = context
                .store
                .log_execution(&job.id, false, Some(&error.to_string()))
                .await
            {
                tracing::warn!(%log_error, "failed to log cron execution");
            }
            return Err(error);
        }

        tracing::info!(
            cron_id = %job.id,
            target = %job.delivery_target,
            "cron result delivered"
        );
    } else {
        tracing::debug!(cron_id = %job.id, "cron job produced no output, skipping delivery");
    }

    let summary = if has_result {
        Some(result_text.as_str())
    } else {
        None
    };
    if let Err(error) = context.store.log_execution(&job.id, true, summary).await {
        tracing::warn!(%error, "failed to log cron execution");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{hour_in_active_window, normalize_active_hours};

    #[test]
    fn test_hour_in_active_window_non_wrapping() {
        assert!(hour_in_active_window(9, 9, 17));
        assert!(hour_in_active_window(16, 9, 17));
        assert!(!hour_in_active_window(8, 9, 17));
        assert!(!hour_in_active_window(17, 9, 17));
    }

    #[test]
    fn test_hour_in_active_window_midnight_wrapping() {
        assert!(hour_in_active_window(22, 22, 6));
        assert!(hour_in_active_window(3, 22, 6));
        assert!(!hour_in_active_window(12, 22, 6));
    }

    #[test]
    fn test_hour_in_active_window_equal_start_end_is_always_active() {
        assert!(hour_in_active_window(0, 0, 0));
        assert!(hour_in_active_window(12, 0, 0));
        assert!(hour_in_active_window(23, 0, 0));
        assert!(hour_in_active_window(5, 5, 5));
        assert!(hour_in_active_window(14, 14, 14));
    }

    #[test]
    fn test_normalize_active_hours() {
        assert_eq!(normalize_active_hours(Some((0, 0))), None);
        assert_eq!(normalize_active_hours(Some((12, 12))), None);
        assert_eq!(normalize_active_hours(Some((9, 17))), Some((9, 17)));
        assert_eq!(normalize_active_hours(None), None);
    }
}
