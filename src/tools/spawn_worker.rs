//! Spawn worker tool for creating new workers.

use crate::WorkerId;
use crate::agent::channel::{
    ChannelState, spawn_acp_worker_from_state, spawn_opencode_worker_from_state,
    spawn_worker_from_state,
};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for spawning workers.
#[derive(Debug, Clone)]
pub struct SpawnWorkerTool {
    state: ChannelState,
}

impl SpawnWorkerTool {
    /// Create a new spawn worker tool with access to channel state.
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

/// Error type for spawn worker tool.
#[derive(Debug, thiserror::Error)]
#[error("Worker spawn failed: {0}")]
pub struct SpawnWorkerError(String);

/// Arguments for spawn worker tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnWorkerArgs {
    /// The task description for the worker.
    pub task: String,
    /// Whether this is an interactive worker (accepts follow-up messages).
    #[serde(default)]
    pub interactive: bool,
    /// Optional list of skill names to suggest to the worker. The worker sees
    /// all available skills and can read any of them via read_skill, but
    /// suggested skills are flagged as recommended for this task.
    #[serde(default)]
    pub suggested_skills: Vec<String>,
    /// Worker type: "builtin" (default) runs a Rig agent loop with shell/file/exec
    /// tools. "opencode" spawns an OpenCode subprocess with full coding agent
    /// capabilities. "acp" spawns an Agent Client Protocol worker.
    #[serde(default)]
    pub worker_type: Option<String>,
    /// Working directory for the worker. Required for "opencode" workers.
    /// The OpenCode agent will operate in this directory.
    #[serde(default)]
    pub directory: Option<String>,
    /// ACP worker id from [defaults.acp.<id>] when worker_type is "acp".
    #[serde(default)]
    pub acp_id: Option<String>,
}

/// Output from spawn worker tool.
#[derive(Debug, Serialize)]
pub struct SpawnWorkerOutput {
    /// The ID of the spawned worker.
    pub worker_id: WorkerId,
    /// Whether the worker was spawned successfully.
    pub spawned: bool,
    /// Whether this is an interactive worker.
    pub interactive: bool,
    /// Status message.
    pub message: String,
}

impl Tool for SpawnWorkerTool {
    const NAME: &'static str = "spawn_worker";

    type Error = SpawnWorkerError;
    type Args = SpawnWorkerArgs;
    type Output = SpawnWorkerOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let rc = &self.state.deps.runtime_config;
        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();
        let opencode_enabled = rc.opencode.load().enabled;
        let acp_enabled = rc
            .acp
            .load()
            .values()
            .any(|cfg| cfg.enabled && !cfg.command.trim().is_empty());

        let mut tools_list = vec!["shell", "file", "exec"];
        if browser_enabled {
            tools_list.push("browser");
        }
        if web_search_enabled {
            tools_list.push("web_search");
        }

        let opencode_note = if opencode_enabled {
            " Set worker_type to \"opencode\" with a directory path for complex coding tasks — this spawns a full OpenCode coding agent with codebase exploration, context management, and its own tool suite."
        } else {
            ""
        };
        let acp_note = if acp_enabled {
            " Set worker_type to \"acp\" with a directory path for ACP-based coding tasks. Optionally provide acp_id to select a specific ACP worker from defaults.acp."
        } else {
            ""
        };

        let base_description = crate::prompts::text::get("tools/spawn_worker");
        let description = base_description
            .replace("{tools}", &tools_list.join(", "))
            .replace("{opencode_note}", opencode_note)
            .replace("{acp_note}", acp_note);

