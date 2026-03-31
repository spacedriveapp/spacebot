//! Cron scheduler: timer management and execution.
//!
//! Each cron job gets its own tokio task that fires on an interval.
//! When a job fires, it creates a fresh short-lived channel,
//! runs the job's prompt through the LLM, and delivers the result
//! to the delivery target via the messaging system.

use crate::agent::channel::Channel;
use crate::cron::store::{CronExecutionRecord, CronStore};
use crate::error::Result;
use crate::messaging::MessagingManager;
use crate::messaging::target::{BroadcastTarget, parse_delivery_target};
use crate::{AgentDeps, InboundMessage, MessageContent, OutboundResponse, RoutedResponse};
use chrono::Timelike;
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
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
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
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
    #[serde(default)]
    pub next_run_at: Option<String>,
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
    /// SAFETY: This is the only place that writes to this AtomicBool, and all reads
    /// use `Acquire` ordering (see line 436). The `Release` store here establishes
    /// a happens-before relationship with those acquire loads, ensuring the flag
    /// is properly cleared when observed by other threads.
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Emit a cron execution error to both working memory and tracing.
/// Centralizes error reporting to ensure consistent handling across all error paths.
fn emit_cron_error(context: &CronContext, job_id: &str, error: &crate::error::Error) {
    let message = format!("Cron failed: {job_id}: {error}");

    // Emit to working memory for agent context awareness
    context
        .deps
        .working_memory
        .emit(crate::memory::WorkingMemoryEventType::Error, message)
        .importance(0.8)
        .record();

    // Log to tracing for observability
    tracing::error!(cron_id = %job_id, %error, "cron job execution failed");
}

#[derive(Debug)]
enum CronRunError {
    Execution(crate::error::Error),
    Delivery(crate::error::Error),
}

impl CronRunError {
    fn as_error(&self) -> &crate::error::Error {
        match self {
            Self::Execution(error) | Self::Delivery(error) => error,
        }
    }

    fn into_error(self) -> crate::error::Error {
        match self {
            Self::Execution(error) | Self::Delivery(error) => error,
        }
    }

    fn failure_class(&self) -> &'static str {
        match self {
            Self::Execution(_) => "execution_error",
            Self::Delivery(_) => "delivery_error",
        }
    }
}

impl From<crate::error::Error> for CronRunError {
    fn from(error: crate::error::Error) -> Self {
        Self::Execution(error)
    }
}

