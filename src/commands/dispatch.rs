//! Inbound-router command dispatch.
//!
//! Runs in the messaging layer, before a message reaches a channel's queue:
//!
//! 1. parse — unknown `/words` flow to the model as ordinary text
//! 2. usage errors reply immediately
//! 3. access check against the scope's authority list
//! 4. Control commands execute on the control plane — no inbound message is
//!    created, so they work even while the channel is mid-turn
//! 5. Agent commands are rewritten to `MessageContent::Command` and injected;
//!    busy policy applies, and a queued command is always acknowledged
//!
//! Replies (usage, denials, control output, busy acks) are ephemeral where
//! the platform supports it and degrade to plain messages elsewhere.

use super::access::{self, AccessContext, access_allows};
use super::control::ControlPlane;
use super::registry::{BusyPolicy, CommandHandler, ControlAction, ParseResult, Surface};
use crate::messaging::MessagingManager;
use crate::{InboundMessage, MessageContent, OutboundResponse};
use std::sync::Arc;

/// Outcome of router-side dispatch for one inbound message.
pub enum Dispatch {
    /// Not a command: forward on the normal message path, untouched.
    Forward,
    /// Fully handled here (control executed, usage/denial/reject replied).
    /// The message must not be forwarded.
    Handled,
    /// Agent command: the message content has been rewritten to
    /// `MessageContent::Command`; forward it to the channel.
    ForwardCommand,
}

/// Facts about the matched scope the router already holds.
pub struct DispatchScope<'a> {
    /// Authority list from the matched binding, when one matched and set
    /// the key. An explicit empty list opens the scope; `None` falls back
    /// to the adapter default.
    pub binding_authority: Option<&'a [String]>,
    /// Adapter-instance default authority lists.
    pub adapter_defaults: &'a access::AdapterAuthorityDefaults,
    /// Settings defaults from the matched binding.
    pub binding_settings: Option<&'a crate::conversation::ConversationSettings>,
    /// Whether the target channel currently has a turn in flight.
    pub turn_active: bool,
}

/// Classify and, where possible, fully handle a slash command. Replies are
/// sent on spawned tasks so the router loop never blocks on an adapter.
/// Metadata key carrying the sender's resolved authority to the channel.
///
/// Authority resolves here, where the binding and adapter config live, and
/// travels with the message: everything downstream that must respect it —
/// the channel's own command path, authority-gated tool registration — reads
/// this rather than re-deriving a check it has no configuration for. Absent
/// means no authority, so a path that never passed through the router cannot
/// inherit one.
pub const AUTHORITY_METADATA_KEY: &str = "sender_is_authority";

/// Resolve the sender's authority for the scope this message arrived in and
/// record it on the message, returning the verdict.
///
/// Every non-system inbound message gets one, whether or not the router goes
/// on to handle it as a command.
pub(crate) fn stamp_authority(message: &mut InboundMessage, scope: &DispatchScope<'_>) -> bool {
    let is_authority = AccessContext {
        binding_authority: scope.binding_authority,
        adapter_default: scope.adapter_defaults.for_adapter(message.adapter_key()),
        sender_id: &message.sender_id,
        sender_login: message
            .metadata
            .get("twitch_user_login")
            .and_then(|value| value.as_str()),
    }
    .is_authority();

    message.metadata.insert(
        AUTHORITY_METADATA_KEY.to_string(),
        serde_json::Value::Bool(is_authority),
    );
    is_authority
}

