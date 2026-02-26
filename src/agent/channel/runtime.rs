use super::*;

use rig::one_or_many::OneOrMany;

impl Channel {
    /// Run the channel event loop.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(channel_id = %self.id, "channel started");

        loop {
            // Compute next deadline from coalesce and retrigger timers
            let next_deadline = match (self.coalesce_deadline, self.retrigger_deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let sleep_duration = next_deadline
                .map(|deadline| {
                    let now = tokio::time::Instant::now();
                    if deadline > now {
                        deadline - now
                    } else {
                        std::time::Duration::from_millis(1)
                    }
                })
                .unwrap_or(std::time::Duration::from_secs(3600)); // Default long timeout if no deadline

            tokio::select! {
                Some(message) = self.message_rx.recv() => {
                    let config = self.deps.runtime_config.coalesce.load();
                    if self.should_coalesce(&message, &config) {
                        self.coalesce_buffer.push(message);
                        self.update_coalesce_deadline(&config).await;
                    } else {
                        // Flush any pending buffer before handling this message
                        if let Err(error) = self.flush_coalesce_buffer().await {
                            tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer");
                        }
                        if let Err(error) = self.handle_message(message).await {
                            tracing::error!(%error, channel_id = %self.id, "error handling message");
                        }
                    }
                }
                Ok(event) = self.event_rx.recv() => {
                    // Events bypass coalescing - flush buffer first if needed
                    if let Err(error) = self.flush_coalesce_buffer().await {
                        tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer");
                    }
                    if let Err(error) = self.handle_event(event).await {
                        tracing::error!(%error, channel_id = %self.id, "error handling event");
                    }
                }
                _ = tokio::time::sleep(sleep_duration), if next_deadline.is_some() => {
                    let now = tokio::time::Instant::now();
                    // Check coalesce deadline
                    if self.coalesce_deadline.is_some_and(|d| d <= now)
                        && let Err(error) = self.flush_coalesce_buffer().await
                    {
                        tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer on deadline");
                    }
                    // Check retrigger deadline
                    if self.retrigger_deadline.is_some_and(|d| d <= now) {
                        self.flush_pending_retrigger().await;
                    }
                }
                else => break,
            }
        }

        // Flush any remaining buffer before shutting down
        if let Err(error) = self.flush_coalesce_buffer().await {
            tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer on shutdown");
        }

        tracing::info!(channel_id = %self.id, "channel stopped");
        Ok(())
    }

    /// Determine if a message should be coalesced (batched with other messages).
    ///
    /// Returns false for:
    /// - System re-trigger messages (always process immediately)
    /// - Messages when coalescing is disabled
    /// - Messages in DMs when multi_user_only is true
    fn should_coalesce(
        &self,
        message: &InboundMessage,
        config: &crate::config::CoalesceConfig,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        if message.source == "system" {
            return false;
        }
        if config.multi_user_only && self.is_dm() {
            return false;
        }
        true
    }

    /// Check if this is a DM (direct message) conversation based on conversation_id.
    fn is_dm(&self) -> bool {
        // Check conversation_id pattern for DM indicators
        if let Some(ref conv_id) = self.conversation_id {
            conv_id.contains(":dm:")
                || conv_id.starts_with("discord:dm:")
                || conv_id.starts_with("slack:dm:")
        } else {
            // If no conversation_id set yet, default to not DM (safer)
            false
        }
    }

    /// Update the coalesce deadline based on buffer size and config.
    async fn update_coalesce_deadline(&mut self, config: &crate::config::CoalesceConfig) {
        let now = tokio::time::Instant::now();

        if let Some(first_message) = self.coalesce_buffer.first() {
            let elapsed_since_first =
                chrono::Utc::now().signed_duration_since(first_message.timestamp);
            let elapsed_millis = elapsed_since_first.num_milliseconds().max(0) as u64;

            let max_wait_ms = config.max_wait_ms;
            let debounce_ms = config.debounce_ms;

            // If we have enough messages to trigger coalescing (min_messages threshold)
            if self.coalesce_buffer.len() >= config.min_messages {
                // Cap at max_wait from the first message
                let remaining_wait_ms = max_wait_ms.saturating_sub(elapsed_millis);
                let max_deadline = now + std::time::Duration::from_millis(remaining_wait_ms);

                // If no deadline set yet, use debounce window
                // Otherwise, keep existing deadline (don't extend past max_wait)
                if self.coalesce_deadline.is_none() {
                    let new_deadline = now + std::time::Duration::from_millis(debounce_ms);
                    self.coalesce_deadline = Some(new_deadline.min(max_deadline));
                } else {
                    // Already have a deadline, cap it at max_wait
                    self.coalesce_deadline = self.coalesce_deadline.map(|d| d.min(max_deadline));
                }
            } else {
                // Not enough messages yet - set a short debounce window
                let new_deadline = now + std::time::Duration::from_millis(debounce_ms);
                self.coalesce_deadline = Some(new_deadline);
            }
        }
    }

