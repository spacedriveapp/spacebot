//! MessagingManager: Fan-in and routing for all adapters.

use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging, MessagingDyn};
use crate::{InboundMessage, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use futures::StreamExt as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;

#[cfg(test)]
const INITIAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);
#[cfg(not(test))]
const INITIAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
const MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(not(test))]
const MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

struct AdapterRuntime {
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub struct ConfiguredAdapter {
    pub name: String,
    pub fingerprint: String,
    adapter: Arc<dyn MessagingDyn>,
}

impl ConfiguredAdapter {
    pub fn new(adapter: impl Messaging, fingerprint: impl Into<String>) -> Self {
        let name = adapter.name().to_string();
        Self {
            name,
            fingerprint: fingerprint.into(),
            adapter: Arc::new(adapter),
        }
    }
}

/// Manages all messaging adapters with support for runtime addition.
///
/// Adapters forward messages into a shared mpsc channel, so new adapters
/// can be registered after `start()` without replacing the inbound stream.
pub struct MessagingManager {
    adapters: RwLock<HashMap<String, Arc<dyn MessagingDyn>>>,
    runtimes: RwLock<HashMap<String, AdapterRuntime>>,
    configured_fingerprints: RwLock<HashMap<String, String>>,
    lifecycle_mutex: tokio::sync::Mutex<()>,
    /// Sender side of the fan-in channel. Cloned for each adapter's forwarding task.
    fan_in_tx: mpsc::Sender<InboundMessage>,
    /// Receiver side, taken once by `start()`.
    fan_in_rx: RwLock<Option<mpsc::Receiver<InboundMessage>>>,
    started: AtomicBool,
}

impl MessagingManager {
    pub fn new() -> Self {
        let (fan_in_tx, fan_in_rx) = mpsc::channel(512);
        Self {
            adapters: RwLock::new(HashMap::new()),
            runtimes: RwLock::new(HashMap::new()),
            configured_fingerprints: RwLock::new(HashMap::new()),
            lifecycle_mutex: tokio::sync::Mutex::new(()),
            fan_in_tx,
            fan_in_rx: RwLock::new(Some(fan_in_rx)),
            started: AtomicBool::new(false),
        }
    }

    /// Register an adapter (before start). Use `register_and_start` for runtime addition.
    pub async fn register(&self, adapter: impl Messaging) {
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        let name = adapter.name().to_string();
        let adapter: Arc<dyn MessagingDyn> = Arc::new(adapter);
        tracing::info!(adapter = %name, "registered messaging adapter");
        let started = self.started.load(Ordering::SeqCst);
        let old_adapter = self
            .adapters
            .write()
            .await
            .insert(name.clone(), Arc::clone(&adapter));
        if started {
            self.stop_runtime(&name, old_adapter, false).await.ok();
        }
        if started && let Err(error) = self.start_runtime(name.clone(), adapter, false).await {
            tracing::warn!(adapter = %name, %error, "failed to start adapter registered after manager start");
        }
    }

    /// Register a pre-wrapped adapter that the caller retains a handle to.
    pub async fn register_shared(&self, adapter: Arc<impl Messaging>) {
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        let name = adapter.name().to_string();
        tracing::info!(adapter = %name, "registered messaging adapter (shared)");
        let adapter: Arc<dyn MessagingDyn> = adapter;
        let started = self.started.load(Ordering::SeqCst);
        let old_adapter = self
            .adapters
            .write()
            .await
            .insert(name.clone(), Arc::clone(&adapter));
        if started {
            self.stop_runtime(&name, old_adapter, false).await.ok();
        }
        if started && let Err(error) = self.start_runtime(name.clone(), adapter, false).await {
            tracing::warn!(adapter = %name, %error, "failed to start shared adapter registered after manager start");
        }
    }