/// Whether the router resolved this sender as holding authority in the scope
/// the message arrived in.
pub fn sender_is_authority(message: &InboundMessage) -> bool {
    message
        .metadata
        .get(AUTHORITY_METADATA_KEY)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub async fn dispatch_inbound(
    message: &mut InboundMessage,
    scope: DispatchScope<'_>,
    deps: &crate::AgentDeps,
    messaging: &Arc<MessagingManager>,
) -> Dispatch {
    if message.source == "system" {
        return Dispatch::Forward;
    }
    let surface = Surface::from_source(&message.source);
    // Stamped for every non-system message, ahead of both the content match
    // and the parse. The channel builds its command text from media captions
    // too, and registers authority-gated tools per turn, so anything this
    // function declines to handle still has to carry the verdict.
    let is_authority = stamp_authority(message, &scope);

    let text = match &message.content {
        MessageContent::Text(text) => text.clone(),
        // Command content arrives from surfaces that parse client-side
        // (Discord interactions, Slack subcommands, the portal palette); it
        // renders as "/name args" so the shared parser revalidates it.
        MessageContent::Command { .. } => message.content.to_string(),
        // A media caption can carry a command, but the router does not
        // dispatch it: attachments have to reach the channel with the
        // message. The channel's own command path handles it, gated on the
        // verdict stamped above.
        _ => return Dispatch::Forward,
    };

    let bot_username = message
        .metadata
        .get("telegram_bot_username")
        .and_then(|value| value.as_str());
    let parsed = match super::REGISTRY.parse_addressed(&text, bot_username) {
        ParseResult::NotACommand => return Dispatch::Forward,
        ParseResult::Usage(_, usage) => {
            reply_ephemeral(messaging, deps, message, usage);
            return Dispatch::Handled;
        }
        ParseResult::Command(parsed) => parsed,
    };

    if !access_allows(parsed.def, is_authority) {
        reply_ephemeral(
            messaging,
            deps,
            message,
            access::denial_text(parsed.def, surface),
        );
        return Dispatch::Handled;
    }

    match parsed.def.handler {
        CommandHandler::Control(action) => {
            let plane = ControlPlane {
                deps: deps.clone(),
                conversation_id: message.conversation_id.clone(),
                adapter: message.adapter_key().to_string(),
                binding_settings: scope.binding_settings.cloned(),
                surface,
                is_authority,
                is_portal: message.adapter.as_deref() == Some("portal"),
            };
            match action {
                // State-mutating control commands execute inline. The router
                // handles inbound messages sequentially, so awaiting here
                // serializes mode changes in delivery order — an earlier
                // /quiet can't land its store write and live update after a
                // later /active. The work is a local SQLite roundtrip plus
                // an in-memory cell update; only the reply delivery goes
                // over the network, and that stays on a spawned task.
                ControlAction::SetResponseMode(_)
                | ControlAction::SetHome
                | ControlAction::SetPause
                | ControlAction::AutonomyOn
                | ControlAction::AutonomyOff => {
                    let reply = plane.execute(action, &parsed.args).await;
                    reply_ephemeral(messaging, deps, message, reply);
                }
                // Read-only control commands stay off the router's critical
                // path entirely.
                ControlAction::Status
                | ControlAction::Help
                | ControlAction::AgentId
                | ControlAction::WhoAmI
                | ControlAction::AutonomyStatus => {
                    let messaging = messaging.clone();
                    let deps = deps.clone();
                    let target = message.clone();
                    let args = parsed.args.clone();
                    tokio::spawn(async move {
                        let reply = plane.execute(action, &args).await;
                        send_ephemeral(&messaging, &deps, &target, reply).await;
                    });
                }
            }
            Dispatch::Handled
        }
        CommandHandler::Agent(_) => {
            if scope.turn_active {
                match parsed.def.busy {
                    BusyPolicy::Reject => {
                        reply_ephemeral(
                            messaging,
                            deps,
                            message,
                            access::busy_reject_text(parsed.def),
                        );
                        return Dispatch::Handled;
                    }
                    BusyPolicy::Queue => {
                        reply_ephemeral(
                            messaging,
                            deps,
                            message,
                            access::busy_queued_text(parsed.def),
                        );
                    }
                }
            }
            message.content = MessageContent::Command {
                name: parsed.def.name.to_string(),
                args: parsed.args,
            };
            Dispatch::ForwardCommand
        }
    }
}

fn reply_ephemeral(
    messaging: &Arc<MessagingManager>,
    deps: &crate::AgentDeps,
    target: &InboundMessage,
    text: String,
) {
    let messaging = messaging.clone();
    let deps = deps.clone();
    let target = target.clone();
    tokio::spawn(async move {
        send_ephemeral(&messaging, &deps, &target, text).await;
    });
}

async fn send_ephemeral(
    messaging: &Arc<MessagingManager>,
    deps: &crate::AgentDeps,
    target: &InboundMessage,
    text: String,
) {
    // Portal delivery rides the SSE event bus, not a messaging adapter —
    // the portal adapter's respond() is deliberately a no-op.
    if target.adapter_key() == "portal" {
        if let Some(api_state) = deps.api_state.as_deref() {
            api_state
                .event_tx
                .send(crate::api::ApiEvent::OutboundMessage {
                    agent_id: deps.agent_id.to_string(),
                    channel_id: target.conversation_id.clone(),
                    text,
                })
                .ok();
        }
        return;
    }

    let response = OutboundResponse::Ephemeral {
        text,
        user_id: target.sender_id.clone(),
    };
    if let Err(error) = messaging.respond(target, response).await {
        tracing::warn!(
            %error,
            conversation_id = %target.conversation_id,
            "failed to deliver command reply"
        );
    }
}
