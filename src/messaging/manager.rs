//! MessagingManager: Fan-in and routing for all adapters.

use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging, MessagingDyn};
use crate::{InboundMessage, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use futures::StreamExt as _;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// Manages all messaging adapters with support for runtime addition.
///
/// Adapters forward messages into a shared mpsc channel, so new adapters
/// can be registered after `start()` without replacing the inbound stream.
pub struct MessagingManager {
    adapters: RwLock<HashMap<String, Arc<dyn MessagingDyn>>>,
    /// Sender side of the fan-in channel. Cloned for each adapter's forwarding task.
    fan_in_tx: mpsc::Sender<InboundMessage>,
    /// Receiver side, taken once by `start()`.
    fan_in_rx: RwLock<Option<mpsc::Receiver<InboundMessage>>>,
}

impl MessagingManager {
    pub fn new() -> Self {
        let (fan_in_tx, fan_in_rx) = mpsc::channel(512);
        Self {
            adapters: RwLock::new(HashMap::new()),
            fan_in_tx,
            fan_in_rx: RwLock::new(Some(fan_in_rx)),
        }
    }

    /// Register an adapter (before start). Use `register_and_start` for runtime addition.
    pub async fn register(&self, adapter: impl Messaging) {
        let name = adapter.name().to_string();
        tracing::info!(adapter = %name, "registered messaging adapter");
        self.adapters.write().await.insert(name, Arc::new(adapter));
    }

    /// Register a pre-wrapped adapter that the caller retains a handle to.
    pub async fn register_shared(&self, adapter: Arc<impl Messaging>) {
        let name = adapter.name().to_string();
        tracing::info!(adapter = %name, "registered messaging adapter (shared)");
        self.adapters.write().await.insert(name, adapter);
    }

    /// Maximum number of retry attempts for failed adapters before giving up.
    const MAX_RETRY_ATTEMPTS: u32 = 12;

    /// Start all registered adapters and return the merged inbound stream.
    ///
    /// Each adapter's stream is forwarded into a shared channel, so adapters
    /// added later via `register_and_start` feed into the same stream.
    /// Adapters that fail to start (e.g. due to network not being ready) are
    /// retried in the background with exponential backoff.
    pub async fn start(&self) -> crate::Result<InboundStream> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            match adapter.start().await {
                Ok(stream) => Self::spawn_forwarder(name.clone(), stream, self.fan_in_tx.clone()),
                Err(error) => {
                    tracing::warn!(
                        adapter = %name,
                        %error,
                        "adapter failed to start, will retry in background"
                    );
                    Self::spawn_retry_task(
                        name.clone(),
                        Arc::clone(adapter),
                        self.fan_in_tx.clone(),
                    );
                }
            }
        }
        drop(adapters);

        let receiver = self
            .fan_in_rx
            .write()
            .await
            .take()
            .context("start() already called")?;

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            receiver,
        )))
    }

    /// Register and start a new adapter at runtime.
    ///
    /// The adapter's inbound stream is forwarded into the existing fan-in
    /// channel, so the main loop's stream receives messages without any
    /// stream replacement or restart.
    pub async fn register_and_start(&self, adapter: impl Messaging) -> crate::Result<()> {
        let name = adapter.name().to_string();

        // Shut down existing adapter with the same name if present
        {
            let adapters = self.adapters.read().await;
            if let Some(existing) = adapters.get(&name) {
                tracing::info!(adapter = %name, "shutting down existing adapter before replacement");
                if let Err(error) = existing.shutdown().await {
                    tracing::warn!(adapter = %name, %error, "failed to shut down existing adapter");
                }
            }
        }

        let adapter: Arc<dyn MessagingDyn> = Arc::new(adapter);

        let stream = adapter
            .start()
            .await
            .with_context(|| format!("failed to start adapter '{name}'"))?;
        Self::spawn_forwarder(name.clone(), stream, self.fan_in_tx.clone());

        self.adapters.write().await.insert(name.clone(), adapter);

        tracing::info!(adapter = %name, "adapter registered and started at runtime");
        Ok(())
    }

    /// Returns true if an adapter with this name is currently registered.
    pub async fn has_adapter(&self, name: &str) -> bool {
        self.adapters.read().await.contains_key(name)
    }

    /// Returns true if any adapter exists for the given platform.
    pub async fn has_platform_adapters(&self, platform: &str) -> bool {
        let prefix = format!("{platform}:");
        self.adapters
            .read()
            .await
            .keys()
            .any(|name| name == platform || name.starts_with(&prefix))
    }

    /// List registered adapter runtime keys.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.adapters.read().await.keys().cloned().collect()
    }

    /// Spawn a background task that retries starting a failed adapter with exponential backoff.
    ///
    /// Once the adapter starts successfully, its stream is forwarded into the
    /// existing fan-in channel — the same mechanism used by `register_and_start`.
    fn spawn_retry_task(
        name: String,
        adapter: Arc<dyn MessagingDyn>,
        fan_in_tx: mpsc::Sender<InboundMessage>,
    ) {
        tokio::spawn(async move {
            let mut delay = std::time::Duration::from_secs(5);
            let max_delay = std::time::Duration::from_secs(60);

            for attempt in 1..=Self::MAX_RETRY_ATTEMPTS {
                tokio::time::sleep(delay).await;

                match adapter.start().await {
                    Ok(stream) => {
                        tracing::info!(
                            adapter = %name,
                            attempt,
                            "adapter started successfully after retry"
                        );
                        Self::spawn_forwarder(name, stream, fan_in_tx);
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(
                            adapter = %name,
                            attempt,
                            max_attempts = Self::MAX_RETRY_ATTEMPTS,
                            %error,
                            "adapter retry failed, next attempt in {:?}",
                            delay.min(max_delay)
                        );
                    }
                }

                delay = (delay * 2).min(max_delay);
            }

            tracing::error!(
                adapter = %name,
                "adapter failed to start after {} attempts, giving up",
                Self::MAX_RETRY_ATTEMPTS
            );
        });
    }

    /// Spawn a task that forwards messages from an adapter stream into the fan-in channel.
    fn spawn_forwarder(
        name: String,
        mut stream: InboundStream,
        fan_in_tx: mpsc::Sender<InboundMessage>,
    ) {
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                if fan_in_tx.send(message).await.is_err() {
                    tracing::warn!(adapter = %name, "fan-in channel closed, stopping forwarder");
                    break;
                }
            }
            tracing::info!(adapter = %name, "adapter stream ended");
        });
    }

    /// Inject a message directly into the fan-in channel, bypassing adapter streams.
    pub async fn inject_message(&self, message: InboundMessage) -> crate::Result<()> {
        self.fan_in_tx
            .send(message)
            .await
            .map_err(|_| crate::error::Error::Other(anyhow::anyhow!("fan-in channel closed")))
    }

    /// Route a response back to the correct adapter based on message source.
    pub async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let adapters = self.adapters.read().await;
        let adapter_key = message.adapter_key();
        let adapter = adapters
            .get(adapter_key)
            .with_context(|| format!("no messaging adapter named '{}'", adapter_key))?;
        adapter.respond(message, response).await
    }

    /// Route a status update to the correct adapter.
    pub async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        let adapters = self.adapters.read().await;
        let adapter_key = message.adapter_key();
        let adapter = adapters
            .get(adapter_key)
            .with_context(|| format!("no messaging adapter named '{}'", adapter_key))?;
        adapter.send_status(message, status).await
    }

    /// Send a proactive message through a specific adapter.
    pub async fn broadcast(
        &self,
        adapter_name: &str,
        target: &str,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let adapters = self.adapters.read().await;
        let adapter = adapters
            .get(adapter_name)
            .with_context(|| format!("no messaging adapter named '{adapter_name}'"))?;
        adapter.broadcast(target, response).await
    }

    /// Fetch recent message history from the platform for context backfill.
    pub async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let adapters = self.adapters.read().await;
        let adapter_key = message.adapter_key();
        let adapter = adapters
            .get(adapter_key)
            .with_context(|| format!("no messaging adapter named '{}'", adapter_key))?;
        adapter.fetch_history(message, limit).await
    }

    /// Remove and shut down a single adapter by name.
    pub async fn remove_adapter(&self, name: &str) -> crate::Result<()> {
        let adapter = self.adapters.write().await.remove(name);
        if let Some(adapter) = adapter {
            adapter.shutdown().await?;
            tracing::info!(adapter = %name, "adapter removed and shut down");
        }
        Ok(())
    }

    /// Remove and shut down all adapters for a platform (default + named).
    pub async fn remove_platform_adapters(&self, platform: &str) -> crate::Result<()> {
        let mut names = Vec::new();
        {
            let adapters = self.adapters.read().await;
            let prefix = format!("{platform}:");
            for name in adapters.keys() {
                if name == platform || name.starts_with(&prefix) {
                    names.push(name.clone());
                }
            }
        }

        for name in names {
            self.remove_adapter(&name).await?;
        }

        Ok(())
    }

    /// Shut down all adapters gracefully.
    pub async fn shutdown(&self) {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            if let Err(error) = adapter.shutdown().await {
                tracing::warn!(adapter = %name, %error, "failed to shut down adapter");
            }
        }
    }
}

impl Default for MessagingManager {
    fn default() -> Self {
        Self::new()
    }
}