    /// Start all registered adapters and return the merged inbound stream.
    ///
    /// Each adapter's stream is forwarded into a shared channel, so adapters
    /// added later via `register_and_start` feed into the same stream.
    /// Adapters that fail to start (e.g. due to network not being ready) are
    /// retried in the background with exponential backoff.
    pub async fn start(&self) -> crate::Result<InboundStream> {
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        self.started.store(true, Ordering::SeqCst);
        let adapters = self
            .adapters
            .read()
            .await
            .iter()
            .map(|(name, adapter)| (name.clone(), Arc::clone(adapter)))
            .collect::<Vec<_>>();

        let receiver = self
            .fan_in_rx
            .write()
            .await
            .take()
            .context("start() already called")?;

        for (name, adapter) in adapters {
            self.start_runtime(name, adapter, false).await?;
        }

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
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        let name = adapter.name().to_string();
        let adapter: Arc<dyn MessagingDyn> = Arc::new(adapter);
        let old_adapter = self
            .adapters
            .write()
            .await
            .insert(name.clone(), Arc::clone(&adapter));
        self.stop_runtime(&name, old_adapter, false).await.ok();
        self.start_runtime(name.clone(), adapter, true).await?;
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

    pub async fn reconcile_configured(&self, desired: Vec<ConfiguredAdapter>) -> crate::Result<()> {
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        let desired_names = desired
            .iter()
            .map(|adapter| adapter.name.clone())
            .collect::<std::collections::HashSet<_>>();
        let current_configured = self
            .adapters
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        let mut first_error = None;
        for name in current_configured {
            if !desired_names.contains(&name)
                && let Err(error) = self.remove_adapter(&name).await
            {
                tracing::warn!(adapter = %name, %error, "failed to remove stale adapter during reconciliation");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        let current_fingerprints = self.configured_fingerprints.read().await.clone();
        for desired_adapter in desired {
            let is_unchanged = current_fingerprints
                .get(&desired_adapter.name)
                .is_some_and(|fingerprint| fingerprint == &desired_adapter.fingerprint);
            if is_unchanged {
                continue;
            }

            let old_adapter = self.adapters.write().await.insert(
                desired_adapter.name.clone(),
                Arc::clone(&desired_adapter.adapter),
            );
            self.stop_runtime(&desired_adapter.name, old_adapter, false)
                .await
                .ok();
            let replace_result = self
                .start_runtime(
                    desired_adapter.name.clone(),
                    Arc::clone(&desired_adapter.adapter),
                    true,
                )
                .await;
            self.configured_fingerprints.write().await.insert(
                desired_adapter.name.clone(),
                desired_adapter.fingerprint.clone(),
            );
            if let Err(error) = replace_result {
                tracing::warn!(
                    adapter = %desired_adapter.name,
                    %error,
                    "adapter reconciliation failed; supervisor will keep retrying"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn spawn_supervisor(
        name: String,
        adapter: Arc<dyn MessagingDyn>,
        fan_in_tx: mpsc::Sender<InboundMessage>,
        mut shutdown_rx: watch::Receiver<bool>,
        initial_stream: Option<InboundStream>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut next_retry_delay = INITIAL_RETRY_DELAY;
            let mut initial_stream = initial_stream;
            #[cfg(test)]
            let healthy_stream_duration = std::time::Duration::from_millis(20);
            #[cfg(not(test))]
            let healthy_stream_duration = std::time::Duration::from_secs(2);

            loop {
                let mut stream = if let Some(stream) = initial_stream.take() {
                    stream
                } else {
                    let start_result = tokio::select! {
                        _ = shutdown_rx.changed() => break,
                        result = adapter.start() => result,
                    };

                    match start_result {
                        Ok(stream) => {
                            tracing::info!(adapter = %name, "adapter started successfully");
                            next_retry_delay = INITIAL_RETRY_DELAY;
                            stream
                        }
                        Err(error) => {
                            tracing::warn!(
                                adapter = %name,
                                %error,
                                retry_delay = ?next_retry_delay,
                                "adapter start failed, retrying in background"
                            );
                            let current_delay = next_retry_delay;
                            next_retry_delay = (next_retry_delay * 2).min(MAX_RETRY_DELAY);
                            tokio::select! {
                                _ = shutdown_rx.changed() => break,
                                _ = tokio::time::sleep(current_delay) => continue,
                            }
                        }
                    }
                };

                let started_at = std::time::Instant::now();
                let mut observed_message = false;
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => {
                            tracing::info!(adapter = %name, "adapter supervisor shutting down");
                            return;
                        }
                        maybe_message = stream.next() => {
                            match maybe_message {
                                Some(message) => {
                                    observed_message = true;
                                    next_retry_delay = INITIAL_RETRY_DELAY;
                                    if fan_in_tx.send(message).await.is_err() {
                                        tracing::warn!(adapter = %name, "fan-in channel closed, stopping supervisor");
                                        return;
                                    }
                                }
                                None => {
                                    if !observed_message && started_at.elapsed() < healthy_stream_duration {
                                        let current_delay = next_retry_delay;
                                        next_retry_delay = (next_retry_delay * 2).min(MAX_RETRY_DELAY);
                                        tracing::warn!(
                                            adapter = %name,
                                            retry_delay = ?current_delay,
                                            "adapter stream ended before becoming healthy, backing off before restart"
                                        );
                                        tokio::select! {
                                            _ = shutdown_rx.changed() => return,
                                            _ = tokio::time::sleep(current_delay) => {}
                                        }
                                    } else {
                                        next_retry_delay = INITIAL_RETRY_DELAY;
                                        tracing::warn!(adapter = %name, "adapter stream ended, restarting");
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    async fn start_runtime(
        &self,
        name: String,
        adapter: Arc<dyn MessagingDyn>,
        surface_start_error: bool,
    ) -> crate::Result<()> {
        if !self.started.load(Ordering::SeqCst) {
            return Ok(());
        }

        let start_result = adapter.start().await;
        let initial_stream = match start_result {
            Ok(stream) => stream,
            Err(error) => {
                self.install_supervisor(name.clone(), Arc::clone(&adapter), None)
                    .await;
                tracing::warn!(adapter = %name, "scheduled background retry for failed adapter start");
                if surface_start_error {
                    return Err(anyhow::anyhow!("failed to start adapter '{name}': {error}").into());
                }
                return Ok(());
            }
        };

        self.install_supervisor(name, adapter, Some(initial_stream))
            .await;
        Ok(())
    }

    async fn install_supervisor(
        &self,
        name: String,
        adapter: Arc<dyn MessagingDyn>,
        initial_stream: Option<InboundStream>,
    ) -> watch::Sender<bool> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = Self::spawn_supervisor(
            name.clone(),
            adapter,
            self.fan_in_tx.clone(),
            shutdown_rx,
            initial_stream,
        );
        self.runtimes.write().await.insert(
            name,
            AdapterRuntime {
                shutdown_tx: shutdown_tx.clone(),
                task,
            },
        );
        shutdown_tx
    }

    async fn stop_runtime(
        &self,
        name: &str,
        adapter: Option<Arc<dyn MessagingDyn>>,
        propagate_shutdown_error: bool,
    ) -> crate::Result<()> {
        let runtime = self.runtimes.write().await.remove(name);
        if let Some(runtime) = runtime.as_ref() {
            runtime.shutdown_tx.send(true).ok();
        }

        let mut shutdown_error = None;
        if let Some(adapter) = adapter
            && let Err(error) = adapter.shutdown().await
        {
            tracing::warn!(adapter = %name, %error, "failed to shut down adapter");
            shutdown_error = Some(error);
        }

        if let Some(runtime) = runtime {
            runtime.task.abort();
            match runtime.task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    tracing::warn!(adapter = %name, %error, "adapter supervisor join failed");
                }
            }
        }

        if propagate_shutdown_error && let Some(error) = shutdown_error {
            return Err(error);
        }

        Ok(())
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
        let _lifecycle_guard = self.lifecycle_mutex.lock().await;
        let adapter = self.adapters.write().await.remove(name);
        self.configured_fingerprints.write().await.remove(name);
        self.stop_runtime(name, adapter, true).await?;
        tracing::info!(adapter = %name, "adapter removed and shut down");
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
        let names = self
            .adapters
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for name in names {
            if let Err(error) = self.remove_adapter(&name).await {
                tracing::warn!(adapter = %name, %error, "failed to shut down adapter");
            }
        }
    }

    pub async fn seed_configured_fingerprints_from_registered(&self) {
        let names = self
            .adapters
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut fingerprints = self.configured_fingerprints.write().await;
        for name in names {
            fingerprints.entry(name).or_insert_with(String::new);
        }
    }
}

impl Default for MessagingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::MessagingManager;
    use crate::messaging::traits::{InboundStream, Messaging};
    use crate::{InboundMessage, MessageContent};
    use futures::StreamExt as _;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    enum StartPlan {
        Error(&'static str),
        Stream(Vec<&'static str>),
        Pending,
    }

    struct TestAdapter {
        name: String,
        plans: std::sync::Mutex<Vec<StartPlan>>,
        start_calls: AtomicUsize,
        shutdown_calls: AtomicUsize,
        start_signal_tx: tokio::sync::watch::Sender<usize>,
    }

    impl TestAdapter {
        fn new(name: &str, plans: Vec<StartPlan>) -> Self {
            let (start_signal_tx, _start_signal_rx) = tokio::sync::watch::channel(0);
            Self {
                name: name.to_string(),
                plans: std::sync::Mutex::new(plans),
                start_calls: AtomicUsize::new(0),
                shutdown_calls: AtomicUsize::new(0),
                start_signal_tx,
            }
        }

        fn subscribe_start_calls(&self) -> tokio::sync::watch::Receiver<usize> {
            self.start_signal_tx.subscribe()
        }
    }

    impl Messaging for TestAdapter {
        fn name(&self) -> &str {
            &self.name
        }

        async fn start(&self) -> crate::Result<InboundStream> {
            let start_count = self.start_calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.start_signal_tx.send_replace(start_count);
            let plan = self.plans.lock().expect("plans lock").remove(0);
            match plan {
                StartPlan::Error(error) => Err(anyhow::anyhow!(error).into()),
                StartPlan::Stream(messages) => {
                    let adapter_name = self.name.clone();
                    let stream =
                        tokio_stream::iter(messages.into_iter().map(move |text| InboundMessage {
                            id: uuid::Uuid::new_v4().to_string(),
                            source: adapter_name.clone(),
                            adapter: Some(adapter_name.clone()),
                            conversation_id: format!("{adapter_name}:test"),
                            sender_id: "sender".to_string(),
                            agent_id: None,
                            content: MessageContent::Text(text.to_string()),
                            timestamp: chrono::Utc::now(),
                            metadata: HashMap::new(),
                            formatted_author: None,
                        }));
                    Ok(Box::pin(stream))
                }
                StartPlan::Pending => Ok(Box::pin(futures::stream::pending())),
            }
        }

        async fn respond(
            &self,
            _message: &InboundMessage,
            _response: crate::OutboundResponse,
        ) -> crate::Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> crate::Result<()> {
            Ok(())
        }

        async fn shutdown(&self) -> crate::Result<()> {
            self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn supervisor_retries_after_start_failure_and_stream_end() {
        let manager = MessagingManager::new();
        let adapter = Arc::new(TestAdapter::new(
            "test",
            vec![
                StartPlan::Error("boom"),
                StartPlan::Stream(vec!["first"]),
                StartPlan::Stream(vec!["second"]),
                StartPlan::Pending,
            ],
        ));
        let mut start_calls = adapter.subscribe_start_calls();
        manager.register_shared(adapter.clone()).await;
        let mut stream = manager.start().await.expect("start manager");

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("timeout")
            .expect("message");
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("timeout")
            .expect("message");

        match first.content {
            MessageContent::Text(text) => assert_eq!(text, "first"),
            other => panic!("unexpected message content: {other:?}"),
        }
        match second.content {
            MessageContent::Text(text) => assert_eq!(text, "second"),
            other => panic!("unexpected message content: {other:?}"),
        }
        wait_for_start_calls(&mut start_calls, 3).await;
    }

    #[tokio::test]
    async fn removing_adapter_stops_retry_supervisor() {
        let manager = MessagingManager::new();
        let adapter = Arc::new(TestAdapter::new(
            "retrying",
            vec![
                StartPlan::Error("boom"),
                StartPlan::Error("boom"),
                StartPlan::Error("boom"),
                StartPlan::Pending,
            ],
        ));
        let mut start_calls = adapter.subscribe_start_calls();
        manager.register_shared(adapter.clone()).await;
        let _stream = manager.start().await.expect("start manager");

        wait_for_start_calls(&mut start_calls, 2).await;
        let before_remove = *start_calls.borrow_and_update();
        manager
            .remove_adapter("retrying")
            .await
            .expect("remove adapter");
        let no_more_starts =
            tokio::time::timeout(std::time::Duration::from_millis(200), start_calls.changed())
                .await;

        assert!(
            no_more_starts.is_err(),
            "unexpected extra start after remove"
        );
        assert_eq!(before_remove, adapter.start_calls.load(Ordering::SeqCst));
        assert!(adapter.shutdown_calls.load(Ordering::SeqCst) >= 1);
    }

    async fn wait_for_start_calls(
        receiver: &mut tokio::sync::watch::Receiver<usize>,
        expected: usize,
    ) {
        if *receiver.borrow() >= expected {
            return;
        }

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                receiver.changed().await.expect("start signal");
                if *receiver.borrow() >= expected {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for start calls");
    }
}
