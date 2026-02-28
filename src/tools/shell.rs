//! Shell tool for executing shell commands (task workers only).

use crate::sandbox::Sandbox;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

/// Tool for executing shell commands within a sandboxed environment.
#[derive(Debug, Clone)]
pub struct ShellTool {
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
    max_timeout_seconds: u64,
}

impl ShellTool {
    /// Create a new shell tool with sandbox containment.
    pub fn new(workspace: PathBuf, sandbox: Arc<Sandbox>) -> Self {
        Self::with_max_timeout(
            workspace,
            sandbox,
            crate::config::WorkerConfig::default().max_tool_timeout_secs,
        )
    }

    /// Create a new shell tool with a configurable max per-call timeout cap.
    pub fn with_max_timeout(
        workspace: PathBuf,
        sandbox: Arc<Sandbox>,
        max_timeout_seconds: u64,
    ) -> Self {
        Self {
            workspace,
            sandbox,
            max_timeout_seconds: max_timeout_seconds.max(1),
        }
    }
}

/// Error type for shell tool.
#[derive(Debug, thiserror::Error)]
#[error("Shell command failed: {message}")]
pub struct ShellError {
    message: String,
    exit_code: i32,
}

/// Arguments for shell tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// The shell command to execute.
    pub command: String,
    /// Optional working directory for the command.
    pub working_dir: Option<String>,
    /// Optional timeout in seconds (default: 60).
    #[serde(
        default = "default_timeout",
        deserialize_with = "crate::tools::deserialize_string_or_u64"
    )]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    60
}

/// Output from shell tool.
#[derive(Debug, Serialize)]
pub struct ShellOutput {
    /// Whether the command succeeded.
    pub success: bool,
    /// The exit code (0 for success).
    pub exit_code: i32,
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Formatted summary for LLM consumption.
    pub summary: String,
}

impl Tool for ShellTool {
    const NAME: &'static str = "shell";

    type Error = ShellError;
    type Args = ShellArgs;
    type Output = ShellOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let timeout_default = default_timeout().min(self.max_timeout_seconds).max(1);
        let timeout_description = format!(
            "Maximum time to wait for the command to complete (1-{} seconds)",
            self.max_timeout_seconds
        );

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/shell").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute. This will be run with sh -c on Unix or cmd /C on Windows."
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory where the command should run"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.max_timeout_seconds,
                        "default": timeout_default,
                        "description": timeout_description
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate working_dir stays within workspace if specified
        let working_dir = if let Some(ref dir) = args.working_dir {
            let path = std::path::Path::new(dir);
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let workspace_canonical = self
                .workspace
                .canonicalize()
                .unwrap_or_else(|_| self.workspace.clone());
            if !canonical.starts_with(&workspace_canonical) {
                return Err(ShellError {
                    message: format!(
                        "working_dir must be within the workspace ({}).",
                        self.workspace.display()
                    ),
                    exit_code: -1,
                });
            }
            canonical
        } else {
            self.workspace.clone()
        };

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&args.command);
            c.current_dir(&working_dir);
            c
        } else {
            self.sandbox
                .wrap("sh", &["-c", &args.command], &working_dir)
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let effective_timeout_seconds = args.timeout_seconds.min(self.max_timeout_seconds).max(1);
        let timeout = tokio::time::Duration::from_secs(effective_timeout_seconds);

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ShellError {
                message: format!("Command timed out after {effective_timeout_seconds}s"),
                exit_code: -1,
            })?
            .map_err(|e| ShellError {
                message: format!("Failed to execute command: {e}"),
                exit_code: -1,
            })?;

        let stdout = crate::tools::truncate_output(
            &String::from_utf8_lossy(&output.stdout),
            crate::tools::MAX_TOOL_OUTPUT_BYTES,
        );
        let stderr = crate::tools::truncate_output(
            &String::from_utf8_lossy(&output.stderr),
            crate::tools::MAX_TOOL_OUTPUT_BYTES,
        );
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        let summary = format_shell_output(exit_code, &stdout, &stderr);

        Ok(ShellOutput {
            success,
            exit_code,
            stdout,
            stderr,
            summary,
        })
    }
}

/// Format shell output for display.
fn format_shell_output(exit_code: i32, stdout: &str, stderr: &str) -> String {
    let mut output = String::new();

    output.push_str(&format!("Exit code: {}\n", exit_code));

    if !stdout.is_empty() {
        output.push_str("\n--- STDOUT ---\n");
        output.push_str(stdout);
    }

    if !stderr.is_empty() {
        output.push_str("\n--- STDERR ---\n");
        output.push_str(stderr);
    }

    if stdout.is_empty() && stderr.is_empty() {
        output.push_str("\n[No output]\n");
    }

    output
}

/// System-internal shell execution that bypasses path restrictions.
/// Used by the system itself, not LLM-facing.
pub async fn shell(
    command: &str,
    working_dir: Option<&std::path::Path>,
) -> crate::error::Result<ShellResult> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = tokio::time::timeout(tokio::time::Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| crate::error::AgentError::Other(anyhow::anyhow!("Command timed out")))?
        .map_err(|e| {
            crate::error::AgentError::Other(anyhow::anyhow!("Failed to execute command: {e}"))
        })?;

    Ok(ShellResult {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Result of a shell command execution.
#[derive(Debug, Clone)]
pub struct ShellResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ShellResult {
    /// Format as a readable string for LLM consumption.
    pub fn format(&self) -> String {
        format_shell_output(self.exit_code, &self.stdout, &self.stderr)
    }
}
