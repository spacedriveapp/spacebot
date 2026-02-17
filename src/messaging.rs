//! Messaging adapters (Discord, Matrix, Slack, Telegram, Webhook).

pub mod traits;
pub mod manager;
pub mod discord;
pub mod matrix;
pub mod slack;
pub mod telegram;
pub mod webhook;

pub use traits::Messaging;
pub use manager::MessagingManager;