        let mut properties = serde_json::json!({
            "task": {
                "type": "string",
                "description": "Clear, specific description of what the worker should do. Include all context needed since the worker can't see your conversation."
            },
            "interactive": {
                "type": "boolean",
                "default": false,
                "description": "If true, the worker stays alive and accepts follow-up messages via route_to_worker. If false (default), the worker runs once and returns."
            },
            "suggested_skills": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Skill names from <available_skills> that are likely relevant to this task. The worker sees all skills and decides what to read, but suggested skills are flagged as recommended."
            }
        });

        if (opencode_enabled || acp_enabled)
            && let Some(obj) = properties.as_object_mut()
        {
            let worker_type_enum = if opencode_enabled && acp_enabled {
                serde_json::json!(["builtin", "opencode", "acp"])
            } else if opencode_enabled {
                serde_json::json!(["builtin", "opencode"])
            } else {
                serde_json::json!(["builtin", "acp"])
            };
            obj.insert(
                "worker_type".to_string(),
                serde_json::json!({
                    "type": "string",
                    "enum": worker_type_enum,
                    "default": "builtin",
                    "description": "\"builtin\" (default) runs a Rig agent loop. \"opencode\" spawns a full OpenCode coding agent. \"acp\" spawns an ACP-backed coding agent."
                }),
            );
            obj.insert(
                "directory".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Working directory for the worker. Required when worker_type is \"opencode\" or \"acp\"."
                }),
            );

            if acp_enabled {
                obj.insert(
                    "acp_id".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Optional ACP worker id from defaults.acp.<id>. Recommended when multiple ACP workers are configured."
                    }),
                );
            }
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["task"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let readiness = self.state.deps.runtime_config.work_readiness();
        let worker_type = args.worker_type.as_deref().unwrap_or("builtin");

        let worker_id = match worker_type {
            "opencode" => {
                let directory = args.directory.as_deref().ok_or_else(|| {
                    SpawnWorkerError("directory is required for opencode workers".into())
                })?;

                spawn_opencode_worker_from_state(
                    &self.state,
                    &args.task,
                    directory,
                    args.interactive,
                )
                .await
                .map_err(|e| SpawnWorkerError(format!("{e}")))?
            }
            "acp" => {
                let directory = args.directory.as_deref().ok_or_else(|| {
                    SpawnWorkerError("directory is required for acp workers".into())
                })?;

                spawn_acp_worker_from_state(
                    &self.state,
                    &args.task,
                    directory,
                    args.acp_id.as_deref(),
                    args.interactive,
                )
                .await
                .map_err(|e| SpawnWorkerError(format!("{e}")))?
            }
            "builtin" => spawn_worker_from_state(
                &self.state,
                &args.task,
                args.interactive,
                &args
                    .suggested_skills
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| SpawnWorkerError(format!("{e}")))?,
            other => {
                return Err(SpawnWorkerError(format!(
                    "unsupported worker_type '{other}'"
                )));
            }
        };

        let worker_type_label = if worker_type == "opencode" {
            "OpenCode"
        } else if worker_type == "acp" {
            "ACP"
        } else {
            "builtin"
        };
        let message = if args.interactive {
            format!(
                "Interactive {worker_type_label} worker {worker_id} spawned for: {}. Route follow-ups with route_to_worker.",
                args.task
            )
        } else {
            format!(
                "{worker_type_label} worker {worker_id} spawned for: {}. It will report back when done.",
                args.task
            )
        };
        let readiness_note = if readiness.ready {
            String::new()
        } else {
            let reason = readiness
                .reason
                .map(|value| value.as_str())
                .unwrap_or("unknown");
            format!(
                " Readiness note: warmup is not fully ready ({reason}, state: {:?}); a warmup pass may already be running or was queued in the background.",
                readiness.warmup_state
            )
        };

        Ok(SpawnWorkerOutput {
            worker_id,
            spawned: true,
            interactive: args.interactive,
            message: format!("{message}{readiness_note}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SpawnWorkerArgs;

    #[test]
    fn deserialize_acp_worker_args() {
        let value = serde_json::json!({
            "task": "implement feature",
            "worker_type": "acp",
            "directory": "/tmp/project",
            "acp_id": "claude",
            "interactive": true,
            "suggested_skills": ["rust", "testing"]
        });

        let args: SpawnWorkerArgs = serde_json::from_value(value).expect("valid args");
        assert_eq!(args.task, "implement feature");
        assert_eq!(args.worker_type.as_deref(), Some("acp"));
        assert_eq!(args.directory.as_deref(), Some("/tmp/project"));
        assert_eq!(args.acp_id.as_deref(), Some("claude"));
        assert!(args.interactive);
        assert_eq!(args.suggested_skills, vec!["rust", "testing"]);
    }

    #[test]
    fn deserialize_defaults_for_worker_args() {
        let value = serde_json::json!({
            "task": "quick check"
        });

        let args: SpawnWorkerArgs = serde_json::from_value(value).expect("valid args");
        assert_eq!(args.task, "quick check");
        assert!(!args.interactive);
        assert!(args.suggested_skills.is_empty());
        assert!(args.worker_type.is_none());
        assert!(args.directory.is_none());
        assert!(args.acp_id.is_none());
    }
}