async fn set_job_enabled_state(
    jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
    job_id: &str,
    enabled: bool,
) -> Result<()> {
    let mut jobs = jobs.write().await;
    let Some(job) = jobs.get_mut(job_id) else {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cron job not found"
        )));
    };
    job.enabled = enabled;
    Ok(())
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
        self.register_with_anchor(config, None).await
    }

    /// Register and start a cron job, optionally anchoring interval-based jobs
    /// to their last execution time. When `last_executed_at` is provided,
    /// interval jobs compute their first sleep from that timestamp instead of
    /// falling back to epoch-aligned delays, preventing skipped or duplicate
    /// firings after a restart.
    pub async fn register_with_anchor(
        &self,
        config: CronConfig,
        last_executed_at: Option<&str>,
    ) -> Result<()> {
        let job = cron_job_from_config(&config)?;
        let anchor = if config.enabled {
            let anchor = last_executed_at.and_then(|ts| {
                chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
                    .ok()
                    .map(|naive| naive.and_utc())
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc3339(ts)
                            .ok()
                            .map(|dt| dt.to_utc())
                    })
            });
            if anchor.is_none() && last_executed_at.is_some() {
                tracing::warn!(
                    cron_id = %config.id,
                    ?last_executed_at,
                    "failed to parse last_executed_at; falling back to epoch-aligned interval delay"
                );
            }
            anchor
        } else {
            None
        };

        let maybe_prev_job = {
            let mut jobs = self.jobs.write().await;
            jobs.insert(config.id.clone(), job)
        };

        if config.enabled
            && let Err(error) = self.ensure_job_next_run_at(&config.id, anchor).await
        {
            {
                let mut jobs = self.jobs.write().await;
                // Restore previous job if one existed, otherwise remove the key
                match maybe_prev_job {
                    Some(prev_job) => {
                        jobs.insert(config.id.clone(), prev_job);
                    }
                    None => {
                        jobs.remove(&config.id);
                    }
                }
            }

            tracing::warn!(
                cron_id = %config.id,
                %error,
                "failed to initialize cron cursor; rolling back in-memory registration"
            );
            return Err(error);
        }

        if config.enabled {
            self.start_timer(&config.id, anchor).await;
        }

        tracing::info!(
            cron_id = %config.id,
            interval_secs = config.interval_secs,
            cron_expr = ?config.cron_expr,
            run_once = config.run_once,
            ?last_executed_at,
            "cron job registered"
        );
        Ok(())
    }

    async fn ensure_job_next_run_at(
        &self,
        job_id: &str,
        anchor: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let next_run_at = {
            let jobs = self.jobs.read().await;
            let Some(job) = jobs.get(job_id) else {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "cron job not found"
                )));
            };

            if job.next_run_at.is_some() {
                return Ok(());
            }

            compute_initial_next_run_at(job, &self.context, anchor)
        };

        let Some(next_run_at) = next_run_at else {
            return Ok(());
        };

        let next_run_at_text = format_cron_timestamp(next_run_at);
        let initialized = self
            .context
            .store
            .initialize_next_run_at(job_id, &next_run_at_text)
            .await?;

        if initialized {
            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.get_mut(job_id)
                && job.next_run_at.is_none()
            {
                job.next_run_at = Some(next_run_at);
            }
        } else {
            sync_job_from_store(&self.context.store, &self.jobs, job_id).await?;
        }

        Ok(())
    }

    /// Start a timer loop for a cron job.
    ///
    /// Idempotent: if a timer is already running for this job, it is aborted before
    /// starting a new one. This prevents timer leaks when a job is re-registered via API.
    ///
    /// When `anchor` is provided, interval-based jobs use it to compute the first
    /// sleep duration from the last known execution, preventing skipped or duplicate
    /// firings after a restart.
    async fn start_timer(&self, job_id: &str, _anchor: Option<chrono::DateTime<chrono::Utc>>) {
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

                let Some(next_run_at) =
                    ensure_job_cursor_initialized(&context, &jobs, &job_id, &job, None).await
                else {
                    tracing::warn!(cron_id = %job_id, "cron job has no next_run_at, retrying in 60s");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                };

                let now = chrono::Utc::now();
                if let Some(fast_forward_to) =
                    stale_recovery_next_run_at(&job, &context, next_run_at, now)
                {
                    if let Err(error) = advance_job_cursor(
                        &context,
                        &jobs,
                        &job_id,
                        &job,
                        next_run_at,
                        fast_forward_to,
                    )
                    .await
                    {
                        tracing::warn!(
                            cron_id = %job_id,
                            scheduled_run_at = %format_cron_timestamp(next_run_at),
                            next_run_at = %format_cron_timestamp(fast_forward_to),
                            %error,
                            "failed to fast-forward stale cron cursor"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    continue;
                }

                let sleep_duration = duration_until_next_run(next_run_at);

                tracing::debug!(
                    cron_id = %job_id,
                    next_run_at = %format_cron_timestamp(next_run_at),
                    sleep_secs = sleep_duration.as_secs(),
                    "cron next fire computed from persisted cursor"
                );

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

                let Some(scheduled_run_at) = job.next_run_at else {
                    continue;
                };
                let now = chrono::Utc::now();
                if scheduled_run_at > now {
                    continue;
                }

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
                        if let Some(next_run_at) =
                            compute_following_next_run_at(&job, &context, scheduled_run_at)
                            && let Err(error) = advance_job_cursor(
                                &context,
                                &jobs,
                                &job_id,
                                &job,
                                scheduled_run_at,
                                next_run_at,
                            )
                            .await
                        {
                            tracing::warn!(
                                cron_id = %job_id,
                                scheduled_run_at = %format_cron_timestamp(scheduled_run_at),
                                next_run_at = %format_cron_timestamp(next_run_at),
                                %error,
                                "failed to advance skipped cron cursor for active-hours gate"
                            );
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                        continue;
                    }
                }

                if execution_lock.load(std::sync::atomic::Ordering::Acquire) {
                    tracing::debug!(cron_id = %job_id, "previous execution still running, skipping tick");
                    if let Some(next_run_at) =
                        compute_following_next_run_at(&job, &context, scheduled_run_at)
                        && let Err(error) = advance_job_cursor(
                            &context,
                            &jobs,
                            &job_id,
                            &job,
                            scheduled_run_at,
                            next_run_at,
                        )
                        .await
                    {
                        tracing::warn!(
                            cron_id = %job_id,
                            scheduled_run_at = %format_cron_timestamp(scheduled_run_at),
                            next_run_at = %format_cron_timestamp(next_run_at),
                            %error,
                            "failed to advance skipped cron cursor while prior execution was still running"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    continue;
                }

                let claimed = if job.run_once {
                    claim_run_once_fire(&context, &jobs, &job_id, scheduled_run_at).await
                } else if let Some(next_run_at) =
                    compute_following_next_run_at(&job, &context, scheduled_run_at)
                {
                    advance_job_cursor(
                        &context,
                        &jobs,
                        &job_id,
                        &job,
                        scheduled_run_at,
                        next_run_at,
                    )
                    .await
                } else {
                    Ok(false)
                };

                match claimed {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(cron_id = %job_id, %error, "failed to claim cron fire");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
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
                            exec_context
                                .deps
                                .working_memory
                                .emit(
                                    crate::memory::WorkingMemoryEventType::CronExecuted,
                                    format!("Cron completed: {exec_job_id}"),
                                )
                                .importance(0.4)
                                .record();

                            let mut j = exec_jobs.write().await;
                            if let Some(j) = j.get_mut(&exec_job_id) {
                                j.consecutive_failures = 0;
                            }
                        }
                        Err(error) => {
                            emit_cron_error(&exec_context, &exec_job_id, error.as_error());

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

    /// Return the number of enabled (active) cron jobs.
    pub async fn job_count(&self) -> usize {
        self.jobs
            .read()
            .await
            .values()
            .filter(|job| job.enabled)
            .count()
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
            run_cron_job(&job, &self.context)
                .await
                .map_err(CronRunError::into_error)
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
            // Reload from the store, insert with enabled=false first, initialize, then enable atomically.
            tracing::info!(cron_id = %job_id, "cold re-enable: reloading config from store");
            let config = self.context.store.load(job_id).await?.ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("cron job not found in store"))
            })?;
            let job = cron_job_from_config(&config)?;

            // Insert disabled first to avoid orphaned enabled state if init fails
            {
                let mut jobs = self.jobs.write().await;
                jobs.insert(job_id.to_string(), job);
            }

            // Initialize cursor, make the enabled state visible, then start the timer.
            if let Err(error) = self.ensure_job_next_run_at(job_id, None).await {
                // Clean up on failure - remove the partially inserted job
                let mut jobs = self.jobs.write().await;
                jobs.remove(job_id);
                return Err(error);
            }
            set_job_enabled_state(&self.jobs, job_id, true).await?;
            self.start_timer(job_id, None).await;
            tracing::info!(cron_id = %job_id, "cron job cold-re-enabled and timer started");
            return Ok(());
        }

        // Job is in the HashMap — normal path.
        // Use read lock to check current state first
        let was_enabled = {
            let jobs = self.jobs.read().await;
            jobs.get(job_id).map(|j| j.enabled).unwrap_or(false)
        };

        if enabled && !was_enabled {
            // Initialize the cursor, make the enabled state visible, then start the timer.
            self.ensure_job_next_run_at(job_id, None).await?;
            set_job_enabled_state(&self.jobs, job_id, true).await?;
            self.start_timer(job_id, None).await;
            tracing::info!(cron_id = %job_id, "cron job enabled and timer started");
        }

        if !enabled && was_enabled {
            // Atomically disable first, then abort timer
            set_job_enabled_state(&self.jobs, job_id, false).await?;

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

fn cron_job_from_config(config: &CronConfig) -> Result<CronJob> {
    let delivery_target = parse_delivery_target(&config.delivery_target).ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "invalid delivery target '{}': expected format 'adapter:target'",
            config.delivery_target
        ))
    })?;
    let cron_expr = normalize_cron_expr(config.cron_expr.clone())?;

    if cron_expr.is_none() && config.interval_secs == 0 {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "interval_secs must be > 0 when no cron_expr is provided"
        )));
    }

    Ok(CronJob {
        id: config.id.clone(),
        prompt: config.prompt.clone(),
        cron_expr,
        interval_secs: config.interval_secs,
        delivery_target,
        active_hours: normalize_active_hours(config.active_hours),
        enabled: config.enabled,
        run_once: config.run_once,
        consecutive_failures: 0,
        next_run_at: config.next_run_at.as_deref().and_then(parse_cron_timestamp),
        timeout_secs: config.timeout_secs,
    })
}

