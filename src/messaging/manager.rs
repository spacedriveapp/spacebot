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

    /// Start all registered adapters and return the merged inbound stream.
    ///
    /// Each adapter's stream is forwarded into a shared channel, so adapters
    /// added later via `register_and_start` feed into the same stream.
    pub async fn start(&self) -> crate::Result<InboundStream> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            let stream = adapter
                .start()
                .await
                .with_context(|| format!("failed to start adapter '{name}'"))?;
            Self::spawn_forwarder(name.clone(), stream, self.fan_in_tx.clone());
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

    /// Route a response back to the correct adapter based on message source.
    pub async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let adapters = self.adapters.read().await;
        let adapter = adapters
            .get(&message.source)
            .with_context(|| format!("no messaging adapter named '{}'", message.source))?;
        adapter.respond(message, response).await
    }

    /// Route a status update to the correct adapter.
    pub async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        let adapters = self.adapters.read().await;
        let adapter = adapters
            .get(&message.source)
            .with_context(|| format!("no messaging adapter named '{}'", message.source))?;
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
        let adapter = adapters
            .get(&message.source)
            .with_context(|| format!("no messaging adapter named '{}'", message.source))?;
        adapter.fetch_history(message, limit).await
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
