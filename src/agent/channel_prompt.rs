//! System prompt assembly and temporal context for channels.
//!
//! Contains `TemporalContext` for timezone-aware timestamps and
//! all the prompt-building methods that assemble the channel's
//! system prompt from identity, memory bulletin, skills, status, etc.

use crate::error::Result;
use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;

/// Debounce window for retriggers: coalesce rapid branch/worker completions
/// into a single retrigger instead of firing one per event.
pub(crate) const RETRIGGER_DEBOUNCE_MS: u64 = 500;

/// Maximum retriggers allowed since the last real user message. Prevents
/// infinite retrigger cascades where each retrigger spawns more work.
pub(crate) const MAX_RETRIGGERS_PER_TURN: usize = 3;

/// Max LLM turns for retrigger relay. Retriggers are simple relay tasks —
/// the LLM just needs to call the reply tool once. A low cap avoids wasting
/// tokens on retries when the model struggles with the retrigger format.
pub(crate) const RETRIGGER_MAX_TURNS: usize = 3;

#[derive(Debug, Clone)]
pub(crate) enum TemporalTimezone {
    Named { timezone_name: String, timezone: Tz },
    SystemLocal,
}

#[derive(Debug, Clone)]
pub(crate) struct TemporalContext {
    pub(crate) now_utc: DateTime<Utc>,
    pub(crate) timezone: TemporalTimezone,
}

impl TemporalContext {
    pub(crate) fn from_runtime(runtime_config: &crate::config::RuntimeConfig) -> Self {
        let now_utc = Utc::now();
        let user_timezone = runtime_config.user_timezone.load().as_ref().clone();
        let cron_timezone = runtime_config.cron_timezone.load().as_ref().clone();

        Self {
            now_utc,
            timezone: Self::resolve_timezone_from_names(user_timezone, cron_timezone),
        }
    }

    pub(crate) fn resolve_timezone_from_names(
        user_timezone: Option<String>,
        cron_timezone: Option<String>,
    ) -> TemporalTimezone {
        if let Some(timezone_name) = user_timezone {
            match timezone_name.parse::<Tz>() {
                Ok(timezone) => {
                    return TemporalTimezone::Named {
                        timezone_name,
                        timezone,
                    };
                }
                Err(_) => {
                    let cron_timezone_candidate =
                        cron_timezone.as_deref().unwrap_or("none configured");
                    tracing::warn!(
                        timezone = %timezone_name,
                        cron_timezone = %cron_timezone_candidate,
                        "invalid runtime timezone for channel temporal context, will try cron_timezone then fall back to system local"
                    );
                }
            }
        }

        if let Some(timezone_name) = cron_timezone {
            match timezone_name.parse::<Tz>() {
                Ok(timezone) => {
                    return TemporalTimezone::Named {
                        timezone_name,
                        timezone,
                    };
                }
                Err(error) => {
                    tracing::warn!(
                        timezone = %timezone_name,
                        error = %error,
                        "invalid cron_timezone for channel temporal context, falling back to system local"
                    );
                }
            }
        }

        TemporalTimezone::SystemLocal
    }

    pub(crate) fn format_timestamp(&self, timestamp: DateTime<Utc>) -> String {
        match &self.timezone {
            TemporalTimezone::Named {
                timezone_name,
                timezone,
            } => {
                let local_timestamp = timestamp.with_timezone(timezone);
                format!(
                    "{} ({}, UTC{})",
                    local_timestamp.format("%Y-%m-%d %H:%M:%S %Z"),
                    timezone_name,
                    local_timestamp.format("%:z")
                )
            }
            TemporalTimezone::SystemLocal => {
                let local_timestamp = timestamp.with_timezone(&Local);
                format!(
                    "{} (system local, UTC{})",
                    local_timestamp.format("%Y-%m-%d %H:%M:%S %Z"),
                    local_timestamp.format("%:z")
                )
            }
        }
    }

    pub(crate) fn current_time_line(&self) -> String {
        format!(
            "{}; UTC {}",
            self.format_timestamp(self.now_utc),
            self.now_utc.format("%Y-%m-%d %H:%M:%S UTC")
        )
    }

    pub(crate) fn worker_task_preamble(
        &self,
        prompt_engine: &crate::prompts::PromptEngine,
    ) -> Result<String> {
        let local_time = self.format_timestamp(self.now_utc);
        let utc_time = self.now_utc.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        prompt_engine.render_system_worker_time_context(&local_time, &utc_time)
    }
}

pub(crate) fn build_worker_task_with_temporal_context(
    task: &str,
    temporal_context: &TemporalContext,
    prompt_engine: &crate::prompts::PromptEngine,
) -> Result<String> {
    let preamble = temporal_context.worker_task_preamble(prompt_engine)?;
    Ok(format!("{preamble}\n\n{task}"))
}
