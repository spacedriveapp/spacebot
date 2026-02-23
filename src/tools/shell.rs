//! Shell tool for executing shell commands (task workers only).

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Sensitive filenames that should not be accessible via shell commands.
pub const SENSITIVE_FILES: &[&str] = &[
    "config.toml",
    "config.redb",
    "settings.redb",
    ".env",
    "spacebot.db",
];

/// Environment variable names that contain secrets.
pub const SECRET_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "DISCORD_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "BRAVE_SEARCH_API_KEY",
];

/// Tool for executing shell commands, with path restrictions to prevent
/// access to instance-level configuration and secrets.
#[derive(Debug, Clone)]
pub struct ShellTool {
    instance_dir: PathBuf,
    workspace: PathBuf,
}

impl ShellTool {
    /// Create a new shell tool with the given instance directory for path blocking.
    pub fn new(instance_dir: PathBuf, workspace: PathBuf) -> Self {
        Self {
            instance_dir,
            workspace,
        }
    }

    /// Check if a command references sensitive instance paths or secret env vars.
    fn check_command(&self, command: &str) -> Result<(), ShellError> {
        let instance_str = self.instance_dir.to_string_lossy();

        // Block any command that directly references the instance directory.
        // This prevents listing, reading, traversing, or archiving the instance
        // dir and its contents (config.toml, databases, agent data, etc).
        // Commands that reference the workspace (which is inside the instance dir)
        // are allowed through.
        if command.contains(instance_str.as_ref()) {
            let workspace_str = self.workspace.to_string_lossy();
            if !command.contains(workspace_str.as_ref()) {
                return Err(ShellError {
                    message: "ACCESS DENIED: Cannot access the instance directory — it contains \
                              protected configuration and data. Do not attempt to reproduce or \
                              guess its contents. Inform the user that this path is restricted."
                        .to_string(),
                    exit_code: -1,
                });
            }
        }

        // Block commands referencing sensitive files even without the full instance path
        // (e.g. "cat config.toml" from a relative path, or via variable expansion)
        for file in SENSITIVE_FILES {
            if command.contains(file) {
                let workspace_str = self.workspace.to_string_lossy();
                let mentions_workspace = command.contains(workspace_str.as_ref());
                let mentions_instance = command.contains(instance_str.as_ref());

                // Block if referencing instance dir, or if not clearly targeting workspace
                if mentions_instance || !mentions_workspace {
                    return Err(ShellError {
                        message: format!(
                            "ACCESS DENIED: Cannot access {file} — instance configuration is protected. \
                             Do not attempt to reproduce or guess the file contents."
                        ),
                        exit_code: -1,
                    });
                }
            }
        }

        // Block access to secret environment variables via any expansion syntax
        for var in SECRET_ENV_VARS {
            if command.contains(&format!("${var}"))
                || command.contains(&format!("${{{var}}}"))
                || command.contains(&format!("printenv {var}"))
            {
                return Err(ShellError {
                    message: "Cannot access secret environment variables.".to_string(),
                    exit_code: -1,
                });
            }
            // Block unbraced $VAR at word boundaries (covers `echo $SECRET` patterns)
            let dollar_var = format!("${var}");
            if let Some(pos) = command.find(&dollar_var) {
                let after = pos + dollar_var.len();
                let next_char = command[after..].chars().next();
                // $VAR is a match if followed by non-alphanumeric/underscore or end-of-string
                if next_char.is_none()
                    || (!next_char.unwrap().is_alphanumeric() && next_char.unwrap() != '_')
                {
                    return Err(ShellError {
                        message: "Cannot access secret environment variables.".to_string(),
                        exit_code: -1,
                    });
                }
            }
        }

        // Block broad env dumps that would expose secrets
        if command.contains("printenv") {
            let trimmed = command.trim();
            if trimmed == "printenv"
                || trimmed.ends_with("| printenv")
                || trimmed.contains("printenv |")
                || trimmed.contains("printenv >")
            {
                return Err(ShellError {
                    message: "Cannot dump all environment variables — they may contain secrets."
                        .to_string(),
                    exit_code: -1,
                });
            }
        }
        if command.contains("env") {
            let trimmed = command.trim();
            if trimmed == "env" || trimmed.starts_with("env |") || trimmed.starts_with("env >") {
                return Err(ShellError {
                    message: "Cannot dump all environment variables — they may contain secrets."
                        .to_string(),
                    exit_code: -1,
                });
            }
        }

        // Block shell builtins that dump all variables
        for dump_cmd in ["set", "declare -p", "export -p", "compgen -e", "compgen -v"] {
            let trimmed = command.trim();
            if trimmed == dump_cmd
                || trimmed.starts_with(&format!("{dump_cmd} "))
                || trimmed.starts_with(&format!("{dump_cmd}|"))
                || trimmed.starts_with(&format!("{dump_cmd}>"))
                || trimmed.contains(&format!("| {dump_cmd}"))
                || trimmed.contains(&format!("; {dump_cmd}"))
                || trimmed.contains(&format!("&& {dump_cmd}"))
            {
                return Err(ShellError {
                    message: "Cannot dump environment variables — they may contain secrets."
                        .to_string(),
                    exit_code: -1,
                });
            }
        }

        // Block subshell/command substitution that could bypass string-level checks.
        // Backtick and $() let an attacker compose a blocked command dynamically.
        if command.contains('`')
            || command.contains("$(")
            || command.contains("<(")
            || command.contains(">(")
        {
            return Err(ShellError {
                message: "Subshell and command substitution (`...`, $(...), <(...), >(...)) \
                          are not allowed."
                    .to_string(),
                exit_code: -1,
            });
        }

        // Block eval/exec which can dynamically construct any command
        if contains_shell_builtin(command, "eval") || contains_shell_builtin(command, "exec") {
            return Err(ShellError {
                message: "eval and exec are not allowed.".to_string(),
                exit_code: -1,
            });
        }

        // Block interpreter one-liners that can bypass shell-level restrictions
        for interpreter in [
            "python3 -c",
            "python -c",
            "perl -e",
            "ruby -e",
            "node -e",
            "node --eval",
        ] {
            if command.contains(interpreter) {
                return Err(ShellError {
                    message:
                        "Inline interpreter execution is not permitted — use script files instead."
                            .to_string(),
                    exit_code: -1,
                });
            }
        }

        // Block /proc entries and /dev paths that expose environment or fd contents
        if command.contains("/proc/self/environ")
            || command.contains("/proc/*/environ")
            || command.contains("/dev/fd/")
            || command.contains("/dev/stdin")
        {
            return Err(ShellError {
                message: "Cannot access process environment — it may contain secrets.".to_string(),
                exit_code: -1,
            });
        }

        // Block additional commands that dump environment state
        if contains_shell_builtin(command, "set")
            || command.contains("declare -p")
            || contains_shell_builtin(command, "compgen")
            || contains_shell_builtin(command, "export")
        {
            return Err(ShellError {
                message: "Cannot dump shell state — it may contain secrets.".to_string(),
                exit_code: -1,
            });
        }

        Ok(())
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
    #[serde(default = "default_timeout")]
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
                        "maximum": 300,
                        "default": 60,
                        "description": "Maximum time to wait for the command to complete (1-300 seconds)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Check for commands targeting sensitive paths or env vars
        self.check_command(&args.command)?;

        // Validate working_dir stays within workspace if specified
        if let Some(ref dir) = args.working_dir {
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
        }

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&args.command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&args.command);
            c
        };

        // Default to workspace as working directory
        if let Some(dir) = args.working_dir {
            cmd.current_dir(dir);
        } else {
            cmd.current_dir(&self.workspace);
        }

        // Prepend persistent tools directory to PATH so user-installed
        // binaries survive container restarts.
        let tools_bin = self.instance_dir.join("tools/bin");
        if let Ok(current_path) = std::env::var("PATH") {
            cmd.env("PATH", format!("{}:{current_path}", tools_bin.display()));
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Set timeout
        let timeout = tokio::time::Duration::from_secs(args.timeout_seconds);

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ShellError {
                message: "Command timed out".to_string(),
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

/// Check if a shell builtin appears as a standalone command
/// (not as a substring of another word).
fn contains_shell_builtin(command: &str, builtin: &str) -> bool {
    for segment in command.split(['|', ';', '&']) {
        let trimmed = segment.trim();
        if trimmed == builtin
            || trimmed.starts_with(&format!("{builtin} "))
            || trimmed.starts_with(&format!("{builtin}\t"))
        {
            return true;
        }
    }
    false
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
