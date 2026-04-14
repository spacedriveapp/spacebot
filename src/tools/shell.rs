//! Shell tool for executing shell commands and subprocesses (task workers only).
//!
//! This is the unified execution tool — it replaces the previous `shell` + `exec`
//! split. Commands run through `sh -c` with optional per-command environment
//! variables. Dangerous env vars that enable library injection are blocked.
//!
//! Output is streamed line-by-line via `ProcessEvent::ToolOutput` when a
//! `tool_output_tx` sender is provided (worker calls). System-internal calls
//! skip streaming and just collect output.

use crate::sandbox::Sandbox;
use crate::tools::ToolCallRegistry;
use crate::{AgentId, ChannelId, ProcessEvent, ProcessId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::process::Command;
use tracing::instrument;

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

/// Exit code returned when a command is killed for waiting for input.
const EXIT_CODE_WAITING_FOR_INPUT: i32 = -2;

/// Tool for executing shell commands within a sandboxed environment.
#[derive(Debug, Clone)]
pub struct ShellTool {
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
    tool_output_tx: Option<tokio::sync::broadcast::Sender<ProcessEvent>>,
    process_id: Option<ProcessId>,
    channel_id: Option<ChannelId>,
    agent_id: Option<AgentId>,
    tool_call_registry: Option<ToolCallRegistry>,
}

impl ShellTool {
    pub fn new(workspace: PathBuf, sandbox: Arc<Sandbox>) -> Self {
        Self {
            workspace,
            sandbox,
            tool_output_tx: None,
            process_id: None,
            channel_id: None,
            agent_id: None,
            tool_call_registry: None,
        }
    }

    /// Enable line-by-line output streaming via `ProcessEvent::ToolOutput`.
    pub fn with_streaming(
        mut self,
        tool_output_tx: tokio::sync::broadcast::Sender<ProcessEvent>,
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        tool_call_registry: ToolCallRegistry,
    ) -> Self {
        self.tool_output_tx = Some(tool_output_tx);
        self.process_id = Some(process_id);
        self.channel_id = channel_id;
        self.agent_id = Some(agent_id);
        self.tool_call_registry = Some(tool_call_registry);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Shell command failed: {message}")]
pub struct ShellError {
    message: String,
    exit_code: i32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    pub command: String,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(
        default = "default_timeout",
        deserialize_with = "crate::tools::deserialize_string_or_u64"
    )]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Serialize)]
pub struct ShellOutput {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub summary: String,
    #[serde(default)]
    pub waiting_for_input: bool,
}

/// Context for streaming shell output lines.
struct StreamContext {
    tool_output_tx: Option<tokio::sync::broadcast::Sender<ProcessEvent>>,
    agent_id: Option<AgentId>,
    process_id: Option<ProcessId>,
    channel_id: Option<ChannelId>,
    /// Stable identifier for this tool invocation, used to correlate ToolOutput events.
    call_id: String,
    stream_name: String,
    last_output_ms: Arc<AtomicU64>,
    interactive_detected: Arc<AtomicBool>,
    prompt_hint_detected: Arc<AtomicBool>,
}

fn has_interactive_prompt_hint(line: &str) -> bool {
    let normalized = line.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }

    [
        "[y/n]",
        "[y/n]:",
        "[y/n]?",
        "[y/n/q]",
        "(y/n)",
        "yes/no",
        "press enter",
        "enter to continue",
        "continue? [y",
        "are you sure",
        "do you want to continue",
        "password:",
        "passphrase",
        "accept? [y",
    ]
    .iter()
    .any(|hint| normalized.contains(hint))
}

