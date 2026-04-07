//! LLM provider management and routing.

pub mod anthropic;
pub mod manager;
pub mod model;
pub mod pricing;
pub mod providers;
pub mod routing;
pub mod transcription;

pub use manager::LlmManager;
pub use model::SpacebotModel;
pub use routing::RoutingConfig;
// Re-export types from transcription module
pub use transcription::{TranscriptionRequest, TranscriptionResponse, transcribe_audio};
