//! ACP worker backend.
//!
//! Spawns an ACP-compatible agent subprocess and communicates over stdio.
//! Spacebot implements ACP `Client` methods (permissions, fs, terminal) and
//! drives prompt turns through the agent connection.

use crate::config::AcpAgentConfig;
use crate::{AgentId, ChannelId, ProcessEvent, WorkerId};

use agent_client_protocol::{Agent as _, ClientSideConnection};
use agent_client_protocol::{
    ClientCapabilities, ContentBlock, ContentChunk, CreateTerminalRequest, CreateTerminalResponse,
    Error as AcpError, FileSystemCapability, InitializeRequest, PermissionOptionKind,
    PromptRequest, PromptResponse, ProtocolVersion, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TerminalExitStatus, TerminalId,
    TerminalOutputRequest, TerminalOutputResponse, ToolCallStatus, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use anyhow::Context as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_STDERR_LOG_BYTES: usize = 64 * 1024;
const DEFAULT_TERMINAL_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// ACP-backed worker.
pub struct AcpWorker {
    pub id: WorkerId,
    pub channel_id: Option<ChannelId>,
    pub agent_id: AgentId,
    pub task: String,
    pub directory: PathBuf,
    pub acp: AcpAgentConfig,
    pub event_tx: broadcast::Sender<ProcessEvent>,
    pub input_rx: Option<mpsc::Receiver<String>>,
    pub cancellation_token: Option<CancellationToken>,
}

/// Result of an ACP worker run.
pub struct AcpWorkerResult {
    pub session_id: String,
    pub result_text: String,
}

impl AcpWorker {
    pub fn new(
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        task: impl Into<String>,
        directory: PathBuf,
        acp: AcpAgentConfig,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            channel_id,
            agent_id,
            task: task.into(),
            directory,
            acp,
            event_tx,
            input_rx: None,
            cancellation_token: None,
        }
    }

    pub fn new_interactive(
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        task: impl Into<String>,
        directory: PathBuf,
        acp: AcpAgentConfig,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> (Self, mpsc::Sender<String>) {
        let (input_tx, input_rx) = mpsc::channel(32);
        let mut worker = Self::new(channel_id, agent_id, task, directory, acp, event_tx);
        worker.input_rx = Some(input_rx);
        (worker, input_tx)
    }

    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    fn ensure_not_cancelled(&self) -> anyhow::Result<()> {
        if self
            .cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            anyhow::bail!("ACP worker cancelled")
        }

        Ok(())
    }

    pub async fn run(mut self) -> anyhow::Result<AcpWorkerResult> {
        let run_result = async {
            self.ensure_not_cancelled()?;

            if self.acp.command.trim().is_empty() {
                anyhow::bail!("ACP command is empty for worker config '{}'", self.acp.id);
            }

            self.send_status(&format!("starting ACP agent '{}'", self.acp.id));

            let mut command = Command::new(&self.acp.command);
            command
                .args(&self.acp.args)
                .current_dir(&self.directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            for (name, value) in &self.acp.env {
                command.env(name, value);
            }

            let mut child = command.spawn().with_context(|| {
                format!(
                    "failed to spawn ACP agent '{}' with command '{}'",
                    self.acp.id, self.acp.command
                )
            })?;

            let child_stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("failed to capture ACP child stdin"))?;
            let child_stdout = child
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("failed to capture ACP child stdout"))?;

            if let Some(stderr) = child.stderr.take() {
                let worker_id = self.id;
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut chunk = [0u8; 2048];
                    let mut buffer = Vec::new();

                    loop {
                        let read = match reader.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(size) => size,
                            Err(error) => {
                                tracing::debug!(worker_id = %worker_id, %error, "failed to read ACP stderr");
                                return;
                            }
                        };

                        let remaining = MAX_STDERR_LOG_BYTES.saturating_sub(buffer.len());
                        if remaining == 0 {
                            break;
                        }

                        let to_copy = read.min(remaining);
                        buffer.extend_from_slice(&chunk[..to_copy]);

                        if buffer.len() >= MAX_STDERR_LOG_BYTES {
                            break;
                        }
                    }

                    if !buffer.is_empty() {
                        let mut output = String::from_utf8_lossy(&buffer).to_string();
                        if buffer.len() >= MAX_STDERR_LOG_BYTES {
                            output.push_str("\n...[truncated]");
                        }
                        tracing::debug!(worker_id = %worker_id, stderr = %output, "ACP stderr");
                    }
                });
            }

            let workspace_root = self
                .directory
                .canonicalize()
                .unwrap_or_else(|_| self.directory.clone());

            let acp_client = Arc::new(SpacebotAcpClient::new(
                self.agent_id.clone(),
                self.id,
                self.channel_id.clone(),
                self.event_tx.clone(),
                workspace_root,
            ));

            let timeout = self.acp.timeout.max(1);
            let prompt_result = tokio::task::LocalSet::new()
                .run_until(async {
                    let (connection, io_task) = ClientSideConnection::new(
                        acp_client.clone(),
                        child_stdin.compat_write(),
                        child_stdout.compat(),
                        |future| {
                            tokio::task::spawn_local(future);
                        },
                    );

                    tokio::task::spawn_local(async move {
                        if let Err(error) = io_task.await {
                            tracing::debug!(%error, "ACP IO task ended with error");
                        }
                    });

                    let initialize = InitializeRequest::new(ProtocolVersion::LATEST)
                        .client_capabilities(
                            ClientCapabilities::new()
                                .fs(FileSystemCapability::new()
                                    .read_text_file(true)
                                    .write_text_file(true))
                                .terminal(true),
                        );

                    let initialize_response = connection
                        .initialize(initialize)
                        .await
                        .context("ACP initialize failed")?;

                    tracing::debug!(
                        worker_id = %self.id,
                        negotiated_protocol = ?initialize_response.protocol_version,
                        "ACP initialized"
                    );

                    let session = connection
                        .new_session(agent_client_protocol::NewSessionRequest::new(
                            self.directory.clone(),
                        ))
                        .await
                        .context("ACP session/new failed")?;

                    let session_id = session.session_id.0.to_string();

                    self.send_status("running ACP task");

                    self.ensure_not_cancelled()?;
                    let prompt_response = prompt_once(
                        &connection,
                        &session.session_id,
                        &self.task,
                        timeout,
                        self.cancellation_token.as_ref(),
                    )
                    .await?;

                    let mut result_text = acp_client.take_text().await;
                    if result_text.trim().is_empty() {
                        result_text = format!(
                            "ACP worker completed with stop reason: {:?}",
                            prompt_response.stop_reason
                        );
                    }

                    self.send_result(&result_text, true, true);

                    if let Some(mut input_rx) = self.input_rx.take() {
                        self.send_status("waiting for follow-up");
                        while let Some(message) = input_rx.recv().await {
                            self.ensure_not_cancelled()?;
                            self.send_status("processing follow-up");
                            // Discard any accumulated text from the previous prompt
                            drop(acp_client.take_text().await);
                            let follow_up_response = prompt_once(
                                &connection,
                                &session.session_id,
                                &message,
                                timeout,
                                self.cancellation_token.as_ref(),
                            )
                            .await?;
                            let follow_up_text = acp_client.take_text().await;
                            if !follow_up_text.trim().is_empty() {
                                result_text = follow_up_text;
                            } else {
                                result_text = format!(
                                    "ACP follow-up completed with stop reason: {:?}",
                                    follow_up_response.stop_reason
                                );
                            }
                            self.send_result(&result_text, true, true);
                            self.send_status("waiting for follow-up");
                        }
                    }

                    Ok::<(String, String), anyhow::Error>((result_text, session_id))
                })
                .await;

            shutdown_child(&mut child, self.id).await;

            let (result, session_id) = prompt_result?;
            Ok::<AcpWorkerResult, anyhow::Error>(AcpWorkerResult {
                session_id,
                result_text: result,
            })
        }
        .await;

        match run_result {
            Ok(result) => {
                self.send_status("completed");
                self.send_complete(&result.result_text, false, true);
                Ok(result)
            }
            Err(error) => {
                if error.to_string().contains("cancelled") {
                    self.send_status("cancelled");
                    self.send_complete("ACP worker cancelled", false, false);
                } else {
                    self.send_status("failed");
                    self.send_complete(&format!("ACP worker failed: {error}"), true, false);
                }
                Err(error)
            }
        }
    }

    fn send_status(&self, status: &str) {
        self.event_tx
            .send(ProcessEvent::WorkerStatus {
                agent_id: self.agent_id.clone(),
                worker_id: self.id,
                channel_id: self.channel_id.clone(),
                status: status.to_string(),
            })
            .ok();
    }

    fn send_result(&self, result: &str, notify: bool, success: bool) {
        self.event_tx
            .send(ProcessEvent::WorkerResult {
                agent_id: self.agent_id.clone(),
                worker_id: self.id,
                channel_id: self.channel_id.clone(),
                result: result.to_string(),
                notify,
                success,
            })
            .ok();
    }

    fn send_complete(&self, result: &str, notify: bool, success: bool) {
        self.event_tx
            .send(ProcessEvent::WorkerComplete {
                agent_id: self.agent_id.clone(),
                worker_id: self.id,
                channel_id: self.channel_id.clone(),
                result: result.to_string(),
                notify,
                success,
            })
            .ok();
    }
}

