//! OpenCode server process management and HTTP client.
//!
//! Manages persistent OpenCode server processes (one per working directory).
//! Each server is spawned as `opencode serve --port <port>` and communicated
//! with via its HTTP API. Servers are reused across worker tasks targeting
//! the same directory.
//!
//! Port mappings are persisted to disk so that after a spacebot restart, we can
//! reattach to OpenCode servers that are still running from the previous session.

use crate::opencode::types::*;

use anyhow::{Context as _, bail};
use reqwest::Client;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Maximum health check attempts during server startup.
const HEALTH_CHECK_MAX_ATTEMPTS: u32 = 30;
/// Delay between health check attempts in milliseconds.
const HEALTH_CHECK_INTERVAL_MS: u64 = 1000;
/// Maximum restart attempts before giving up.
const MAX_RESTART_RETRIES: u32 = 5;

/// A running OpenCode server process bound to a specific directory.
pub struct OpenCodeServer {
    directory: PathBuf,
    port: u16,
    /// None for reattached servers (process was spawned by a previous spacebot run).
    process: Option<Child>,
    base_url: String,
    client: Client,
    restart_count: u32,
    opencode_path: String,
    permissions: OpenCodePermissions,
}

impl OpenCodeServer {
    /// Spawn a new OpenCode server for the given directory.
    /// Uses a deterministic port derived from the directory path so that
    /// servers can be rediscovered after a spacebot restart.
    pub async fn spawn(
        directory: PathBuf,
        opencode_path: &str,
        permissions: &OpenCodePermissions,
    ) -> anyhow::Result<Self> {
        let port = port_for_directory(&directory);
        let base_url = format!("http://127.0.0.1:{port}");

        let env_config = OpenCodeEnvConfig::new(permissions);
        let config_json =
            serde_json::to_string(&env_config).context("failed to serialize OpenCode config")?;

        tracing::info!(
            directory = %directory.display(),
            port,
            "spawning OpenCode server"
        );

        let process = Command::new(opencode_path)
            .args(["serve", "--port", &port.to_string()])
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("OPENCODE_CONFIG_CONTENT", &config_json)
            .env("OPENCODE_PORT", port.to_string())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn OpenCode at '{}' for directory '{}'",
                    opencode_path,
                    directory.display()
                )
            })?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("failed to create HTTP client")?;

        let server = Self {
            directory,
            port,
            process: Some(process),
            base_url,
            client,
            restart_count: 0,
            opencode_path: opencode_path.to_string(),
            permissions: permissions.clone(),
        };

        server.wait_for_health().await?;

        tracing::info!(
            directory = %server.directory.display(),
            port = server.port,
            "OpenCode server ready"
        );

        Ok(server)
    }

    /// Try to reattach to an OpenCode server on the deterministic port for
    /// this directory. Returns None if nothing is listening.
    async fn reattach(
        directory: PathBuf,
        opencode_path: &str,
        permissions: &OpenCodePermissions,
    ) -> Option<Self> {
        let port = port_for_directory(&directory);
        let base_url = format!("http://127.0.0.1:{port}");
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .ok()?;

        let server = Self {
            directory,
            port,
            process: None, // we didn't spawn it
            base_url,
            client,
            restart_count: 0,
            opencode_path: opencode_path.to_string(),
            permissions: permissions.clone(),
        };

        // Quick health check -- if it fails, server is gone
        match server.health_check().await {
            Ok(true) => {
                tracing::info!(
                    directory = %server.directory.display(),
                    port,
                    "reattached to existing OpenCode server"
                );
                Some(server)
            }
            _ => {
                tracing::debug!(
                    directory = %server.directory.display(),
                    port,
                    "could not reattach to OpenCode server, not responding"
                );
                None
            }
        }
    }

    /// Poll the health endpoint until the server is ready.
    async fn wait_for_health(&self) -> anyhow::Result<()> {
        for attempt in 1..=HEALTH_CHECK_MAX_ATTEMPTS {
            match self.health_check().await {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    tracing::trace!(
                        attempt,
                        directory = %self.directory.display(),
                        "health check returned unhealthy, retrying"
                    );
                }
                Err(error) => {
                    tracing::trace!(
                        attempt,
                        %error,
                        directory = %self.directory.display(),
                        "health check failed, retrying"
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(HEALTH_CHECK_INTERVAL_MS)).await;
        }

        bail!(
            "OpenCode server for '{}' failed to become healthy after {} attempts",
            self.directory.display(),
            HEALTH_CHECK_MAX_ATTEMPTS
        );
    }

    /// Check if the server is healthy.
    async fn health_check(&self) -> anyhow::Result<bool> {
        let url = format!("{}/global/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(true);
        }

        // Fallback
        let url = format!("{}/api/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?;

        Ok(response.status().is_success())
    }

    /// Check if the server is still alive. For spawned servers, checks the
    /// process handle. For reattached servers, does a health check.
    pub async fn is_alive(&mut self) -> bool {
        match &mut self.process {
            Some(child) => child.try_wait().ok().flatten().is_none(),
            // Reattached server -- no process handle, check via HTTP
            None => self.health_check().await.unwrap_or(false),
        }
    }

    /// Get the port this server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Restart the server process. Reuses the same directory and config.
    pub async fn restart(&mut self) -> anyhow::Result<()> {
        self.restart_count += 1;
        if self.restart_count > MAX_RESTART_RETRIES {
            bail!(
                "OpenCode server for '{}' exceeded max restart attempts ({})",
                self.directory.display(),
                MAX_RESTART_RETRIES
            );
        }

        tracing::warn!(
            directory = %self.directory.display(),
            restart_count = self.restart_count,
            "restarting OpenCode server"
        );

        // Kill existing process if still running
        if let Some(mut child) = self.process.take() {
            let _ = child.kill().await;
        }

        // Reuse the same deterministic port
        let port = port_for_directory(&self.directory);
        let base_url = format!("http://127.0.0.1:{port}");

        let env_config = OpenCodeEnvConfig::new(&self.permissions);
        let config_json = serde_json::to_string(&env_config)?;

        let process = Command::new(&self.opencode_path)
            .args(["serve", "--port", &port.to_string()])
            .current_dir(&self.directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("OPENCODE_CONFIG_CONTENT", &config_json)
            .env("OPENCODE_PORT", port.to_string())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to restart OpenCode server for '{}'",
                    self.directory.display()
                )
            })?;

        self.port = port;
        self.base_url = base_url;
        self.process = Some(process);

        self.wait_for_health().await?;

        tracing::info!(
            directory = %self.directory.display(),
            port = self.port,
            restart_count = self.restart_count,
            "OpenCode server restarted"
        );

        Ok(())
    }

    /// Kill the server process.
    pub async fn kill(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill().await;
            tracing::info!(
                directory = %self.directory.display(),
                "OpenCode server killed"
            );
        }
        // For reattached servers we don't own the process, so we can't kill it.
        // It'll get cleaned up when the OS reaps it or the user kills it.
    }

    // -- API methods --

    /// Get the base URL for this server.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get the directory this server is bound to.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Create a new session.
    pub async fn create_session(&self, title: Option<String>) -> anyhow::Result<Session> {
        let url = format!("{}/session", self.base_url);
        let body = CreateSessionRequest { title };

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .json(&body)
            .send()
            .await
            .context("failed to create OpenCode session")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("create session failed ({status}): {text}");
        }

        response
            .json::<Session>()
            .await
            .context("failed to parse session response")
    }

    /// Send a prompt to a session (blocking until complete).
    pub async fn send_prompt(
        &self,
        session_id: &str,
        request: &SendPromptRequest,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .json(request)
            .send()
            .await
            .context("failed to send prompt to OpenCode session")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("send prompt failed ({status}): {text}");
        }

        response
            .json::<serde_json::Value>()
            .await
            .context("failed to parse prompt response")
    }

    /// Send a prompt asynchronously (returns immediately, use SSE events for results).
    pub async fn send_prompt_async(
        &self,
        session_id: &str,
        request: &SendPromptRequest,
    ) -> anyhow::Result<()> {
        let url = format!("{}/session/{}/prompt_async", self.base_url, session_id);

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .json(request)
            .send()
            .await
            .context("failed to send async prompt")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("async prompt failed ({status}): {text}");
        }

        Ok(())
    }

    /// Abort a session.
    pub async fn abort_session(&self, session_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/session/{}/abort", self.base_url, session_id);

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .send()
            .await
            .context("failed to abort OpenCode session")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("abort session failed ({status}): {text}");
        }

        Ok(())
    }

    /// Reply to a permission request.
    pub async fn reply_permission(
        &self,
        request_id: &str,
        reply: PermissionReply,
    ) -> anyhow::Result<()> {
        let url = format!("{}/permission/{}/reply", self.base_url, request_id);
        let body = PermissionReplyRequest {
            reply,
            message: None,
        };

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .json(&body)
            .send()
            .await
            .context("failed to reply to permission")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("permission reply failed ({status}): {text}");
        }

        Ok(())
    }

    /// Reply to a question request.
    pub async fn reply_question(
        &self,
        request_id: &str,
        answers: Vec<QuestionAnswer>,
    ) -> anyhow::Result<()> {
        let url = format!("{}/question/{}/reply", self.base_url, request_id);
        let body = QuestionReplyRequest { answers };

        let response = self
            .client
            .post(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .json(&body)
            .send()
            .await
            .context("failed to reply to question")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("question reply failed ({status}): {text}");
        }

        Ok(())
    }

    /// Subscribe to the SSE event stream. Returns a response whose body can
    /// be read as a byte stream and parsed line-by-line for SSE events.
    pub async fn subscribe_events(&self) -> anyhow::Result<reqwest::Response> {
        let url = format!("{}/event", self.base_url);

        let response = self
            .client
            .get(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .header("Accept", "text/event-stream")
            .timeout(std::time::Duration::from_secs(86400)) // long-lived
            .send()
            .await
            .context("failed to subscribe to OpenCode event stream")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("event subscription failed ({status}): {text}");
        }

        Ok(response)
    }

    /// Get messages for a session (for reading final results).
    pub async fn get_messages(&self, session_id: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);

        let response = self
            .client
            .get(&url)
            .query(&[("directory", self.directory.to_str().unwrap_or("."))])
            .send()
            .await
            .context("failed to get session messages")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("get messages failed ({status}): {text}");
        }

        response
            .json::<Vec<serde_json::Value>>()
            .await
            .context("failed to parse messages response")
    }
}

