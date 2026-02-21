//! Encrypted credentials storage (AES-256-GCM, redb).

/// Secrets store.
pub struct SecretsStore;

impl SecretsStore {
    /// Create a new secrets store.
    pub fn new() -> Self {
        Self
    }
}

impl Default for SecretsStore {
    fn default() -> Self {
        Self::new()
    }
}
