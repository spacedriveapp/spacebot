//! Cron job management tool for creating, listing, and deleting scheduled tasks.

use crate::cron::scheduler::{CronConfig, Scheduler};
use crate::cron::store::CronStore;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for managing cron jobs (scheduled recurring tasks).
#[derive(Debug, Clone)]
pub struct CronTool {
    store: Arc<CronStore>,
    scheduler: Arc<Scheduler>,
}

impl CronTool {
    pub fn new(store: Arc<CronStore>, scheduler: Arc<Scheduler>) -> Self {
        Self { store, scheduler }
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
    /// Required for "create": interval in seconds between runs.
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Required for "create": where to deliver results, in "adapter:target" format (e.g. "discord:123456789").
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
    pub interval_secs: u64,
    pub delivery_target: String,
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
                    "interval_secs": {
                        "type": "integer",
                        "description": "For 'create': seconds between runs (e.g. 3600 = hourly, 86400 = daily)."
                    },
                    "delivery_target": {
                        "type": "string",
                        "description": "For 'create': where to send results, format 'adapter:target' (e.g. 'discord:123456789')."
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
        let interval_secs = args
            .interval_secs
            .ok_or_else(|| CronError("'interval_secs' is required for create".into()))?;
        let delivery_target = args
            .delivery_target
            .ok_or_else(|| CronError("'delivery_target' is required for create".into()))?;

        let active_hours = match (args.active_start_hour, args.active_end_hour) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        };

        let config = CronConfig {
            id: id.clone(),
            prompt: prompt.clone(),
            interval_secs,
            delivery_target: delivery_target.clone(),
            active_hours,
            enabled: true,
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

        let interval_desc = format_interval(interval_secs);
        let mut message = format!("Cron job '{id}' created. Runs {interval_desc}.");
        if let Some((start, end)) = active_hours {
            message.push_str(&format!(" Active {start:02}:00-{end:02}:00."));
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
                interval_secs: config.interval_secs,
                delivery_target: config.delivery_target,
                active_hours: config
                    .active_hours
                    .map(|(s, e)| format!("{s:02}:00-{e:02}:00")),
            })
            .collect();

        let count = entries.len();
        Ok(CronOutput {
            success: true,
            message: format!("{count} active cron job(s)."),
            jobs: Some(entries),
        })
    }

    async fn delete(&self, args: CronArgs) -> Result<CronOutput, CronError> {
        let id = args
            .delete_id
            .or(args.id)
            .ok_or_else(|| CronError("'delete_id' or 'id' is required for delete".into()))?;

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
    if secs % 86400 == 0 {
        let days = secs / 86400;
        if days == 1 {
            "every day".into()
        } else {
            format!("every {days} days")
        }
    } else if secs % 3600 == 0 {
        let hours = secs / 3600;
        if hours == 1 {
            "every hour".into()
        } else {
            format!("every {hours} hours")
        }
    } else if secs % 60 == 0 {
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