async fn sync_job_from_store(
    store: &CronStore,
    jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
    job_id: &str,
) -> Result<()> {
    let Some(config) = store.load(job_id).await? else {
        let mut guard = jobs.write().await;
        guard.remove(job_id);
        return Ok(());
    };

    let synced_job = cron_job_from_config(&config)?;

    let mut guard = jobs.write().await;
    if let Some(job) = guard.get_mut(job_id) {
        let consecutive_failures = job.consecutive_failures;
        *job = CronJob {
            consecutive_failures,
            ..synced_job
        };
    }

    Ok(())
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

    // The `cron` crate uses 7-field expressions (sec min hour dom month dow year).
    // Users write standard 5-field cron (min hour dom month dow). Convert by
    // prepending "0" for seconds and appending "*" for year.
    let expanded = format!("0 {trimmed} *");

    Schedule::from_str(&expanded).map_err(|error| {
        crate::error::Error::Other(anyhow::anyhow!(
            "invalid cron expression '{trimmed}': {error}"
        ))
    })?;

    // Store the original 5-field form — it's what users and the UI expect.
    Ok(Some(trimmed.to_string()))
}

/// Compute the initial delay for an interval-based cron job, anchored to its
/// last execution time when available.
///
/// With an anchor:
///   - `elapsed = now - last_run`
///   - If `elapsed >= interval`, fire after a short 2s jitter (avoid thundering herd on restart).
///   - Otherwise, sleep for `interval - elapsed` (the remainder).
///
/// Without an anchor (first-ever run, or no execution history), falls back to
/// `interval_initial_delay` which aligns to clean epoch-based clock boundaries.
fn anchored_initial_delay(
    interval_secs: u64,
    anchor: Option<chrono::DateTime<chrono::Utc>>,
) -> Duration {
    if let Some(last_run) = anchor {
        let now = chrono::Utc::now();
        let elapsed = (now - last_run).num_seconds().max(0) as u64;
        if elapsed >= interval_secs {
            // Overdue — fire soon with a small jitter to avoid thundering herd
            Duration::from_secs(2)
        } else {
            Duration::from_secs(interval_secs - elapsed)
        }
    } else {
        interval_initial_delay(interval_secs)
    }
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

fn parse_cron_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.to_utc())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|timestamp| timestamp.and_utc())
        })
}

