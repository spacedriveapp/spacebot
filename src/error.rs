//! Top-level error types for Spacebot.

use std::sync::Arc;

/// Crate-wide result type alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error enum wrapping domain-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(Box<ConfigError>),

    #[error(transparent)]
    Db(Box<DbError>),

    #[error(transparent)]
    Llm(Box<LlmError>),

    #[error(transparent)]
    Memory(Box<MemoryError>),

    #[error(transparent)]
    Agent(Box<AgentError>),

    #[error(transparent)]
    Secrets(Box<SecretsError>),

    #[error(transparent)]
    Settings(Box<SettingsError>),

    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<ConfigError> for Error {
    fn from(e: ConfigError) -> Self {
        Error::Config(Box::new(e))
    }
}
impl From<DbError> for Error {
    fn from(e: DbError) -> Self {
        Error::Db(Box::new(e))
    }
}
impl From<LlmError> for Error {
    fn from(e: LlmError) -> Self {
        Error::Llm(Box::new(e))
    }
}
impl From<MemoryError> for Error {
    fn from(e: MemoryError) -> Self {
        Error::Memory(Box::new(e))
    }
}
impl From<AgentError> for Error {
    fn from(e: AgentError) -> Self {
        Error::Agent(Box::new(e))
    }
}
impl From<SecretsError> for Error {
    fn from(e: SecretsError) -> Self {
        Error::Secrets(Box::new(e))
    }
}
impl From<SettingsError> for Error {
    fn from(e: SettingsError) -> Self {
        Error::Settings(Box::new(e))
    }
}

/// Configuration loading errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to load config from {path}: {source}")]
    Load {
        path: String,
        source: Arc<std::io::Error>,
    },

    #[error("invalid configuration: {0}")]
    Invalid(String),

    #[error("missing required config key: {0}")]
    MissingKey(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Database connection and operation errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("failed to connect to SQLite: {0}")]
    SqliteConnect(#[from] sqlx::Error),

    #[error("failed to connect to LanceDB: {0}")]
    LanceConnect(String),

    #[error("LanceDB operation failed: {0}")]
    LanceDb(String),

    #[error("failed to connect to redb: {0}")]
    RedbConnect(#[from] redb::Error),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("query failed: {0}")]
    Query(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// LLM provider and model errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    #[error("unknown model: {0}")]
    UnknownModel(String),

    #[error("provider request failed: {0}")]
    ProviderRequest(String),

    #[error("missing API key for provider: {0}")]
    MissingProviderKey(String),

    #[error("embedding generation failed: {0}")]
    EmbeddingFailed(String),

    #[error("completion failed: {0}")]
    CompletionFailed(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Memory storage and retrieval errors.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory not found: {id}")]
    NotFound { id: String },

    #[error("failed to save memory: {0}")]
    SaveFailed(String),

    #[error("failed to search memories: {0}")]
    SearchFailed(String),

    #[error("failed to generate embedding: {0}")]
    EmbeddingFailed(String),

    #[error("graph operation failed: {0}")]
    GraphOperationFailed(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Agent (channel, branch, worker) errors.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("channel {id} not found")]
    ChannelNotFound { id: String },

    #[error("worker {id} not found")]
    WorkerNotFound { id: String },

    #[error("branch {id} not found")]
    BranchNotFound { id: String },

    #[error("max concurrent branches ({max}) reached for channel {channel_id}")]
    BranchLimitReached { channel_id: String, max: usize },

    #[error("max concurrent workers ({max}) reached for channel {channel_id}")]
    WorkerLimitReached { channel_id: String, max: usize },

    #[error("worker state transition failed: {0}")]
    InvalidStateTransition(String),

    #[error("compaction failed: {0}")]
    CompactionFailed(String),

    #[error("process cancelled: {reason}")]
    Cancelled { reason: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Secrets and credential errors.
#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("failed to encrypt secret: {0}")]
    EncryptionFailed(String),

    #[error("failed to decrypt secret: {0}")]
    DecryptionFailed(String),

    #[error("secret not found: {key}")]
    NotFound { key: String },

    #[error("invalid key format")]
    InvalidKey,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Settings storage errors.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("failed to read setting {key}: {details}")]
    ReadFailed { key: String, details: String },

    #[error("failed to write setting {key}: {details}")]
    WriteFailed { key: String, details: String },

    #[error("setting not found: {key}")]
    NotFound { key: String },

    #[error("settings error: {0}")]
    Other(String),
}