async fn stream_lines<R: AsyncBufRead + Unpin>(mut reader: R, ctx: &StreamContext) -> String {
    let mut collected = String::new();
    loop {
        if ctx.interactive_detected.load(Ordering::SeqCst) {
            break;
        }
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if has_interactive_prompt_hint(&line) {
                    ctx.prompt_hint_detected.store(true, Ordering::SeqCst);
                }
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                ctx.last_output_ms.store(now_ms, Ordering::SeqCst);

                if let (Some(tx), Some(agent_id), Some(process_id)) =
                    (&ctx.tool_output_tx, &ctx.agent_id, &ctx.process_id)
                {
                    let scrubbed = crate::secrets::scrub::scrub_leaks(&line);
                    let line = scrubbed.trim_end_matches(['\r', '\n']).to_string();
                    if let Err(err) = tx.send(ProcessEvent::ToolOutput {
                        agent_id: agent_id.clone(),
                        process_id: process_id.clone(),
                        channel_id: ctx.channel_id.clone(),
                        call_id: ctx.call_id.clone(),
                        tool_name: "shell".to_string(),
                        line,
                        stream: ctx.stream_name.clone(),
                    }) {
                        tracing::trace!(%err, "tool_output broadcast dropped - no receivers");
                    }
                }

                collected.push_str(&line);
            }
            Err(error) => {
                tracing::debug!(%error, stream = ctx.stream_name, "shell stream read error");
                break;
            }
        }
    }
    collected
}

fn spawn_quiesce_watchdog(
    last_output_ms: Arc<AtomicU64>,
    interactive_detected: Arc<AtomicBool>,
    prompt_hint_detected: Arc<AtomicBool>,
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let last = last_output_ms.load(Ordering::SeqCst);
            if now_ms.saturating_sub(last) > 5000 && prompt_hint_detected.load(Ordering::SeqCst) {
                interactive_detected.store(true, Ordering::SeqCst);
                // Kill the child to unblock stream_lines tasks blocked in read_line()
                let lock_result =
                    tokio::time::timeout(std::time::Duration::from_secs(1), child.lock()).await;
                match lock_result {
                    Ok(mut child_guard) => {
                        if let Err(err) = child_guard.kill().await {
                            tracing::warn!(%err, "failed to kill child process during quiesce detection");
                        }
                    }
                    Err(_) => {
                        tracing::warn!("timed out acquiring child lock for quiesce kill");
                    }
                }
                break;
            }
        }
    })
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
                    "env": {
                        "type": "array",
                        "description": "Environment variables to set for this command",
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
                        "description": "Maximum time to wait for the command to complete (1-300 seconds)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Reserve the lifecycle call ID up-front so validation errors cannot
        // leave stale IDs queued for later shell invocations.
        let streaming_call_id = self.tool_output_tx.as_ref().map(|_| {
            self.tool_call_registry
                .as_ref()
                .and_then(|registry| {
                    self.process_id
                        .as_ref()
                        .and_then(|process_id| registry.take(process_id, Self::NAME))
                })
                .unwrap_or_else(|| format!("shell_{}", uuid::Uuid::new_v4()))
        });

        let working_dir = if let Some(ref dir) = args.working_dir {
            let raw_path = Path::new(dir);
            let resolved = if raw_path.is_absolute() {
                raw_path.to_path_buf()
            } else {
                self.workspace.join(raw_path)
            };
            let canonical = resolved.canonicalize().unwrap_or(resolved);

            if self.sandbox.mode_enabled() && !self.sandbox.is_path_allowed(&canonical) {
                return Err(ShellError {
                    message: format!(
                        "working_dir must be within the workspace ({}) or an allowed project path.",
                        self.workspace.display()
                    ),
                    exit_code: -1,
                });
            }

            canonical
        } else {
            self.workspace.clone()
        };

        for env_var in &args.env {
            if env_var.key.is_empty() {
                return Err(ShellError {
                    message: "Environment variable name cannot be empty.".to_string(),
                    exit_code: -1,
                });
            }
            if env_var.key.contains('=') {
                return Err(ShellError {
                    message: format!(
                        "Environment variable name '{}' cannot contain '='.",
                        env_var.key
                    ),
                    exit_code: -1,
                });
            }
            if env_var.key.contains('\0') || env_var.value.contains('\0') {
                return Err(ShellError {
                    message: format!(
                        "Environment variable '{}' cannot contain null bytes.",
                        env_var.key
                    ),
                    exit_code: -1,
                });
            }
        }

        for env_var in &args.env {
            if DANGEROUS_ENV_VARS
                .iter()
                .any(|blocked| env_var.key.eq_ignore_ascii_case(blocked))
            {
                return Err(ShellError {
                    message: format!(
                        "Cannot set {}: this environment variable enables code injection.",
                        env_var.key
                    ),
                    exit_code: -1,
                });
            }
        }

        let command_env: std::collections::HashMap<String, String> = args
            .env
            .into_iter()
            .map(|var| (var.key, var.value))
            .collect();

        let mut cmd = if cfg!(target_os = "windows") {
            self.sandbox
                .wrap("cmd", &["/C", &args.command], &working_dir, &command_env)
        } else {
            self.sandbox
                .wrap("sh", &["-c", &args.command], &working_dir, &command_env)
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let timeout = tokio::time::Duration::from_secs(args.timeout_seconds);

        if self.tool_output_tx.is_none() {
            return run_batch(cmd, timeout).await;
        }

        run_streaming(
            cmd,
            timeout,
            &self.tool_output_tx,
            &self.process_id,
            &self.channel_id,
            &self.agent_id,
            streaming_call_id.expect("streaming call_id must exist when streaming is enabled"),
        )
        .await
    }
}

