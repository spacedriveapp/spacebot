//! Cron job management tool for creating, listing, and deleting scheduled tasks.

use crate::cron::scheduler::{CronConfig, Scheduler};
use crate::cron::store::CronStore;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

/// Minimum allowed interval between cron job runs (seconds).
const MIN_CRON_INTERVAL_SECS: u64 = 60;

/// Maximum allowed prompt length for cron jobs (characters).
const MAX_CRON_PROMPT_LENGTH: usize = 10_000;

/// Tool for managing cron jobs (scheduled recurring tasks).
#[derive(Debug, Clone)]
pub struct CronTool {
    store: Arc<CronStore>,
    scheduler: Arc<Scheduler>,
    default_delivery_target: Option<String>,
    current_adapter: Option<String>,
}

impl CronTool {
    pub fn new(store: Arc<CronStore>, scheduler: Arc<Scheduler>) -> Self {
        Self {
            store,
            scheduler,
            default_delivery_target: None,
            current_adapter: None,
        }
    }

    pub fn with_default_delivery_target(mut self, default_delivery_target: Option<String>) -> Self {
        self.default_delivery_target = default_delivery_target;
        self
    }

    pub fn with_current_adapter(mut self, current_adapter: Option<String>) -> Self {
        self.current_adapter = current_adapter;
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Cron operation failed: {0}")]
pub struct CronError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CronArgs {
    /// The operation to perform: "create", "list", or "delete".
    pub action: String,
    /// Required for "create": a short unique ID for the cron job (e.g. "check-email", "daily-summary").
    #[serde(default)]
    pub id: Option<String>,
    /// Required for "create": the prompt/instruction to execute on each run.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional for "create": strict wall-clock cron expression (5-field syntax).
    /// When provided, this takes precedence over interval-based scheduling.
    #[serde(default)]
    pub cron_expr: Option<String>,
    /// Required for "create": interval in seconds between runs.
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Optional for "create": where to deliver results, in "adapter:target" format (e.g. "discord:123456789"). If omitted, defaults to the current conversation when available.
    #[serde(default)]
    pub delivery_target: Option<String>,
    /// Optional for "create": hour (0-23) when the job becomes active.
    #[serde(default)]
    pub active_start_hour: Option<u8>,
    /// Optional for "create": hour (0-23) when the job becomes inactive.
    #[serde(default)]
    pub active_end_hour: Option<u8>,
    /// Required for "delete": the ID of the cron job to remove.
    #[serde(default)]
    pub delete_id: Option<String>,
    /// Optional for "create": maximum seconds to wait for the job to complete before timing out.
    /// Defaults to 120. Use a larger value (e.g. 600) for long-running research or writing tasks.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Optional for "create": if true, run only once and disable after first execution attempt.
    #[serde(default)]
    pub run_once: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CronOutput {
    pub success: bool,
    pub message: String,
    /// Populated on "list" action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs: Option<Vec<CronEntry>>,
}

#[derive(Debug, Serialize)]
pub struct CronEntry {
    pub id: String,
    pub prompt: String,
    pub cron_expr: Option<String>,
    pub interval_secs: u64,
    pub delivery_target: String,
    pub run_once: bool,
    pub active_hours: Option<String>,
}

impl Tool for CronTool {
    const NAME: &'static str = "cron";

    type Error = CronError;
    type Args = CronArgs;
    type Output = CronOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/cron").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "list", "delete"],
                        "description": "The operation: create a new cron job, list all cron jobs, or delete one."
                    },
                    "id": {
                        "type": "string",
                        "description": "For 'create': a short unique ID (e.g. 'check-email', 'daily-summary')."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "For 'create': the instruction to execute on each run."
                    },
                    "cron_expr": {
                        "type": "string",
                        "description": "For 'create': strict wall-clock schedule in cron format (e.g. '0 9 * * *' for daily at 09:00)."
                    },
                    "interval_secs": {
                        "type": "integer",
                        "description": "DEPRECATED — legacy fallback only. Use `cron_expr` instead. Interval scheduling drifts and does not align to wall-clock times."
                    },
                    "delivery_target": {
                        "type": "string",
                        "description": "For 'create': where to send results, format 'adapter:target' (e.g. 'discord:dm:123456789' for DM, 'discord:channel_id' for server). If omitted, defaults to the current conversation."
                    },
                    "active_start_hour": {
                        "type": "integer",
                        "description": "For 'create': optional start of active window (0-23, 24h format)."
                    },
                    "active_end_hour": {
                        "type": "integer",
                        "description": "For 'create': optional end of active window (0-23, 24h format)."
                    },
                    "delete_id": {
                        "type": "string",
                        "description": "For 'delete': the ID of the cron job to remove."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "For 'create': max seconds to wait for the job to finish (default 120). Use 600 for long-running tasks like research."
                    },
                    "run_once": {
                        "type": "boolean",
                        "description": "For 'create': if true, run this job once and auto-disable after the first execution attempt."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action.as_str() {
            "create" => self.create(args).await,
            "list" => self.list().await,
            "delete" => self.delete(args).await,
            other => Ok(CronOutput {
                success: false,
                message: format!("Unknown action '{other}'. Use 'create', 'list', or 'delete'."),
                jobs: None,
            }),
        }
    }
}

impl CronTool {
    async fn create(&self, args: CronArgs) -> Result<CronOutput, CronError> {
        let id = args
            .id
            .ok_or_else(|| CronError("'id' is required for create".into()))?;
        let prompt = args
            .prompt
            .ok_or_else(|| CronError("'prompt' is required for create".into()))?;
        let cron_expr = args
            .cron_expr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let interval_secs = args.interval_secs.unwrap_or(3600);
        let delivery_target = args
            .delivery_target
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .or_else(|| self.default_delivery_target.clone())
            .ok_or_else(|| {
                CronError(
                    "'delivery_target' is required for create when no conversation default is available"
                        .into(),
                )
            })?;

        // Normalize delivery target for named instances
        let delivery_target = normalize_delivery_target(&delivery_target, &self.current_adapter);

        // Validate cron job ID: alphanumeric, hyphens, underscores only
        if id.is_empty()
            || id.len() > 50
            || !id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(CronError(
                "'id' must be 1-50 characters, alphanumeric with hyphens and underscores only"
                    .into(),
            ));
        }

        // Prevent excessively short intervals that could cause resource exhaustion
        if cron_expr.is_none() && interval_secs < MIN_CRON_INTERVAL_SECS {
            return Err(CronError(format!(
                "'interval_secs' must be at least {MIN_CRON_INTERVAL_SECS} (got {interval_secs})"
            )));
        }

        if let Some(expr) = cron_expr.as_deref() {
            let field_count = expr.split_whitespace().count();
            if field_count != 5 {
                return Err(CronError(format!(
                    "'cron_expr' must have exactly 5 fields (got {field_count}): '{expr}'"
                )));
            }
            // The `cron` crate uses 7-field expressions (sec min hour dom month dow year).
            // Users write standard 5-field cron (min hour dom month dow). Expand before parsing.
            let expanded = format!("0 {expr} *");
            cron::Schedule::from_str(&expanded)
                .map_err(|error| CronError(format!("invalid 'cron_expr' '{expr}': {error}")))?;
        }

        // Cap prompt length to prevent context flooding
        if prompt.len() > MAX_CRON_PROMPT_LENGTH {
            return Err(CronError(format!(
                "'prompt' exceeds maximum length of {MAX_CRON_PROMPT_LENGTH} characters (got {})",
                prompt.len()
            )));
        }

        // Validate delivery_target format (must be "adapter:target")
        if !delivery_target.contains(':') {
            return Err(CronError(
                "'delivery_target' must be in 'adapter:target' format (e.g. 'discord:123456789')"
                    .into(),
            ));
        }

        let active_hours = match (args.active_start_hour, args.active_end_hour) {
            (Some(start), Some(end)) => {
                if start > 23 || end > 23 {
                    return Err(CronError("active hours must be 0-23".into()));
                }
                Some((start, end))
            }
            _ => None,
        };
        let run_once = args.run_once.unwrap_or(false);

        let config = CronConfig {
            id: id.clone(),
            prompt: prompt.clone(),
            cron_expr: cron_expr.clone(),
            interval_secs,
            delivery_target: delivery_target.clone(),
            active_hours,
            enabled: true,
            run_once,
            timeout_secs: args.timeout_secs,
        };

        // Persist to database
        self.store
            .save(&config)
            .await
            .map_err(|error| CronError(format!("failed to save: {error}")))?;

        // Register with the running scheduler so it starts immediately
        self.scheduler
            .register(config)
            .await
            .map_err(|error| CronError(format!("failed to register: {error}")))?;

        let schedule_desc = cron_expr
            .as_deref()
            .map(|expr| format!("on schedule `{expr}`"))
            .unwrap_or_else(|| format_interval(interval_secs));
        let timezone = self.scheduler.cron_timezone_label();
        let mut message = if run_once {
            format!("Cron job '{id}' created. First run {schedule_desc}; it then disables itself.")
        } else {
            format!("Cron job '{id}' created. Runs {schedule_desc}.")
        };
        if let Some((start, end)) = active_hours {
            if timezone == "system" {
                message.push_str(&format!(
                    " Active {start:02}:00-{end:02}:00 in server local time."
                ));
            } else {
                message.push_str(&format!(" Active {start:02}:00-{end:02}:00 in {timezone}."));
            }
        } else if timezone == "system" {
            message.push_str(" Active-hours timezone: server local time.");
        } else {
            message.push_str(&format!(" Active-hours timezone: {timezone}."));
        }

        tracing::info!(cron_id = %id, %interval_secs, %delivery_target, "cron job created via tool");

        Ok(CronOutput {
            success: true,
            message,
            jobs: None,
        })
    }

    async fn list(&self) -> Result<CronOutput, CronError> {
        let configs = self
            .store
            .load_all()
            .await
            .map_err(|error| CronError(format!("failed to list: {error}")))?;

        let entries: Vec<CronEntry> = configs
            .into_iter()
            .map(|config| CronEntry {
                id: config.id,
                prompt: config.prompt,
                cron_expr: config.cron_expr,
                interval_secs: config.interval_secs,
                delivery_target: config.delivery_target,
                run_once: config.run_once,
                active_hours: config
                    .active_hours
                    .map(|(s, e)| format!("{s:02}:00-{e:02}:00")),
            })
            .collect();

        let count = entries.len();
        let timezone = self.scheduler.cron_timezone_label();
        let timezone_note = if timezone == "system" {
            "active hours use server local time".to_string()
        } else {
            format!("active hours use {timezone}")
        };
        Ok(CronOutput {
            success: true,
            message: format!("{count} active cron job(s); {timezone_note}."),
            jobs: Some(entries),
        })
    }

    async fn delete(&self, args: CronArgs) -> Result<CronOutput, CronError> {
        let id = args
            .delete_id
            .or(args.id)
            .ok_or_else(|| CronError("'delete_id' or 'id' is required for delete".into()))?;

        self.scheduler.unregister(&id).await;

        self.store
            .delete(&id)
            .await
            .map_err(|error| CronError(format!("failed to delete: {error}")))?;

        tracing::info!(cron_id = %id, "cron job deleted via tool");

        Ok(CronOutput {
            success: true,
            message: format!("Cron job '{id}' deleted."),
            jobs: None,
        })
    }
}

fn format_interval(secs: u64) -> String {
    if secs.is_multiple_of(86400) {
        let days = secs / 86400;
        if days == 1 {
            "every day".into()
        } else {
            format!("every {days} days")
        }
    } else if secs.is_multiple_of(3600) {
        let hours = secs / 3600;
        if hours == 1 {
            "every hour".into()
        } else {
            format!("every {hours} hours")
        }
    } else if secs.is_multiple_of(60) {
        let minutes = secs / 60;
        if minutes == 1 {
            "every minute".into()
        } else {
            format!("every {minutes} minutes")
        }
    } else {
        format!("every {secs} seconds")
    }
}

/// Normalize delivery target for named instances.
///
/// If the LLM provided a bare platform adapter (e.g., "signal", "slack") but we're in
/// a named instance conversation (e.g., "signal:gvoice1", "slack:work"), rewrite to
/// include the instance name. This ensures the cron job can find the correct adapter
/// at runtime.
fn normalize_delivery_target(delivery_target: &str, current_adapter: &Option<String>) -> String {
    if let Some(parsed) = crate::messaging::target::parse_delivery_target(delivery_target)
        && let Some(current_adapter) = current_adapter.as_ref()
    {
        let expected_prefix = format!("{}:", parsed.adapter);
        if current_adapter.starts_with(&expected_prefix) {
            // current_adapter is a named instance of the parsed platform
            return format!("{current_adapter}:{}", parsed.target);
        }
    }
    delivery_target.to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_delivery_target;

    #[test]
    fn test_normalize_signal_named_instance() {
        // Bare signal + named instance context → rewrite
        let result = normalize_delivery_target(
            "signal:uuid:550e8400-e29b-41d4-a716-446655440000",
            &Some("signal:gvoice1".to_string()),
        );
        assert_eq!(
            result,
            "signal:gvoice1:uuid:550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn test_normalize_signal_group_named_instance() {
        // Signal group + named instance context → rewrite
        let result =
            normalize_delivery_target("signal:group:grp123", &Some("signal:work".to_string()));
        assert_eq!(result, "signal:work:group:grp123");
    }

    #[test]
    fn test_normalize_signal_default_instance_no_rewrite() {
        // Bare signal + default signal context → no change
        let result = normalize_delivery_target(
            "signal:uuid:550e8400-e29b-41d4-a716-446655440000",
            &Some("signal".to_string()),
        );
        assert_eq!(result, "signal:uuid:550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_normalize_signal_no_context_no_rewrite() {
        // Bare signal + no context → no change
        let result =
            normalize_delivery_target("signal:uuid:550e8400-e29b-41d4-a716-446655440000", &None);
        assert_eq!(result, "signal:uuid:550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_normalize_slack_named_instance() {
        // Bare slack + named instance context → rewrite
        let result = normalize_delivery_target("slack:C123456", &Some("slack:work".to_string()));
        assert_eq!(result, "slack:work:C123456");
    }

    #[test]
    fn test_normalize_discord_named_instance() {
        // Bare discord + named instance context → rewrite
        let result =
            normalize_delivery_target("discord:987654321", &Some("discord:personal".to_string()));
        assert_eq!(result, "discord:personal:987654321");
    }

    #[test]
    fn test_normalize_telegram_named_instance() {
        // Bare telegram + named instance context → rewrite
        let result =
            normalize_delivery_target("telegram:-1001234", &Some("telegram:bot1".to_string()));
        assert_eq!(result, "telegram:bot1:-1001234");
    }

    #[test]
    fn test_normalize_different_platform_no_rewrite() {
        // Signal target but in Discord context → no change
        let result = normalize_delivery_target(
            "signal:uuid:550e8400-e29b-41d4-a716-446655440000",
            &Some("discord:general".to_string()),
        );
        assert_eq!(result, "signal:uuid:550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_normalize_cron_context_no_rewrite() {
        // Any target in cron context → no change (cron can't receive delivery)
        let result = normalize_delivery_target(
            "signal:uuid:550e8400-e29b-41d4-a716-446655440000",
            &Some("cron".to_string()),
        );
        assert_eq!(result, "signal:uuid:550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_normalize_already_qualified_no_rewrite() {
        // Already qualified with instance name → no change
        let result = normalize_delivery_target(
            "signal:gvoice1:uuid:550e8400-e29b-41d4-a716-446655440000",
            &Some("signal:gvoice1".to_string()),
        );
        assert_eq!(
            result,
            "signal:gvoice1:uuid:550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn test_normalize_email_no_rewrite() {
        // Email doesn't use named instances → no change
        let result =
            normalize_delivery_target("email:alice@example.com", &Some("email".to_string()));
        assert_eq!(result, "email:alice@example.com");
    }
}
