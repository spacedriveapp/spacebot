//! Messaging adapters (Discord, Slack, Telegram, Webhook).

pub mod discord;
pub mod manager;
pub mod slack;
pub mod telegram;
pub mod traits;
pub mod webhook;

pub use manager::MessagingManager;
pub use traits::Messaging;