fn format_cron_timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn duration_until_next_run(next_run_at: chrono::DateTime<chrono::Utc>) -> Duration {
    let delay_ms = (next_run_at - chrono::Utc::now()).num_milliseconds().max(1) as u64;
    Duration::from_millis(delay_ms)
}

fn compute_initial_next_run_at(
    job: &CronJob,
    context: &CronContext,
    anchor: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Some(cron_expr) = job.cron_expr.as_deref() {
        next_fire_after(context, &job.id, cron_expr, chrono::Utc::now()).map(|(next, _)| next)
    } else {
        let delay = anchored_initial_delay(job.interval_secs, anchor);
        Some(chrono::Utc::now() + chrono::Duration::from_std(delay).ok()?)
    }
}

fn compute_following_next_run_at(
    job: &CronJob,
    context: &CronContext,
    after: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Some(cron_expr) = job.cron_expr.as_deref() {
        next_fire_after(context, &job.id, cron_expr, after).map(|(next, _)| next)
    } else {
        Some(after + chrono::Duration::seconds(job.interval_secs as i64))
    }
}

fn recurring_grace_window(job: &CronJob, context: &CronContext) -> Duration {
    const MIN_GRACE_SECS: u64 = 120;
    const MAX_GRACE_SECS: u64 = 7200;

    let period_secs = if let Some(cron_expr) = job.cron_expr.as_deref() {
        cron_period_secs(context, cron_expr).unwrap_or(MIN_GRACE_SECS)
    } else {
        job.interval_secs.max(1)
    };

    let grace_secs = (period_secs / 2).clamp(MIN_GRACE_SECS, MAX_GRACE_SECS);
    Duration::from_secs(grace_secs)
}

fn stale_recovery_next_run_at(
    job: &CronJob,
    context: &CronContext,
    next_run_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let overdue = (now - next_run_at).to_std().ok()?;
    if overdue <= recurring_grace_window(job, context) {
        return None;
    }

    compute_following_next_run_at(job, context, now)
}

async fn ensure_job_cursor_initialized(
    context: &CronContext,
    jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
    job_id: &str,
    job: &CronJob,
    anchor: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Some(next_run_at) = job.next_run_at {
        return Some(next_run_at);
    }

    let next_run_at = compute_initial_next_run_at(job, context, anchor)?;
    let next_run_at_text = format_cron_timestamp(next_run_at);
    let initialized = match context
        .store
        .initialize_next_run_at(job_id, &next_run_at_text)
        .await
    {
        Ok(initialized) => initialized,
        Err(error) => {
            tracing::warn!(cron_id = %job_id, %error, "failed to initialize cron cursor");
            return None;
        }
    };

    if initialized {
        let mut guard = jobs.write().await;
        if let Some(job) = guard.get_mut(job_id)
            && job.next_run_at.is_none()
        {
            job.next_run_at = Some(next_run_at);
        }

        return Some(next_run_at);
    }

    if let Err(error) = sync_job_from_store(&context.store, jobs, job_id).await {
        tracing::warn!(cron_id = %job_id, %error, "failed to refresh cron cursor after contention");
        return None;
    }

    let guard = jobs.read().await;
    guard.get(job_id).and_then(|job| job.next_run_at)
}

async fn advance_job_cursor(
    context: &CronContext,
    jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
    job_id: &str,
    job: &CronJob,
    expected_next_run_at: chrono::DateTime<chrono::Utc>,
    next_run_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let expected = format_cron_timestamp(expected_next_run_at);
    let next = format_cron_timestamp(next_run_at);
    let claimed = context
        .store
        .claim_and_advance(job_id, &expected, &next)
        .await?;

    if claimed {
        let mut guard = jobs.write().await;
        if let Some(stored_job) = guard.get_mut(job_id) {
            stored_job.next_run_at = Some(next_run_at);
            stored_job.enabled = job.enabled;
        }
    } else {
        sync_job_from_store(&context.store, jobs, job_id).await?;
    }

    Ok(claimed)
}

async fn claim_run_once_fire(
    context: &CronContext,
    jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
    job_id: &str,
    expected_next_run_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let expected = format_cron_timestamp(expected_next_run_at);
    let claimed = context.store.claim_run_once(job_id, &expected).await?;

    if claimed {
        let mut guard = jobs.write().await;
        if let Some(job) = guard.get_mut(job_id) {
            job.enabled = false;
            job.next_run_at = None;
        }
    } else {
        sync_job_from_store(&context.store, jobs, job_id).await?;
    }

    Ok(claimed)
}

