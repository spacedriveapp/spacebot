//! Exec tool for running subprocesses (task workers only).

use crate::sandbox::Sandbox;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

/// Tool for executing subprocesses within a sandboxed environment.
#[derive(Debug, Clone)]
pub struct ExecTool {
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
}

impl ExecTool {
    /// Create a new exec tool with sandbox containment.
    pub fn new(workspace: PathBuf, sandbox: Arc<Sandbox>) -> Self {
        Self { workspace, sandbox }
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
    #[serde(
        default = "default_timeout",
        deserialize_with = "crate::tools::deserialize_string_or_u64"
    )]
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

/// Env vars that enable library injection or alter runtime loading behavior.
const DANGEROUS_ENV_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "RUBYOPT",
    "PERL5OPT",
    "PERL5LIB",
    "BASH_ENV",
    "ENV",
];

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
        // Validate working_dir stays within workspace if specified
        let working_dir = if let Some(ref dir) = args.working_dir {
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
            canonical
        } else {
            self.workspace.clone()
        };

        // Block env vars that enable library injection or alter runtime
        // loading behavior — these allow arbitrary code execution regardless
        // of filesystem sandbox state.
        for env_var in &args.env {
            if DANGEROUS_ENV_VARS
                .iter()
                .any(|blocked| env_var.key.eq_ignore_ascii_case(blocked))
            {
                return Err(ExecError {
                    message: format!(
                        "Cannot set {}: this environment variable enables code injection.",
                        env_var.key
                    ),
                    exit_code: -1,
                });
            }
        }

        let arg_refs: Vec<&str> = args.args.iter().map(|s| s.as_str()).collect();
        let mut cmd = self.sandbox.wrap(&args.program, &arg_refs, &working_dir);

        // Apply user-specified env vars after sandbox wrapping
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
        .map_err(|_| crate::error::AgentError::Other(anyhow::anyhow!("Execution timed out")))?
        .map_err(|e| crate::error::AgentError::Other(anyhow::anyhow!("Failed to execute: {e}")))?;

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