    /// Flush the coalesce buffer by processing all buffered messages.
    ///
    /// If there's only one message, process it normally.
    /// If there are multiple messages, batch them into a single turn.
    async fn flush_coalesce_buffer(&mut self) -> Result<()> {
        if self.coalesce_buffer.is_empty() {
            return Ok(());
        }

        self.coalesce_deadline = None;

        let messages: Vec<InboundMessage> = std::mem::take(&mut self.coalesce_buffer);

        if messages.len() == 1 {
            // Single message - process normally
            let message = messages
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty iterator after length check"))?;
            self.handle_message(message).await
        } else {
            // Multiple messages - batch them
            self.handle_message_batch(messages).await
        }
    }

    /// Handle a process event (branch results, worker completions, status updates).
    async fn handle_event(&mut self, event: ProcessEvent) -> Result<()> {
        // Only process events targeted at this channel
        if !event_is_for_channel(&event, &self.id) {
            return Ok(());
        }

        // Update status block
        {
            let mut status = self.state.status_block.write().await;
            status.update(&event);
        }

        let mut should_retrigger = false;
        let mut retrigger_metadata = std::collections::HashMap::new();
        let run_logger = &self.state.process_run_logger;

        match &event {
            ProcessEvent::BranchStarted {
                branch_id,
                channel_id,
                description,
                reply_to_message_id,
                ..
            } => {
                run_logger.log_branch_started(channel_id, *branch_id, description);
                if let Some(message_id) = reply_to_message_id {
                    self.branch_reply_targets.insert(*branch_id, *message_id);
                }
            }
            ProcessEvent::BranchResult {
                branch_id,
                conclusion,
                ..
            } => {
                run_logger.log_branch_completed(*branch_id, conclusion);

                // Remove from active branches
                let mut branches = self.state.active_branches.write().await;
                branches.remove(branch_id);

                #[cfg(feature = "metrics")]
                crate::telemetry::Metrics::global()
                    .active_branches
                    .with_label_values(&[&*self.deps.agent_id])
                    .dec();

                // Memory persistence branches complete silently — no history
                // injection, no re-trigger. The work (memory saves) already
                // happened inside the branch via tool calls.
                if self.memory_persistence_branches.remove(branch_id) {
                    self.branch_reply_targets.remove(branch_id);
                    tracing::info!(branch_id = %branch_id, "memory persistence branch completed");
                } else {
                    // Regular branch: accumulate result for the next retrigger.
                    // The result text will be embedded directly in the retrigger
                    // message so the LLM knows exactly which process produced it.
                    self.pending_results.push(PendingResult {
                        process_type: "branch",
                        process_id: branch_id.to_string(),
                        result: conclusion.clone(),
                        success: true,
                    });
                    should_retrigger = true;

                    if let Some(message_id) = self.branch_reply_targets.remove(branch_id) {
                        retrigger_metadata.insert(
                            "discord_reply_to_message_id".to_string(),
                            serde_json::Value::from(message_id),
                        );
                    }

                    tracing::info!(branch_id = %branch_id, "branch result queued for retrigger");
                }
            }
            ProcessEvent::WorkerStarted {
                worker_id,
                channel_id,
                task,
                worker_type,
                ..
            } => {
                run_logger.log_worker_started(
                    channel_id.as_ref(),
                    *worker_id,
                    task,
                    worker_type,
                    &self.deps.agent_id,
                );
            }
            ProcessEvent::WorkerStatus {
                worker_id, status, ..
            } => {
                run_logger.log_worker_status(*worker_id, status);
            }
            ProcessEvent::WorkerComplete {
                worker_id,
                result,
                notify,
                success,
                ..
            } => {
                run_logger.log_worker_completed(*worker_id, result, *success);

                let mut workers = self.state.active_workers.write().await;
                workers.remove(worker_id);
                drop(workers);

                self.state.worker_handles.write().await.remove(worker_id);
                self.state.worker_inputs.write().await.remove(worker_id);

                if *notify {
                    // Accumulate result for the next retrigger instead of
                    // injecting into history as a fake user message.
                    self.pending_results.push(PendingResult {
                        process_type: "worker",
                        process_id: worker_id.to_string(),
                        result: result.clone(),
                        success: *success,
                    });
                    should_retrigger = true;
                }

                tracing::info!(worker_id = %worker_id, "worker completed, result queued for retrigger");
            }
            _ => {}
        }

        // Debounce retriggers: instead of firing immediately, set a deadline.
        // Multiple branch/worker completions within the debounce window are
        // coalesced into a single retrigger to prevent message spam.
        if should_retrigger {
            if self.retrigger_count >= MAX_RETRIGGERS_PER_TURN {
                tracing::warn!(
                    channel_id = %self.id,
                    retrigger_count = self.retrigger_count,
                    max = MAX_RETRIGGERS_PER_TURN,
                    "retrigger cap reached, suppressing further retriggers until next user message"
                );
                // Drain any pending results into history as assistant messages
                // so they aren't silently lost when the cap prevents a retrigger.
                if !self.pending_results.is_empty() {
                    let results = std::mem::take(&mut self.pending_results);
                    let mut history = self.state.history.write().await;
                    for r in &results {
                        let status = if r.success { "completed" } else { "failed" };
                        let summary = format!(
                            "[Background {} {} {}]: {}",
                            r.process_type, r.process_id, status, r.result
                        );
                        history.push(rig::message::Message::Assistant {
                            id: None,
                            content: OneOrMany::one(rig::message::AssistantContent::text(summary)),
                        });
                    }
                    tracing::info!(
                        channel_id = %self.id,
                        count = results.len(),
                        "injected capped results into history as assistant messages"
                    );
                }
            } else {
                self.pending_retrigger = true;
                // Merge metadata (later events override earlier ones for the same key)
                for (key, value) in retrigger_metadata {
                    self.pending_retrigger_metadata.insert(key, value);
                }
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
            }
        }

        Ok(())
    }