/// Expand a 5-field standard cron expression to the 7-field format required by
/// the `cron` crate: `sec min hour dom month dow year`. If the expression
/// already has 6+ fields, return it as-is.
fn expand_cron_expr(expr: &str) -> String {
    let field_count = expr.split_whitespace().count();
    if field_count == 5 {
        format!("0 {expr} *")
    } else {
        expr.to_string()
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

fn next_fire_after(
    context: &CronContext,
    cron_id: &str,
    cron_expr: &str,
    after_utc: chrono::DateTime<chrono::Utc>,
) -> Option<(chrono::DateTime<chrono::Utc>, String)> {
    // Expand 5-field standard cron to 7-field for the `cron` crate.
    let expanded = expand_cron_expr(cron_expr);
    let schedule = match Schedule::from_str(&expanded) {
        Ok(schedule) => schedule,
        Err(error) => {
            tracing::warn!(cron_id = %cron_id, cron_expr, %error, "invalid cron expression");
            return None;
        }
    };

    let (timezone, timezone_label) = resolve_cron_timezone(context);
    let next_utc = if let Some(timezone) = timezone {
        let after_local = after_utc.with_timezone(&timezone);
        schedule
            .after(&after_local)
            .next()?
            .with_timezone(&chrono::Utc)
    } else {
        let after_local = after_utc.with_timezone(&chrono::Local);
        schedule
            .after(&after_local)
            .next()?
            .with_timezone(&chrono::Utc)
    };

    Some((next_utc, timezone_label))
}

fn cron_period_secs(context: &CronContext, cron_expr: &str) -> Option<u64> {
    let baseline = chrono::Utc::now();
    let (first, _) = next_fire_after(context, "period", cron_expr, baseline)?;
    let (second, _) = next_fire_after(context, "period", cron_expr, first)?;
    let period = (second - first).num_seconds().max(1) as u64;
    Some(period)
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
async fn run_cron_job(job: &CronJob, context: &CronContext) -> std::result::Result<(), CronRunError> {
    ensure_cron_dispatch_readiness(context, &job.id);
    let channel_id: crate::ChannelId = Arc::from(format!("cron:{}", job.id).as_str());

    // Create the outbound response channel to collect whatever the channel produces
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<RoutedResponse>(32);

    // Subscribe to the agent's event bus (the channel needs this for branch/worker events)
    let event_rx = context.deps.event_tx.subscribe();

    let (channel, channel_tx) = Channel::new(
        channel_id.clone(),
        context.deps.clone(),
        response_tx,
        event_rx,
        context.screenshot_dir.clone(),
        context.logs_dir.clone(),
        None, // cron channels don't capture prompt snapshots
        None, // cron channels don't share live transcript cache
        crate::conversation::settings::ResolvedConversationSettings::default(),
    );

    // Spawn the channel's event loop
    let channel_handle = tokio::spawn(async move { channel.run().await });

    // Send the cron job prompt as a synthetic message
    // Derive source from the delivery target's adapter so adapter_selector() can extract
    // the platform prefix (e.g., "signal" from "signal:gvoice1"). This ensures the channel
    // correctly identifies as being "from" that messaging platform and tools resolve properly.
    let source_adapter = job
        .delivery_target
        .adapter
        .split(':')
        .next()
        .unwrap_or("cron");
    let message = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: source_adapter.into(),
        adapter: Some(job.delivery_target.adapter.clone()),
        conversation_id: format!("cron:{}", job.id),
        sender_id: "system".into(),
        agent_id: Some(context.deps.agent_id.clone()),
        content: MessageContent::Text(job.prompt.clone()),
        timestamp: chrono::Utc::now(),
        metadata: HashMap::new(),
        formatted_author: None,
    };

    if let Err(error) = channel_tx.send(message).await {
        let error_message = format!("failed to send cron prompt to channel: {error}");
        persist_cron_execution(
            context,
            &job.id,
            CronExecutionRecord {
                execution_succeeded: false,
                delivery_attempted: false,
                delivery_succeeded: None,
                result_summary: None,
                execution_error: Some(error_message.clone()),
                delivery_error: None,
            },
        );
        return Err(CronRunError::Execution(anyhow::anyhow!(error_message).into()));
    }

    let timeout = Duration::from_secs(job.timeout_secs.unwrap_or(120));

    // Drop the sender so the cron channel behaves as a one-shot conversation.
    drop(channel_tx);

    let delivery_response = match await_cron_delivery_response(&job.id, &mut response_rx, timeout)
        .await
    {
        CronResponseWaitOutcome::Delivered(response) => {
            if !channel_handle.is_finished() {
                channel_handle.abort();
            }

            match channel_handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(cron_id = %job.id, %error, "cron channel returned an error after emitting a delivery response");
                }
                Err(join_error) if !join_error.is_cancelled() => {
                    tracing::warn!(cron_id = %job.id, %join_error, "cron channel join failed after emitting a delivery response");
                }
                Err(_) => {}
            }

            Some(response)
        }
        CronResponseWaitOutcome::ChannelClosed => match channel_handle.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => {
                let error_message = format!("cron channel failed: {error}");
                persist_cron_execution(
                    context,
                    &job.id,
                    CronExecutionRecord {
                        execution_succeeded: false,
                        delivery_attempted: false,
                        delivery_succeeded: None,
                        result_summary: None,
                        execution_error: Some(error_message.clone()),
                        delivery_error: None,
                    },
                );
                return Err(CronRunError::Execution(anyhow::anyhow!(error_message).into()));
            }
            Err(join_error) => {
                let error_message = format!("cron channel join failed: {join_error}");
                persist_cron_execution(
                    context,
                    &job.id,
                    CronExecutionRecord {
                        execution_succeeded: false,
                        delivery_attempted: false,
                        delivery_succeeded: None,
                        result_summary: None,
                        execution_error: Some(error_message.clone()),
                        delivery_error: None,
                    },
                );
                return Err(CronRunError::Execution(anyhow::anyhow!(error_message).into()));
            }
        },
        CronResponseWaitOutcome::TimedOut => {
            if !channel_handle.is_finished() {
                channel_handle.abort();
            }

            match channel_handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(
                        cron_id = %job.id,
                        %error,
                        "cron channel returned an error after timing out"
                    );
                }
                Err(join_error) if !join_error.is_cancelled() => {
                    tracing::warn!(
                        cron_id = %job.id,
                        %join_error,
                        "cron channel join failed after timing out"
                    );
                }
                Err(_) => {}
            }

            let error_message = format!("cron job timed out after {timeout:?}");
            persist_cron_execution(
                context,
                &job.id,
                CronExecutionRecord {
                    execution_succeeded: false,
                    delivery_attempted: false,
                    delivery_succeeded: None,
                    result_summary: None,
                    execution_error: Some(error_message.clone()),
                    delivery_error: None,
                },
            );

            return Err(CronRunError::Execution(anyhow::anyhow!(error_message).into()));
        }
    };

    // Deliver result to target (only if there's something to say)
    if let Some(response) = delivery_response {
        let summary = cron_response_summary(&response);
        if let Err(error) = context
            .messaging_manager
            .broadcast_proactive(
                &job.delivery_target.adapter,
                &job.delivery_target.target,
                response,
            )
            .await
        {
            tracing::error!(
                cron_id = %job.id,
                target = %job.delivery_target,
                %error,
                "failed to deliver cron result"
            );
            persist_cron_execution(
                context,
                &job.id,
                CronExecutionRecord {
                    execution_succeeded: true,
                    delivery_attempted: true,
                    delivery_succeeded: Some(false),
                    result_summary: summary.clone(),
                    execution_error: None,
                    delivery_error: Some(error.to_string()),
                },
            );
            return Err(CronRunError::Delivery(error));
        }

        tracing::info!(
            cron_id = %job.id,
            target = %job.delivery_target,
            "cron result delivered"
        );
        persist_cron_execution(
            context,
            &job.id,
            CronExecutionRecord {
                execution_succeeded: true,
                delivery_attempted: true,
                delivery_succeeded: Some(true),
                result_summary: summary,
                execution_error: None,
                delivery_error: None,
            },
        );
    } else {
        tracing::debug!(cron_id = %job.id, "cron job produced no output, skipping delivery");
        persist_cron_execution(
            context,
            &job.id,
            CronExecutionRecord {
                execution_succeeded: true,
                delivery_attempted: false,
                delivery_succeeded: None,
                result_summary: None,
                execution_error: None,
                delivery_error: None,
            },
        );
    }

    Ok(())
}