async fn shutdown_child(child: &mut Child, worker_id: WorkerId) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            if let Err(error) = child.kill().await {
                tracing::debug!(worker_id = %worker_id, %error, "failed to kill ACP child");
            }
            if let Err(error) = child.wait().await {
                tracing::debug!(worker_id = %worker_id, %error, "failed waiting for ACP child exit");
            }
        }
        Err(error) => {
            tracing::debug!(worker_id = %worker_id, %error, "failed to check ACP child status");
        }
    }
}

async fn prompt_once(
    connection: &ClientSideConnection,
    session_id: &agent_client_protocol::SessionId,
    message: &str,
    timeout_seconds: u64,
    cancellation_token: Option<&CancellationToken>,
) -> anyhow::Result<PromptResponse> {
    let request = PromptRequest::new(session_id.clone(), vec![ContentBlock::from(message)]);
    if let Some(token) = cancellation_token {
        tokio::select! {
            _ = token.cancelled() => {
                anyhow::bail!("ACP worker cancelled")
            }
            response = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_seconds),
                connection.prompt(request),
            ) => {
                response
                    .context("ACP prompt timed out")?
                    .context("ACP prompt failed")
            }
        }
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            connection.prompt(request),
        )
        .await
        .context("ACP prompt timed out")?
        .context("ACP prompt failed")
    }
}