    /// Flush the pending retrigger: send a synthetic system message to re-trigger
    /// the channel LLM so it can process background results and respond.
    ///
    /// Drains `pending_results` and embeds them directly in the retrigger message
    /// so the LLM sees exactly which process(es) completed and what they returned.
    /// No result text is left floating in history as an ambiguous user message.
    ///
    /// Results are drained only after the synthetic message is queued
    /// successfully. On transient failures, retrigger state is kept and retried
    /// so background results are not silently lost.
    async fn flush_pending_retrigger(&mut self) {
        self.retrigger_deadline = None;

        if !self.pending_retrigger {
            return;
        }

        let Some(conversation_id) = &self.conversation_id else {
            tracing::warn!(
                channel_id = %self.id,
                "retrigger pending but conversation_id is missing, dropping pending results"
            );
            self.pending_retrigger = false;
            self.pending_retrigger_metadata.clear();
            self.pending_results.clear();
            return;
        };

        if self.pending_results.is_empty() {
            tracing::warn!(
                channel_id = %self.id,
                "retrigger fired but no pending results to relay"
            );
            self.pending_retrigger = false;
            self.pending_retrigger_metadata.clear();
            return;
        }

        let result_count = self.pending_results.len();

        // Build per-result summaries for the template.
        let result_items: Vec<_> = self
            .pending_results
            .iter()
            .map(|r| crate::prompts::engine::RetriggerResult {
                process_type: r.process_type.to_string(),
                process_id: r.process_id.clone(),
                success: r.success,
                result: r.result.clone(),
            })
            .collect();

        let retrigger_message = match self
            .deps
            .runtime_config
            .prompts
            .load()
            .render_system_retrigger(&result_items)
        {
            Ok(message) => message,
            Err(error) => {
                tracing::error!(
                    channel_id = %self.id,
                    %error,
                    "failed to render retrigger message, retrying"
                );
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
                return;
            }
        };

        // Build a compact summary of the results to inject into history after
        // a successful relay. This goes into metadata so handle_message can
        // pull it out without re-parsing the template.
        let result_summary = self
            .pending_results
            .iter()
            .map(|r| {
                let status = if r.success { "completed" } else { "failed" };
                // Truncate very long results for the history record — the user
                // already saw the full version via the reply tool.
                let truncated = if r.result.len() > 500 {
                    let boundary = r.result.floor_char_boundary(500);
                    format!("{}... [truncated]", &r.result[..boundary])
                } else {
                    r.result.clone()
                };
                format!(
                    "[{} {} {}]: {}",
                    r.process_type, r.process_id, status, truncated
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut metadata = self.pending_retrigger_metadata.clone();
        metadata.insert(
            "retrigger_result_summary".to_string(),
            serde_json::Value::String(result_summary),
        );

        let synthetic = InboundMessage {
            id: uuid::Uuid::new_v4().to_string(),
            source: "system".into(),
            conversation_id: conversation_id.clone(),
            sender_id: "system".into(),
            agent_id: None,
            content: crate::MessageContent::Text(retrigger_message),
            timestamp: chrono::Utc::now(),
            metadata,
            formatted_author: None,
        };
        match self.self_tx.try_send(synthetic) {
            Ok(()) => {
                self.retrigger_count += 1;
                tracing::info!(
                    channel_id = %self.id,
                    retrigger_count = self.retrigger_count,
                    result_count,
                    "firing debounced retrigger with {} result(s)",
                    result_count,
                );

                self.pending_retrigger = false;
                self.pending_retrigger_metadata.clear();
                self.pending_results.clear();
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    channel_id = %self.id,
                    result_count,
                    "channel self queue is full, retrying retrigger"
                );
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(
                    channel_id = %self.id,
                    "failed to re-trigger channel: queue is closed, dropping pending results"
                );
                self.pending_retrigger = false;
                self.pending_retrigger_metadata.clear();
                self.pending_results.clear();
            }
        }
    }

    /// Get the current status block as a string.
    pub async fn get_status(&self) -> String {
        let status = self.state.status_block.read().await;
        status.render()
    }

    /// Check if a memory persistence branch should be spawned based on message count.
    pub(super) async fn check_memory_persistence(&mut self) {
        let config = **self.deps.runtime_config.memory_persistence.load();
        if !config.enabled || config.message_interval == 0 {
            return;
        }

        if self.message_count < config.message_interval {
            return;
        }

        // Reset counter before spawning so subsequent messages don't pile up
        self.message_count = 0;

        match spawn_memory_persistence_branch(&self.state, &self.deps).await {
            Ok(branch_id) => {
                self.memory_persistence_branches.insert(branch_id);
                tracing::info!(
                    channel_id = %self.id,
                    branch_id = %branch_id,
                    interval = config.message_interval,
                    "memory persistence branch spawned"
                );
            }
            Err(error) => {
                tracing::warn!(
                    channel_id = %self.id,
                    %error,
                    "failed to spawn memory persistence branch"
                );
            }
        }
    }
}

/// Check if a ProcessEvent is targeted at a specific channel.
///
/// Events from branches and workers carry a channel_id. We only process events
/// that originated from this channel — otherwise broadcast events from one
/// channel's workers would leak into sibling channels (e.g. threads).
fn event_is_for_channel(event: &ProcessEvent, channel_id: &ChannelId) -> bool {
    match event {
        ProcessEvent::BranchResult {
            channel_id: event_channel,
            ..
        } => event_channel == channel_id,
        ProcessEvent::WorkerComplete {
            channel_id: event_channel,
            ..
        } => event_channel.as_ref() == Some(channel_id),
        ProcessEvent::WorkerStatus {
            channel_id: event_channel,
            ..
        } => event_channel.as_ref() == Some(channel_id),
        // Status block updates, tool events, etc. — match on agent_id which
        // is already filtered by the event bus subscription. Let them through.
        _ => true,
    }
}