fn persist_cron_execution(context: &CronContext, cron_id: &str, record: CronExecutionRecord) {
    #[cfg(feature = "metrics")]
    record_cron_metrics(&context.deps.agent_id, cron_id, &record);

    let store = context.store.clone();
    let cron_id = cron_id.to_string();
    tokio::spawn(async move {
        if let Err(error) = store.log_execution(&cron_id, &record).await {
            tracing::warn!(cron_id = %cron_id, %error, "failed to log cron execution");
        }
    });
}

#[cfg(feature = "metrics")]
fn record_cron_metrics(agent_id: &str, cron_id: &str, record: &CronExecutionRecord) {
    let overall_result = if record.execution_succeeded {
        "success"
    } else {
        "failure"
    };

    let delivery_result = if !record.delivery_attempted {
        "skipped"
    } else if record.delivery_succeeded == Some(true) {
        "success"
    } else {
        "failure"
    };

    crate::telemetry::Metrics::global()
        .cron_executions_total
        .with_label_values(&[agent_id, cron_id, overall_result])
        .inc();
    crate::telemetry::Metrics::global()
        .cron_delivery_total
        .with_label_values(&[agent_id, cron_id, delivery_result])
        .inc();
}

#[derive(Debug)]
enum CronResponseWaitOutcome {
    Delivered(OutboundResponse),
    ChannelClosed,
    TimedOut,
}

async fn await_cron_delivery_response(
    cron_id: &str,
    response_rx: &mut mpsc::Receiver<RoutedResponse>,
    timeout: Duration,
) -> CronResponseWaitOutcome {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(cron_id = %cron_id, "cron job timed out after {timeout:?}");
            return CronResponseWaitOutcome::TimedOut;
        }

        match tokio::time::timeout(remaining, response_rx.recv()).await {
            Ok(Some(RoutedResponse { response, .. })) => {
                if let Some(delivery_response) = normalize_cron_delivery_response(response) {
                    return CronResponseWaitOutcome::Delivered(delivery_response);
                }
            }
            Ok(None) => return CronResponseWaitOutcome::ChannelClosed,
            Err(_) => {
                tracing::warn!(cron_id = %cron_id, "cron job timed out after {timeout:?}");
                return CronResponseWaitOutcome::TimedOut;
            }
        }
    }
}