struct TerminalEntry {
    child: Mutex<Child>,
    output: Mutex<Vec<u8>>,
    output_limit: usize,
    truncated: AtomicBool,
    exit_status: Mutex<Option<std::process::ExitStatus>>,
}

impl TerminalEntry {
    fn new(child: Child, output_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            child: Mutex::new(child),
            output: Mutex::new(Vec::new()),
            output_limit,
            truncated: AtomicBool::new(false),
            exit_status: Mutex::new(None),
        })
    }

    /// Check and cache exit status, returning it if the process has exited.
    async fn poll_exit_status(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut stored = self.exit_status.lock().await;
        if stored.is_none()
            && let Some(status) = self.child.lock().await.try_wait()?
        {
            *stored = Some(status);
        }
        Ok(*stored)
    }

    /// Wait for the process to exit, caching and returning the status.
    async fn wait_for_exit(&self) -> std::io::Result<std::process::ExitStatus> {
        let mut stored = self.exit_status.lock().await;
        if let Some(status) = *stored {
            return Ok(status);
        }
        let status = self.child.lock().await.wait().await?;
        *stored = Some(status);
        Ok(status)
    }
}

struct SpacebotAcpClient {
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    workspace_root: PathBuf,
    terminals: Arc<Mutex<HashMap<String, Arc<TerminalEntry>>>>,
    collected_text: Arc<Mutex<String>>,
    thought_buffer: Arc<Mutex<String>>,
}