async fn run_batch(
    mut cmd: Command,
    timeout: std::time::Duration,
) -> Result<ShellOutput, ShellError> {
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

    let summary = format_shell_output(exit_code, &stdout, &stderr, false);

    Ok(ShellOutput {
        success,
        exit_code,
        stdout,
        stderr,
        summary,
        waiting_for_input: false,
    })
}

#[instrument(skip(cmd, tool_output_tx), fields(process_id = ?process_id))]
async fn run_streaming(
    mut cmd: Command,
    timeout: std::time::Duration,
    tool_output_tx: &Option<tokio::sync::broadcast::Sender<ProcessEvent>>,
    process_id: &Option<ProcessId>,
    channel_id: &Option<ChannelId>,
    agent_id: &Option<AgentId>,
    call_id: String,
) -> Result<ShellOutput, ShellError> {
    let mut child = cmd.spawn().map_err(|e| ShellError {
        message: format!("Failed to spawn command: {e}"),
        exit_code: -1,
    })?;

    let stdout_pipe = child.stdout.take().ok_or_else(|| ShellError {
        message: "Failed to capture stdout".to_string(),
        exit_code: -1,
    })?;
    let stderr_pipe = child.stderr.take().ok_or_else(|| ShellError {
        message: "Failed to capture stderr".to_string(),
        exit_code: -1,
    })?;

    let start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last_output_ms = Arc::new(AtomicU64::new(start_ms));
    let interactive_detected = Arc::new(AtomicBool::new(false));
    let prompt_hint_detected = Arc::new(AtomicBool::new(false));

    // Wrap child in Arc<Mutex<>> so the watchdog can kill it
    let child_arc = Arc::new(tokio::sync::Mutex::new(child));

    let watchdog = spawn_quiesce_watchdog(
        Arc::clone(&last_output_ms),
        Arc::clone(&interactive_detected),
        Arc::clone(&prompt_hint_detected),
        Arc::clone(&child_arc),
    );

    let stdout_reader = tokio::io::BufReader::new(stdout_pipe);
    let stderr_reader = tokio::io::BufReader::new(stderr_pipe);

    let stdout_ctx = StreamContext {
        tool_output_tx: tool_output_tx.clone(),
        agent_id: agent_id.clone(),
        process_id: process_id.clone(),
        channel_id: channel_id.clone(),
        call_id: call_id.clone(),
        stream_name: "stdout".to_string(),
        last_output_ms: Arc::clone(&last_output_ms),
        interactive_detected: Arc::clone(&interactive_detected),
        prompt_hint_detected: Arc::clone(&prompt_hint_detected),
    };

    let stderr_ctx = StreamContext {
        tool_output_tx: tool_output_tx.clone(),
        agent_id: agent_id.clone(),
        process_id: process_id.clone(),
        channel_id: channel_id.clone(),
        call_id,
        stream_name: "stderr".to_string(),
        last_output_ms: Arc::clone(&last_output_ms),
        interactive_detected: Arc::clone(&interactive_detected),
        prompt_hint_detected: Arc::clone(&prompt_hint_detected),
    };

    // Run streaming with overall timeout
    let streaming_result = tokio::time::timeout(timeout, async {
        tokio::join!(
            stream_lines(stdout_reader, &stdout_ctx),
            stream_lines(stderr_reader, &stderr_ctx),
        )
    })
    .await;

    let (stdout_lines, stderr_lines) = match streaming_result {
        Ok((stdout, stderr)) => {
            watchdog.abort();
            (stdout, stderr)
        }
        Err(_) => {
            // Timeout - kill child and return error
            watchdog.abort();
            // Acquire lock with timeout to avoid indefinite blocking
            let lock_result =
                tokio::time::timeout(std::time::Duration::from_secs(1), child_arc.lock()).await;
            match lock_result {
                Ok(mut child_guard) => {
                    if let Err(err) = child_guard.kill().await {
                        tracing::warn!(%err, "failed to kill child process during timeout handling");
                    }
                }
                Err(_) => {
                    tracing::warn!("timed out acquiring child lock for timeout kill");
                }
            }
            return Err(ShellError {
                message: format!("Command timed out after {} seconds", timeout.as_secs()),
                exit_code: -1,
            });
        }
    };

    let waiting_for_input = interactive_detected.load(Ordering::SeqCst);

    // Wait for child to exit (with short timeout in case watchdog already killed it)
    let exit_status = {
        let mut child_guard = child_arc.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), child_guard.wait()).await
    };

    let exit_code = match exit_status {
        Ok(Ok(status)) => {
            if waiting_for_input {
                EXIT_CODE_WAITING_FOR_INPUT
            } else {
                status.code().unwrap_or(-1)
            }
        }
        _ => {
            if waiting_for_input {
                EXIT_CODE_WAITING_FOR_INPUT
            } else {
                -1
            }
        }
    };

    let success = !waiting_for_input && exit_code == 0;

    let stdout = crate::tools::truncate_output(&stdout_lines, crate::tools::MAX_TOOL_OUTPUT_BYTES);
    let stderr = crate::tools::truncate_output(&stderr_lines, crate::tools::MAX_TOOL_OUTPUT_BYTES);

    let summary = format_shell_output(exit_code, &stdout, &stderr, waiting_for_input);

    Ok(ShellOutput {
        success,
        exit_code,
        stdout,
        stderr,
        summary,
        waiting_for_input,
    })
}

fn format_shell_output(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    waiting_for_input: bool,
) -> String {
    let mut output = String::new();

    if waiting_for_input {
        output.push_str("Command appears to be waiting for interactive input.\n\n");
        output.push_str(&format!("Exit code: {}\n", exit_code));

        let last_lines: Vec<&str> = stdout
            .lines()
            .chain(stderr.lines())
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !last_lines.is_empty() {
            output.push_str("\nLast output:\n");
            for line in &last_lines {
                output.push_str(&format!("  {line}\n"));
            }
        }

        output.push_str(
            "\nRetry with --yes, -y, or pipe input (echo y | command) to avoid interactive prompts.\n",
        );
        return output;
    }

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

#[derive(Debug, Clone)]
pub struct ShellResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ShellResult {
    pub fn format(&self) -> String {
        format_shell_output(self.exit_code, &self.stdout, &self.stderr, false)
    }
}