fn normalize_cron_delivery_response(response: OutboundResponse) -> Option<OutboundResponse> {
    match response {
        OutboundResponse::Text(text) => {
            if text.trim().is_empty() {
                None
            } else {
                Some(OutboundResponse::Text(text))
            }
        }
        OutboundResponse::RichMessage {
            text,
            blocks,
            cards,
            interactive_elements,
            poll,
        } => {
            if text.trim().is_empty()
                && blocks.is_empty()
                && cards.is_empty()
                && interactive_elements.is_empty()
                && poll.is_none()
            {
                None
            } else {
                Some(OutboundResponse::RichMessage {
                    text,
                    blocks,
                    cards,
                    interactive_elements,
                    poll,
                })
            }
        }
        OutboundResponse::ThreadReply { text, .. }
        | OutboundResponse::Ephemeral { text, .. }
        | OutboundResponse::ScheduledMessage { text, .. } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(OutboundResponse::Text(text))
            }
        }
        OutboundResponse::File {
            filename,
            data,
            mime_type,
            caption,
        } => Some(OutboundResponse::File {
            filename,
            data,
            mime_type,
            caption,
        }),
        OutboundResponse::Status(_)
        | OutboundResponse::Reaction(_)
        | OutboundResponse::RemoveReaction(_)
        | OutboundResponse::StreamStart
        | OutboundResponse::StreamChunk(_)
        | OutboundResponse::StreamEnd => None,
    }
}