impl SpacebotAcpClient {
    fn new(
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        event_tx: broadcast::Sender<ProcessEvent>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            agent_id,
            worker_id,
            channel_id,
            event_tx,
            workspace_root,
            terminals: Arc::new(Mutex::new(HashMap::new())),
            collected_text: Arc::new(Mutex::new(String::new())),
            thought_buffer: Arc::new(Mutex::new(String::new())),
        }
    }

    async fn take_text(&self) -> String {
        std::mem::take(&mut *self.collected_text.lock().await)
    }

    async fn flush_thoughts(&self) {
        let mut buffer = self.thought_buffer.lock().await;
        if !buffer.is_empty() {
            self.send_status(buffer.as_str());
            buffer.clear();
        }
    }

    fn is_protected_write_path(path: &Path) -> bool {
        const PROTECTED_FILES: &[&str] = &["SOUL.md", "IDENTITY.md", "USER.md"];
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| PROTECTED_FILES.iter().any(|p| name.eq_ignore_ascii_case(p)))
    }

    fn send_status(&self, status: impl Into<String>) {
        self.event_tx
            .send(ProcessEvent::WorkerStatus {
                agent_id: self.agent_id.clone(),
                worker_id: self.worker_id,
                channel_id: self.channel_id.clone(),
                status: status.into(),
            })
            .ok();
    }

    fn resolve_path(&self, path: &Path) -> agent_client_protocol::Result<PathBuf> {
        if !path.is_absolute() {
            return Err(AcpError::invalid_params().data("path must be absolute"));
        }

        let canonical_workspace = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        let candidate = if path.exists() {
            path.canonicalize().map_err(|error| {
                AcpError::resource_not_found(Some(path.display().to_string()))
                    .data(error.to_string())
            })?
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| AcpError::invalid_params().data("path has no parent"))?;
            let canonical_parent = parent.canonicalize().map_err(|error| {
                AcpError::resource_not_found(Some(parent.display().to_string()))
                    .data(error.to_string())
            })?;
            canonical_parent.join(
                path.file_name()
                    .ok_or_else(|| AcpError::invalid_params().data("path is missing file name"))?,
            )
        };

        if !candidate.starts_with(&canonical_workspace) {
            return Err(AcpError::invalid_params().data(format!(
                "path '{}' is outside workspace root '{}'",
                candidate.display(),
                canonical_workspace.display()
            )));
        }

        Ok(candidate)
    }

    async fn terminal_entry(
        &self,
        terminal_id: &TerminalId,
    ) -> agent_client_protocol::Result<Arc<TerminalEntry>> {
        self.terminals
            .lock()
            .await
            .get(terminal_id.0.as_ref())
            .cloned()
            .ok_or_else(|| AcpError::resource_not_found(Some(terminal_id.0.to_string())))
    }
}

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Client for SpacebotAcpClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        let title = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "permission requested".to_string());

        self.event_tx
            .send(ProcessEvent::WorkerPermission {
                agent_id: self.agent_id.clone(),
                worker_id: self.worker_id,
                channel_id: self.channel_id.clone(),
                permission_id: args.tool_call.tool_call_id.0.to_string(),
                description: title,
                patterns: Vec::new(),
            })
            .ok();

        let selected = args
            .options
            .iter()
            .find(|option| matches!(option.kind, PermissionOptionKind::AllowOnce))
            .or_else(|| {
                args.options
                    .iter()
                    .find(|option| matches!(option.kind, PermissionOptionKind::AllowAlways))
            })
            .or_else(|| args.options.first())
            .ok_or_else(|| AcpError::invalid_params().data("permission request has no options"))?;

        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                selected.option_id.clone(),
            )),
        ))
    }

    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        match args.update {
            SessionUpdate::AgentMessageChunk(ContentChunk { content, .. }) => {
                self.flush_thoughts().await;
                if let ContentBlock::Text(text_content) = content {
                    let mut text = self.collected_text.lock().await;
                    text.push_str(&text_content.text);
                }
            }
            SessionUpdate::AgentThoughtChunk(ContentChunk {
                content: ContentBlock::Text(text_content),
                ..
            }) => {
                let mut buffer = self.thought_buffer.lock().await;
                buffer.push_str(&text_content.text);
            }
            SessionUpdate::ToolCall(tool_call) => {
                self.flush_thoughts().await;
                self.send_status(format!("tool: {}", tool_call.title));
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.flush_thoughts().await;
                if let Some(status) = update.fields.status
                    && status == ToolCallStatus::Failed
                {
                    self.send_status("tool failed");
                }
            }
            SessionUpdate::Plan(_) => {
                self.flush_thoughts().await;
                self.send_status("planning");
            }
            _ => {}
        }

        Ok(())
    }

    async fn write_text_file(
        &self,
        args: WriteTextFileRequest,
    ) -> agent_client_protocol::Result<WriteTextFileResponse> {
        let path = self.resolve_path(&args.path)?;

        if Self::is_protected_write_path(&path) {
            return Err(AcpError::invalid_params().data(
                "ACCESS DENIED: Identity files are protected and cannot be modified through ACP file operations. Use the identity management API instead.",
            ));
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(AcpError::into_internal_error)?;
        }

        tokio::fs::write(&path, args.content)
            .await
            .map_err(AcpError::into_internal_error)?;

        Ok(WriteTextFileResponse::new())
    }

    async fn read_text_file(
        &self,
        args: ReadTextFileRequest,
    ) -> agent_client_protocol::Result<ReadTextFileResponse> {
        let path = self.resolve_path(&args.path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(AcpError::into_internal_error)?;

        let skip = args.line.map(|l| l.saturating_sub(1) as usize).unwrap_or(0);
        let limited_content = match args.limit {
            Some(limit) => content
                .lines()
                .skip(skip)
                .take(limit as usize)
                .collect::<Vec<_>>()
                .join("\n"),
            None if skip > 0 => content.lines().skip(skip).collect::<Vec<_>>().join("\n"),
            None => content,
        };

        Ok(ReadTextFileResponse::new(limited_content))
    }

    async fn create_terminal(
        &self,
        args: CreateTerminalRequest,
    ) -> agent_client_protocol::Result<CreateTerminalResponse> {
        let mut command = Command::new(&args.command);
        command
            .args(&args.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let cwd = match args.cwd {
            Some(cwd) => self.resolve_path(&cwd)?,
            None => self.workspace_root.clone(),
        };
        command.current_dir(cwd);

        for env_var in args.env {
            command.env(env_var.name, env_var.value);
        }

        let mut child = command.spawn().map_err(AcpError::into_internal_error)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let output_limit = args
            .output_byte_limit
            .and_then(|value| usize::try_from(value).ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_TERMINAL_OUTPUT_LIMIT_BYTES);
        let entry = TerminalEntry::new(child, output_limit);

        if let Some(stdout_reader) = stdout {
            spawn_output_reader(entry.clone(), stdout_reader);
        }
        if let Some(stderr_reader) = stderr {
            spawn_output_reader(entry.clone(), stderr_reader);
        }

        let terminal_id = TerminalId::new(format!("term_{}", Uuid::new_v4()));
        self.terminals
            .lock()
            .await
            .insert(terminal_id.0.to_string(), entry);

        Ok(CreateTerminalResponse::new(terminal_id))
    }

    async fn terminal_output(
        &self,
        args: TerminalOutputRequest,
    ) -> agent_client_protocol::Result<TerminalOutputResponse> {
        let entry = self.terminal_entry(&args.terminal_id).await?;
        let exit_status = entry
            .poll_exit_status()
            .await
            .map_err(AcpError::into_internal_error)?;
        let output = String::from_utf8_lossy(&entry.output.lock().await).into_owned();

        Ok(
            TerminalOutputResponse::new(output, entry.truncated.load(Ordering::Relaxed))
                .exit_status(exit_status.map(to_terminal_exit_status)),
        )
    }

    async fn release_terminal(
        &self,
        args: agent_client_protocol::ReleaseTerminalRequest,
    ) -> agent_client_protocol::Result<agent_client_protocol::ReleaseTerminalResponse> {
        if let Some(entry) = self
            .terminals
            .lock()
            .await
            .remove(args.terminal_id.0.as_ref())
        {
            let mut child = entry.child.lock().await;
            if child
                .try_wait()
                .map_err(AcpError::into_internal_error)?
                .is_none()
                && let Err(error) = child.kill().await
            {
                tracing::debug!(%error, "failed to kill ACP terminal on release");
            }
        }

        Ok(agent_client_protocol::ReleaseTerminalResponse::new())
    }

    async fn wait_for_terminal_exit(
        &self,
        args: WaitForTerminalExitRequest,
    ) -> agent_client_protocol::Result<WaitForTerminalExitResponse> {
        let entry = self.terminal_entry(&args.terminal_id).await?;
        let status = entry
            .wait_for_exit()
            .await
            .map_err(AcpError::into_internal_error)?;

        Ok(WaitForTerminalExitResponse::new(to_terminal_exit_status(
            status,
        )))
    }

    async fn kill_terminal_command(
        &self,
        args: agent_client_protocol::KillTerminalCommandRequest,
    ) -> agent_client_protocol::Result<agent_client_protocol::KillTerminalCommandResponse> {
        let entry = self.terminal_entry(&args.terminal_id).await?;
        let mut child = entry.child.lock().await;
        if child
            .try_wait()
            .map_err(AcpError::into_internal_error)?
            .is_none()
        {
            child.kill().await.map_err(AcpError::into_internal_error)?;
        }

        Ok(agent_client_protocol::KillTerminalCommandResponse::new())
    }
}

fn spawn_output_reader(
    entry: Arc<TerminalEntry>,
    mut reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) {
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        loop {
            let read = match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(size) => size,
                Err(error) => {
                    tracing::debug!(%error, "failed reading ACP terminal output");
                    break;
                }
            };

            let mut output = entry.output.lock().await;
            output.extend_from_slice(&chunk[..read]);
            if output.len() > entry.output_limit {
                let overflow = output.len() - entry.output_limit;
                output.drain(0..overflow);
                entry.truncated.store(true, Ordering::Relaxed);
            }
        }
    });
}

fn to_terminal_exit_status(status: std::process::ExitStatus) -> TerminalExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        TerminalExitStatus::new()
            .exit_code(status.code().and_then(|c| u32::try_from(c).ok()))
            .signal(status.signal().map(|signal| signal.to_string()))
    }

    #[cfg(not(unix))]
    {
        TerminalExitStatus::new().exit_code(status.code().and_then(|c| u32::try_from(c).ok()))
    }
}
