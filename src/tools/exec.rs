//! Exec tool for running subprocesses (task workers only).

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Tool for executing subprocesses, with path restrictions to prevent
/// access to instance-level configuration and secrets.
#[derive(Debug, Clone)]
pub struct ExecTool {
    instance_dir: PathBuf,
    workspace: PathBuf,
}

impl ExecTool {
    /// Create a new exec tool with the given instance directory for path blocking.
    pub fn new(instance_dir: PathBuf, workspace: PathBuf) -> Self {
        Self {
            instance_dir,
            workspace,
        }
    }

    /// Check if program arguments reference sensitive instance paths.
    fn check_args(&self, program: &str, args: &[String]) -> Result<(), ExecError> {
        let instance_str = self.instance_dir.to_string_lossy();
        let workspace_str = self.workspace.to_string_lossy();
        let all_args = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");

        // Block references to instance dir (unless targeting workspace within it)
        if all_args.contains(instance_str.as_ref()) && !all_args.contains(workspace_str.as_ref()) {
            return Err(ExecError {
                message: "Cannot access the instance directory — it contains protected configuration and data.".to_string(),
                exit_code: -1,
            });
        }

        // Block references to sensitive files by name
        for file in super::shell::SENSITIVE_FILES {
            if all_args.contains(file) {
                if all_args.contains(instance_str.as_ref())
                    || !all_args.contains(workspace_str.as_ref())
                {
                    return Err(ExecError {
                        message: format!(
                            "Cannot access {file} — instance configuration is protected."
                        ),
                        exit_code: -1,
                    });
                }
            }
        }

        Ok(())
    }
}

/// Error type for exec tool.
#[derive(Debug, thiserror::Error)]
#[error("Execution failed: {message}")]
pub struct ExecError {
    message: String,
    exit_code: i32,
}

/// Arguments for exec tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecArgs {
    /// The program to execute.
    pub program: String,
    /// Arguments to pass to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional working directory.
    pub working_dir: Option<String>,
    /// Environment variables to set (key-value pairs).
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Timeout in seconds (default: 60).
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Environment variable.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EnvVar {
    /// The variable name.
    pub key: String,
    /// The variable value.
    pub value: String,
}

fn default_timeout() -> u64 {
    60
}

/// Output from exec tool.
#[derive(Debug, Serialize)]
pub struct ExecOutput {
    /// Whether the execution succeeded.
    pub success: bool,
    /// The exit code.
    pub exit_code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Formatted summary.
    pub summary: String,
}

impl Tool for ExecTool {
    const NAME: &'static str = "exec";

    type Error = ExecError;
    type Args = ExecArgs;
    type Output = ExecOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/exec").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "program": {
                        "type": "string",
                        "description": "The program or binary to execute (e.g., 'cargo', 'python', 'node')"
                    },
                    "args": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "default": [],
                        "description": "Arguments to pass to the program"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory for the execution"
                    },
                    "env": {
                        "type": "array",
                        "description": "Environment variables to set",
                        "items": {
                            "type": "object",
                            "properties": {
                                "key": {
                                    "type": "string",
                                    "description": "Environment variable name"
                                },
                                "value": {
                                    "type": "string",
                                    "description": "Environment variable value"
                                }
                            },
                            "required": ["key", "value"]
                        }
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 300,
                        "default": 60,
                        "description": "Maximum time to wait (1-300 seconds)"
                    }
                },
                "required": ["program"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Check for references to sensitive instance paths
        self.check_args(&args.program, &args.args)?;

        // Validate working_dir stays within workspace if specified
        if let Some(ref dir) = args.working_dir {
            let path = std::path::Path::new(dir);
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let workspace_canonical = self
                .workspace
                .canonicalize()
                .unwrap_or_else(|_| self.workspace.clone());
            if !canonical.starts_with(&workspace_canonical) {
                return Err(ExecError {
                    message: format!(
                        "working_dir must be within the workspace ({}).",
                        self.workspace.display()
                    ),
                    exit_code: -1,
                });
            }
        }

        // Block passing secret env var values directly
        for env_var in &args.env {
            for secret in super::shell::SECRET_ENV_VARS {
                if env_var.key == *secret {
                    return Err(ExecError {
                        message: "Cannot set secret environment variables.".to_string(),
                        exit_code: -1,
                    });
                }
            }
        }

        let mut cmd = Command::new(&args.program);
        cmd.args(&args.args);

        // Default to workspace as working directory
        if let Some(dir) = args.working_dir {
            cmd.current_dir(dir);
        } else {
            cmd.current_dir(&self.workspace);
        }

        for env_var in args.env {
            cmd.env(env_var.key, env_var.value);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let timeout = tokio::time::Duration::from_secs(args.timeout_seconds);

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ExecError {
                message: "Execution timed out".to_string(),
                exit_code: -1,
            })?
            .map_err(|e| ExecError {
                message: format!("Failed to execute: {e}"),
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

        let summary = format_exec_output(exit_code, &stdout, &stderr);

        Ok(ExecOutput {
            success,
            exit_code,
            stdout,
            stderr,
            summary,
        })
    }
}

/// Format exec output for display.
fn format_exec_output(exit_code: i32, stdout: &str, stderr: &str) -> String {
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

/// System-internal exec that bypasses path restrictions.
/// Used by the system itself, not LLM-facing.
pub async fn exec(
    program: &str,
    args: &[&str],
    working_dir: Option<&std::path::Path>,
    env: Option<&[(&str, &str)]>,
) -> crate::error::Result<ExecResult> {
    let mut cmd = Command::new(program);
    cmd.args(args);

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_vars) = env {
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = tokio::time::timeout(tokio::time::Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| {
            crate::error::AgentError::Other(anyhow::anyhow!("Execution timed out").into())
        })?
        .map_err(|e| {
            crate::error::AgentError::Other(anyhow::anyhow!("Failed to execute: {e}").into())
        })?;

    Ok(ExecResult {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Result of a subprocess execution.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

use anyhow::Context as _;
