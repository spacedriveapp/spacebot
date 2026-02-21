//! Messaging adapters (Discord, Slack, Telegram, Twitch, Webhook, WebChat).

pub mod discord;
pub mod manager;
pub mod slack;
pub mod target;
pub mod telegram;
pub mod traits;
pub mod twitch;
pub mod webchat;
pub mod webhook;

pub use manager::MessagingManager;
pub use traits::Messaging;
