//! Conversation history and context management.

pub mod channel_settings;
pub mod channels;
pub mod context;
pub mod history;
pub mod participants;
pub mod portal;
pub mod settings;
pub mod worker_transcript;

pub use channel_settings::ChannelSettingsStore;
pub use channels::ChannelStore;
pub use history::{
    ConversationLogger, ProcessRunLogger, TimelineItem, WorkerDetailRow, WorkerRunRow,
};
pub use participants::{
    ActiveParticipant, participant_display_name, participant_memory_key, renderable_participants,
    track_active_participant,
};
pub use portal::{PortalConversation, PortalConversationStore, PortalConversationSummary};
pub use settings::{
    ConversationDefaultsResponse, ConversationSettings, DelegationMode, MemoryMode, ModelOption,
    ResolvedConversationSettings, ResponseMode, WorkerContextMode, WorkerHistoryMode,
    WorkerMemoryMode,
};
pub use worker_transcript::{ActionContent, TranscriptStep};