fn cron_response_summary(response: &OutboundResponse) -> Option<String> {
    match response {
        OutboundResponse::Text(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        OutboundResponse::RichMessage { text, cards, .. } => {
            let text = text.trim();
            if !text.is_empty() {
                Some(text.to_string())
            } else {
                let derived = OutboundResponse::text_from_cards(cards);
                let derived = derived.trim();
                (!derived.is_empty()).then(|| derived.to_string())
            }
        }
        OutboundResponse::ThreadReply { text, .. }
        | OutboundResponse::Ephemeral { text, .. }
        | OutboundResponse::ScheduledMessage { text, .. } => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        OutboundResponse::File {
            filename, caption, ..
        } => caption
            .as_deref()
            .map(str::trim)
            .filter(|caption| !caption.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| Some(format!("Attached file: {filename}"))),
        OutboundResponse::Status(_)
        | OutboundResponse::Reaction(_)
        | OutboundResponse::RemoveReaction(_)
        | OutboundResponse::StreamStart
        | OutboundResponse::StreamChunk(_)
        | OutboundResponse::StreamEnd => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CronConfig, CronJob, CronResponseWaitOutcome, await_cron_delivery_response,
        CronRunError, cron_response_summary, hour_in_active_window, normalize_active_hours,
        normalize_cron_delivery_response, set_job_enabled_state, sync_job_from_store,
    };
    use crate::cron::store::CronStore;
    use crate::messaging::target::parse_delivery_target;
    use crate::{Card, InboundMessage, OutboundResponse, RoutedResponse};
    use chrono::Timelike as _;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tokio::sync::mpsc;
    use tokio::time::Duration;

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

    #[tokio::test]
    async fn cron_wait_returns_first_delivery_response_without_waiting_for_channel_close() {
        let (response_tx, mut response_rx) = mpsc::channel(4);

        response_tx
            .send(RoutedResponse {
                response: OutboundResponse::RichMessage {
                    text: "daily digest".to_string(),
                    blocks: Vec::new(),
                    cards: vec![Card {
                        title: Some("Digest".to_string()),
                        ..Card::default()
                    }],
                    interactive_elements: Vec::new(),
                    poll: None,
                },
                target: InboundMessage::empty(),
            })
            .await
            .expect("send test response");

        let outcome =
            await_cron_delivery_response("daily-digest", &mut response_rx, Duration::from_secs(1))
                .await;

        match outcome {
            CronResponseWaitOutcome::Delivered(OutboundResponse::RichMessage {
                text,
                cards,
                ..
            }) => {
                assert_eq!(text, "daily digest");
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].title.as_deref(), Some("Digest"));
            }
            CronResponseWaitOutcome::Delivered(other) => {
                panic!("expected rich message, got {other:?}");
            }
            CronResponseWaitOutcome::ChannelClosed => {
                panic!("expected delivered response, channel closed");
            }
            CronResponseWaitOutcome::TimedOut => {
                panic!("expected delivered response, timed out");
            }
        }
    }

    #[test]
    fn cron_normalizes_thread_replies_to_text() {
        let response = normalize_cron_delivery_response(OutboundResponse::ThreadReply {
            thread_name: "ops".to_string(),
            text: "hello from cron".to_string(),
        });

        match response {
            Some(OutboundResponse::Text(text)) => assert_eq!(text, "hello from cron"),
            Some(other) => panic!("expected text response, got {other:?}"),
            None => panic!("expected normalized text response"),
        }
    }

    #[test]
    fn cron_preserves_file_responses_for_delivery() {
        let response = normalize_cron_delivery_response(OutboundResponse::File {
            filename: "digest.txt".to_string(),
            data: b"all good".to_vec(),
            mime_type: "text/plain".to_string(),
            caption: Some("nightly digest".to_string()),
        });

        match response {
            Some(OutboundResponse::File {
                filename,
                mime_type,
                caption,
                ..
            }) => {
                assert_eq!(filename, "digest.txt");
                assert_eq!(mime_type, "text/plain");
                assert_eq!(caption.as_deref(), Some("nightly digest"));
            }
            Some(other) => panic!("expected file response, got {other:?}"),
            None => panic!("expected file response"),
        }
    }

    #[test]
    fn cron_summary_uses_card_fallback_when_rich_text_is_empty() {
        let summary = cron_response_summary(&OutboundResponse::RichMessage {
            text: String::new(),
            blocks: Vec::new(),
            cards: vec![Card {
                title: Some("Digest".to_string()),
                description: Some("Everything is green.".to_string()),
                ..Card::default()
            }],
            interactive_elements: Vec::new(),
            poll: None,
        });

        assert_eq!(summary.as_deref(), Some("Digest\n\nEverything is green."));
    }

    #[test]
    fn cron_summary_uses_file_caption_or_filename() {
        let with_caption = cron_response_summary(&OutboundResponse::File {
            filename: "digest.txt".to_string(),
            data: Vec::new(),
            mime_type: "text/plain".to_string(),
            caption: Some("nightly digest".to_string()),
        });
        let without_caption = cron_response_summary(&OutboundResponse::File {
            filename: "digest.txt".to_string(),
            data: Vec::new(),
            mime_type: "text/plain".to_string(),
            caption: None,
        });

        assert_eq!(with_caption.as_deref(), Some("nightly digest"));
        assert_eq!(
            without_caption.as_deref(),
            Some("Attached file: digest.txt")
        );
    }

    async fn setup_cron_store() -> Arc<CronStore> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect sqlite memory db");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        Arc::new(CronStore::new(pool))
    }

    fn sample_cron_job(
        id: &str,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        consecutive_failures: u32,
    ) -> CronJob {
        CronJob {
            id: id.to_string(),
            prompt: "digest".to_string(),
            cron_expr: None,
            interval_secs: 300,
            delivery_target: parse_delivery_target("discord:123456789")
                .expect("parse delivery target"),
            active_hours: None,
            enabled: true,
            run_once: false,
            consecutive_failures,
            next_run_at,
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn cron_sync_job_from_store_refreshes_stale_cursor_after_lost_claim() {
        let store = setup_cron_store().await;
        let expected_next_run_at = chrono::Utc::now()
            .with_nanosecond(0)
            .expect("truncate nanos");
        let advanced_next_run_at = expected_next_run_at + chrono::Duration::minutes(5);
        let expected_text = expected_next_run_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let advanced_text = advanced_next_run_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        store
            .save(&CronConfig {
                id: "daily-digest".to_string(),
                prompt: "digest".to_string(),
                cron_expr: None,
                interval_secs: 300,
                delivery_target: "discord:123456789".to_string(),
                active_hours: None,
                enabled: true,
                run_once: false,
                next_run_at: Some(expected_text.clone()),
                timeout_secs: None,
            })
            .await
            .expect("save cron config");

        assert!(
            store
                .claim_and_advance("daily-digest", &expected_text, &advanced_text)
                .await
                .expect("advance persisted cursor")
        );

        let jobs = Arc::new(RwLock::new(HashMap::from([(
            "daily-digest".to_string(),
            sample_cron_job("daily-digest", Some(expected_next_run_at), 2),
        )])));

        sync_job_from_store(store.as_ref(), &jobs, "daily-digest")
            .await
            .expect("sync local job from store");

        let guard = jobs.read().await;
        let job = guard.get("daily-digest").expect("job remains registered");
        assert_eq!(job.next_run_at, Some(advanced_next_run_at));
        assert_eq!(job.consecutive_failures, 2);
    }

    #[test]
    fn cron_run_error_distinguishes_delivery_failures() {
        let delivery_error = CronRunError::Delivery(crate::error::Error::Other(anyhow::anyhow!(
            "adapter offline"
        )));
        let execution_error = CronRunError::Execution(crate::error::Error::Other(anyhow::anyhow!(
            "channel failed"
        )));

        assert_eq!(delivery_error.as_error().to_string(), "adapter offline");
        assert_eq!(delivery_error.failure_class(), "delivery_error");
        assert_eq!(execution_error.failure_class(), "execution_error");
    }

    #[tokio::test]
    async fn set_job_enabled_state_updates_in_memory_flag() {
        let jobs = Arc::new(RwLock::new(HashMap::from([(
            "daily-digest".to_string(),
            CronJob {
                enabled: false,
                ..sample_cron_job("daily-digest", None, 1)
            },
        )])));

        set_job_enabled_state(&jobs, "daily-digest", true)
            .await
            .expect("enable in-memory job");

        let guard = jobs.read().await;
        let job = guard.get("daily-digest").expect("job remains registered");
        assert!(job.enabled);
        assert_eq!(job.consecutive_failures, 1);
        assert_eq!(job.next_run_at, None);
    }
}
