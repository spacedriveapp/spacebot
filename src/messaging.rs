//! Messaging adapters (Discord, Slack, Telegram, Twitch, Signal, Email, Webhook, Portal, Mattermost, Teams).

pub mod discord;
pub mod email;
pub mod manager;
pub mod mattermost;
pub mod portal;
pub mod signal;
pub mod slack;
pub mod target;
pub mod teams;
pub mod telegram;
pub mod traits;
pub mod twitch;
pub mod webhook;

pub use manager::MessagingManager;
pub use traits::Messaging;
pub use traits::apply_runtime_adapter_to_conversation_id;