impl Drop for OpenCodeServer {
    fn drop(&mut self) {
        // Spawned servers: kill_on_drop(true) handles cleanup.
        // Reattached servers (process: None): left running intentionally.
        if self.process.is_some() {
            tracing::debug!(
                directory = %self.directory.display(),
                "OpenCode server dropped, process will be killed"
            );
        }
    }
}

/// Pool of OpenCode server processes, one per working directory.
///
/// Uses deterministic ports derived from directory paths so that after a
/// spacebot restart, we can rediscover servers that are still running.
/// No file persistence needed -- just health-check the expected port.
pub struct OpenCodeServerPool {
    servers: Mutex<HashMap<PathBuf, Arc<Mutex<OpenCodeServer>>>>,
    opencode_path: String,
    permissions: OpenCodePermissions,
    max_servers: usize,
}

impl OpenCodeServerPool {
    /// Create a new server pool.
    pub fn new(
        opencode_path: impl Into<String>,
        permissions: OpenCodePermissions,
        max_servers: usize,
    ) -> Self {
        Self {
            servers: Mutex::new(HashMap::new()),
            opencode_path: opencode_path.into(),
            permissions,
            max_servers,
        }
    }

    /// Get or create a server for the given directory.
    ///
    /// On first access for a directory, checks the deterministic port for
    /// a server left over from a previous run. If one responds, reattaches.
    /// Otherwise spawns a new one. Subsequent calls reuse the pooled server.
    pub async fn get_or_create(
        &self,
        directory: &Path,
    ) -> anyhow::Result<Arc<Mutex<OpenCodeServer>>> {
        let canonical = directory
            .canonicalize()
            .with_context(|| format!("directory '{}' does not exist", directory.display()))?;

        let mut servers = self.servers.lock().await;

        // Check if we already have it in the pool
        if let Some(server) = servers.get(&canonical) {
            let mut guard = server.lock().await;
            if guard.is_alive().await {
                return Ok(Arc::clone(server));
            }

            tracing::warn!(
                directory = %canonical.display(),
                "OpenCode server found dead, restarting"
            );
            guard.restart().await?;
            return Ok(Arc::clone(server));
        }

        // Not in pool yet. Try reattaching to an existing server on the
        // deterministic port (left over from a previous spacebot run).
        if let Some(reattached) =
            OpenCodeServer::reattach(canonical.clone(), &self.opencode_path, &self.permissions)
                .await
        {
            let server = Arc::new(Mutex::new(reattached));
            servers.insert(canonical, Arc::clone(&server));
            return Ok(server);
        }

        // No existing server. Enforce pool limit and spawn fresh.
        if servers.len() >= self.max_servers {
            bail!(
                "OpenCode server pool limit reached ({}). Kill an existing server or increase the limit.",
                self.max_servers
            );
        }

        let server =
            OpenCodeServer::spawn(canonical.clone(), &self.opencode_path, &self.permissions)
                .await?;

        let server = Arc::new(Mutex::new(server));
        servers.insert(canonical, Arc::clone(&server));

        Ok(server)
    }

    /// Shut down all servers in the pool.
    pub async fn shutdown_all(&self) {
        let mut servers = self.servers.lock().await;
        for (directory, server) in servers.drain() {
            let mut guard = server.lock().await;
            guard.kill().await;
            tracing::info!(
                directory = %directory.display(),
                "OpenCode server shut down"
            );
        }
    }

    /// Number of active servers.
    pub async fn server_count(&self) -> usize {
        self.servers.lock().await.len()
    }
}

/// Derive a deterministic port from a directory path.
///
/// Uses a hash of the canonical path mapped into the range 10000-60000.
/// This means the same directory always gets the same port, so we can
/// rediscover servers after a restart without persisting state.
fn port_for_directory(directory: &Path) -> u16 {
    let mut hasher = DefaultHasher::new();
    directory.hash(&mut hasher);
    let hash = hasher.finish();
    // Map into range 10000..60000 (50000 ports)
    10000 + (hash % 50000) as u16
}
