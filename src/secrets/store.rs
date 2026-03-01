//! Credential storage with two secret categories: system secrets (internal, never
//! exposed to subprocesses) and tool secrets (passed to worker subprocesses as env
//! vars).
//!
//! Supports two modes:
//! - **Unencrypted (default):** secrets stored as plaintext in redb. No master key
//!   needed. All secret store features work (categories, env sanitization, output
//!   scrubbing). Only encryption at rest is missing.
//! - **Encrypted (opt-in):** AES-256-GCM with a master key derived via Argon2id.
//!   The master key lives in the OS credential store (Keychain / kernel keyring),
//!   never on disk.

use crate::error::SecretsError;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use rand::RngCore;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::path::Path;
use std::sync::RwLock;

/// Table for secret values. Stores either plaintext UTF-8 or nonce+ciphertext
/// depending on the store's encryption mode.
const SECRETS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("secrets");

/// Table for secret metadata (category, timestamps). Always stored as plaintext
/// JSON so names and categories are readable even when the store is locked.
const METADATA_TABLE: TableDefinition<&str, &str> = TableDefinition::new("secrets_metadata");

/// Table for store-level configuration (encryption flag, argon2 salt, etc.).
const STORE_CONFIG_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("secrets_config");

/// Key in STORE_CONFIG_TABLE indicating whether encryption is enabled.
const CONFIG_KEY_ENCRYPTED: &str = "encrypted";

/// Key in STORE_CONFIG_TABLE storing the Argon2id salt (16 bytes).
const CONFIG_KEY_SALT: &str = "argon2_salt";

/// Key in STORE_CONFIG_TABLE storing a sentinel value encrypted with the master
/// key. Used to validate the key on unlock without decrypting every secret.
const CONFIG_KEY_SENTINEL: &str = "sentinel";

/// The plaintext sentinel value. Encrypted during `enable_encryption()` and
/// verified during `unlock()`.
const SENTINEL_PLAINTEXT: &[u8] = b"spacebot-secrets-sentinel-v1";

/// Secret value wrapper that redacts in Debug and Display to prevent accidental
/// logging of credential values.
pub struct DecryptedSecret(String);

impl DecryptedSecret {
    /// Access the raw secret value.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Debug for DecryptedSecret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "DecryptedSecret(***)")
    }
}

impl Display for DecryptedSecret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "***")
    }
}

/// Secret category determines subprocess exposure.
///
/// All secrets are readable by Rust code via `SecretsStore::get()` regardless
/// of category. The category answers one question: should this value be injected
/// as an env var into worker subprocesses?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretCategory {
    /// Not exposed to subprocesses. Rust code reads them for internal use
    /// (LLM clients, messaging adapters, webhook integrations).
    System,
    /// Exposed to subprocesses as env vars. CLI tools workers invoke need these
    /// (gh, npm, aws, etc.). Rust code can also read them.
    Tool,
}

impl std::fmt::Display for SecretCategory {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretCategory::System => write!(f, "system"),
            SecretCategory::Tool => write!(f, "tool"),
        }
    }
}

/// Metadata stored alongside each secret (always unencrypted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub category: SecretCategory,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Store state exposed via the status API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreState {
    /// Secrets stored in plaintext in redb. No master key configured.
    Unencrypted,
    /// Encryption enabled, master key loaded. Secrets are decrypted and operational.
    Unlocked,
    /// Encryption enabled but master key not available. Encrypted secrets are
    /// inaccessible. Happens after Linux reboot (kernel keyring cleared).
    Locked,
}

impl std::fmt::Display for StoreState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreState::Unencrypted => write!(f, "unencrypted"),
            StoreState::Unlocked => write!(f, "unlocked"),
            StoreState::Locked => write!(f, "locked"),
        }
    }
}

/// Status snapshot for the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreStatus {
    pub state: StoreState,
    pub encrypted: bool,
    pub secret_count: usize,
    pub system_count: usize,
    pub tool_count: usize,
    pub platform_managed: bool,
}

/// Credential store with system/tool categorization and optional encryption.
///
/// Created once per agent, shared via `Arc`. Thread-safe — internal `RwLock`
/// protects the cipher state for encryption transitions.
pub struct SecretsStore {
    db: Database,
    /// Current cipher state. `None` when unencrypted or locked.
    /// Protected by RwLock for encrypt/unlock/lock transitions.
    cipher_state: RwLock<Option<CipherState>>,
    /// Whether the redb store has encrypted secrets (persisted flag).
    encrypted: RwLock<bool>,
}

/// Derived cipher key + salt for the encrypted mode.
struct CipherState {
    cipher: Aes256Gcm,
    #[allow(dead_code)]
    salt: [u8; 16],
}

impl Debug for SecretsStore {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsStore")
            .field("state", &self.state())
            .finish()
    }
}

impl SecretsStore {
    /// Open or create a secrets store at the given path.
    ///
    /// The store starts in unencrypted mode. If the redb file contains the
    /// `encrypted` flag, the store starts in locked state until `unlock()` is
    /// called with the master key.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SecretsError> {
        let db = Database::create(path.as_ref()).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open secrets database: {error}"))
        })?;

        // Ensure all tables exist.
        let write_transaction = db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to initialize secrets tables: {error}"
            ))
        })?;
        {
            let _secrets = write_transaction
                .open_table(SECRETS_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
                })?;
            let _metadata = write_transaction
                .open_table(METADATA_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
                })?;
            let _config = write_transaction
                .open_table(STORE_CONFIG_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!(
                        "failed to open store config table: {error}"
                    ))
                })?;
        }
        write_transaction.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to commit table initialization: {error}"
            ))
        })?;

        // Check if encryption was previously enabled.
        let encrypted = {
            let read_txn = db.begin_read().map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
            })?;
            let table = read_txn.open_table(STORE_CONFIG_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to open store config table: {error}"
                ))
            })?;
            match table.get(CONFIG_KEY_ENCRYPTED) {
                Ok(Some(value)) => value.value() == [1],
                _ => false,
            }
        };

        Ok(Self {
            db,
            cipher_state: RwLock::new(None),
            encrypted: RwLock::new(encrypted),
        })
    }

    /// Current store state.
    pub fn state(&self) -> StoreState {
        let encrypted = *self.encrypted.read().expect("encrypted lock poisoned");
        if !encrypted {
            return StoreState::Unencrypted;
        }
        if self
            .cipher_state
            .read()
            .expect("cipher lock poisoned")
            .is_some()
        {
            StoreState::Unlocked
        } else {
            StoreState::Locked
        }
    }

    /// Whether the store has encryption enabled.
    pub fn is_encrypted(&self) -> bool {
        *self.encrypted.read().expect("encrypted lock poisoned")
    }

    /// Full status snapshot for the API.
    pub fn status(&self, platform_managed: bool) -> Result<StoreStatus, SecretsError> {
        let all_metadata = self.list_metadata()?;
        let system_count = all_metadata
            .values()
            .filter(|m| m.category == SecretCategory::System)
            .count();
        let tool_count = all_metadata
            .values()
            .filter(|m| m.category == SecretCategory::Tool)
            .count();

        Ok(StoreStatus {
            state: self.state(),
            encrypted: self.is_encrypted(),
            secret_count: all_metadata.len(),
            system_count,
            tool_count,
            platform_managed,
        })
    }

    /// Store a secret with the given category. Creates or updates.
    pub fn set(
        &self,
        name: &str,
        value: &str,
        category: SecretCategory,
    ) -> Result<(), SecretsError> {
        let state = self.state();
        if state == StoreState::Locked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "secret store is locked — unlock with master key first"
            )));
        }

        let stored_value = self.encode_value(value)?;
        let now = chrono::Utc::now();

        // Check if updating an existing secret (preserve created_at).
        let existing_meta = self.get_metadata(name).ok();
        let metadata = SecretMetadata {
            category,
            created_at: existing_meta.as_ref().map(|m| m.created_at).unwrap_or(now),
            updated_at: now,
        };
        let metadata_json = serde_json::to_string(&metadata).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to serialize metadata: {error}"))
        })?;

        let write_txn = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;
        {
            let mut secrets = write_txn.open_table(SECRETS_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;
            secrets
                .insert(name, stored_value.as_slice())
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!(
                        "failed to insert secret '{name}': {error}"
                    ))
                })?;

            let mut meta = write_txn.open_table(METADATA_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
            })?;
            meta.insert(name, metadata_json.as_str()).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to insert metadata for '{name}': {error}"
                ))
            })?;
        }
        write_txn.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to commit secret '{name}': {error}"))
        })?;

        Ok(())
    }

    /// Retrieve a decrypted secret value.
    pub fn get(&self, name: &str) -> Result<DecryptedSecret, SecretsError> {
        let state = self.state();
        if state == StoreState::Locked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "secret store is locked — unlock with master key first"
            )));
        }

        let read_txn = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(SECRETS_TABLE).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
        })?;

        let value = table
            .get(name)
            .map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to read key '{name}': {error}"))
            })?
            .ok_or_else(|| SecretsError::NotFound {
                key: name.to_string(),
            })?;

        let raw = value.value();
        self.decode_value(raw)
    }

    /// Delete a secret.
    pub fn delete(&self, name: &str) -> Result<(), SecretsError> {
        let write_txn = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;
        {
            let mut secrets = write_txn.open_table(SECRETS_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;
            secrets.remove(name).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to remove key '{name}': {error}"))
            })?;

            let mut meta = write_txn.open_table(METADATA_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
            })?;
            meta.remove(name).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to remove metadata for '{name}': {error}"
                ))
            })?;
        }
        write_txn.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to commit delete for '{name}': {error}"
            ))
        })?;

        Ok(())
    }

    /// List all secret names.
    pub fn list(&self) -> Result<Vec<String>, SecretsError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(METADATA_TABLE).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
        })?;

        let mut names = Vec::new();
        let iter = table.iter().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to iterate metadata table: {error}"))
        })?;
        for entry in iter {
            let (key, _) = entry.map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to read metadata entry: {error}"))
            })?;
            names.push(key.value().to_string());
        }

        Ok(names)
    }

    /// Get metadata for a specific secret.
    pub fn get_metadata(&self, name: &str) -> Result<SecretMetadata, SecretsError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(METADATA_TABLE).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
        })?;

        let value = table
            .get(name)
            .map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to read metadata for '{name}': {error}"
                ))
            })?
            .ok_or_else(|| SecretsError::NotFound {
                key: name.to_string(),
            })?;

        serde_json::from_str(value.value()).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to parse metadata for '{name}': {error}"
            ))
        })
    }

    /// List all secrets with their metadata. Works in all states (names and
    /// categories are stored as unencrypted metadata).
    pub fn list_metadata(&self) -> Result<HashMap<String, SecretMetadata>, SecretsError> {
        let read_txn = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_txn.open_table(METADATA_TABLE).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open metadata table: {error}"))
        })?;

        let mut result = HashMap::new();
        let iter = table.iter().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to iterate metadata table: {error}"))
        })?;
        for entry in iter {
            let (key, value) = entry.map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to read metadata entry: {error}"))
            })?;
            match serde_json::from_str::<SecretMetadata>(value.value()) {
                Ok(meta) => {
                    result.insert(key.value().to_string(), meta);
                }
                Err(error) => {
                    tracing::warn!(
                        key = key.value(),
                        %error,
                        "skipping secret with corrupted metadata"
                    );
                }
            }
        }

        Ok(result)
    }

    /// Get all tool secrets as name→value pairs for `Sandbox::wrap()` injection.
    ///
    /// Returns an empty map when locked (tool secrets unavailable).
    pub fn tool_env_vars(&self) -> HashMap<String, String> {
        if self.state() == StoreState::Locked {
            return HashMap::new();
        }

        let metadata = match self.list_metadata() {
            Ok(m) => m,
            Err(error) => {
                tracing::warn!(%error, "failed to list secret metadata for tool env vars");
                return HashMap::new();
            }
        };

        let mut result = HashMap::new();
        for (name, meta) in &metadata {
            if meta.category == SecretCategory::Tool
                && let Ok(secret) = self.get(name)
            {
                result.insert(name.clone(), secret.expose().to_string());
            }
        }

        result
    }

    /// Get names of all tool secrets for injection into worker system prompts.
    ///
    /// Returns names even when locked (metadata is always accessible).
    pub fn tool_secret_names(&self) -> Vec<String> {
        match self.list_metadata() {
            Ok(metadata) => metadata
                .into_iter()
                .filter(|(_, meta)| meta.category == SecretCategory::Tool)
                .map(|(name, _)| name)
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "failed to list secret metadata for tool secret names");
                Vec::new()
            }
        }
    }

    /// Get all tool secret name→value pairs for the output scrubber.
    ///
    /// Returns pairs suitable for `StreamScrubber::new()`.
    pub fn tool_secret_pairs(&self) -> Vec<(String, String)> {
        self.tool_env_vars().into_iter().collect()
    }

    /// Enable encryption. Generates a random master key, encrypts all existing
    /// secrets in place, stores the key derivation salt and sentinel in redb.
    ///
    /// Returns the raw master key bytes for the caller to store in the OS
    /// credential store and display to the user.
    pub fn enable_encryption(&self) -> Result<Vec<u8>, SecretsError> {
        if self.is_encrypted() {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "encryption is already enabled"
            )));
        }

        // Generate master key and salt.
        let mut master_key = vec![0u8; 32];
        rand::rng().fill_bytes(&mut master_key);
        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);

        // Derive cipher key.
        let cipher = derive_cipher(&master_key, &salt)?;

        // Encrypt sentinel value.
        let encrypted_sentinel = encrypt_bytes(&cipher, SENTINEL_PLAINTEXT)?;

        // Re-encrypt all existing secrets.
        let names = self.list()?;
        let mut plain_values: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
        {
            let read_txn = self.db.begin_read().map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
            })?;
            let table = read_txn.open_table(SECRETS_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;
            for name in &names {
                if let Some(val) = table.get(name.as_str()).map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to read secret '{name}': {error}"))
                })? {
                    // Current values are plaintext UTF-8.
                    plain_values.push((name.clone(), val.value().to_vec()));
                }
            }
        }

        // Write everything in one transaction.
        let write_txn = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;
        {
            let mut secrets = write_txn.open_table(SECRETS_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;
            // Re-encrypt each secret value.
            for (name, plaintext_bytes) in &plain_values {
                let encrypted = encrypt_bytes(&cipher, plaintext_bytes)?;
                secrets
                    .insert(name.as_str(), encrypted.as_slice())
                    .map_err(|error| {
                        SecretsError::Other(anyhow::anyhow!(
                            "failed to write encrypted secret '{name}': {error}"
                        ))
                    })?;
            }

            let mut config = write_txn.open_table(STORE_CONFIG_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to open store config table: {error}"
                ))
            })?;
            config
                .insert(CONFIG_KEY_ENCRYPTED, &[1u8][..])
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to write encryption flag: {error}"))
                })?;
            config.insert(CONFIG_KEY_SALT, &salt[..]).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to write salt: {error}"))
            })?;
            config
                .insert(CONFIG_KEY_SENTINEL, encrypted_sentinel.as_slice())
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to write sentinel: {error}"))
                })?;
        }
        write_txn.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to commit encryption enablement: {error}"
            ))
        })?;

        // Update in-memory state.
        *self.encrypted.write().expect("encrypted lock poisoned") = true;
        *self.cipher_state.write().expect("cipher lock poisoned") =
            Some(CipherState { cipher, salt });

        Ok(master_key)
    }

    /// Unlock the store with the given master key. Validates against the stored
    /// sentinel before accepting.
    pub fn unlock(&self, master_key: &[u8]) -> Result<(), SecretsError> {
        if !self.is_encrypted() {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "store is not encrypted — nothing to unlock"
            )));
        }
        if self.state() == StoreState::Unlocked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "store is already unlocked"
            )));
        }

        // Read salt and sentinel from redb.
        let (salt, encrypted_sentinel) = {
            let read_txn = self.db.begin_read().map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
            })?;
            let table = read_txn.open_table(STORE_CONFIG_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to open store config table: {error}"
                ))
            })?;

            let salt_val = table
                .get(CONFIG_KEY_SALT)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to read salt: {error}"))
                })?
                .ok_or_else(|| {
                    SecretsError::Other(anyhow::anyhow!("encrypted store missing salt"))
                })?;
            let mut salt = [0u8; 16];
            let raw = salt_val.value();
            if raw.len() != 16 {
                return Err(SecretsError::Other(anyhow::anyhow!(
                    "invalid salt length: {}",
                    raw.len()
                )));
            }
            salt.copy_from_slice(raw);

            let sentinel_val = table
                .get(CONFIG_KEY_SENTINEL)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to read sentinel: {error}"))
                })?
                .ok_or_else(|| {
                    SecretsError::Other(anyhow::anyhow!("encrypted store missing sentinel"))
                })?;

            (salt, sentinel_val.value().to_vec())
        };

        // Derive cipher and verify sentinel.
        let cipher = derive_cipher(master_key, &salt)?;
        let decrypted_sentinel =
            decrypt_bytes(&cipher, &encrypted_sentinel).map_err(|_| SecretsError::InvalidKey)?;

        if decrypted_sentinel != SENTINEL_PLAINTEXT {
            return Err(SecretsError::InvalidKey);
        }

        // Accept the key.
        *self.cipher_state.write().expect("cipher lock poisoned") =
            Some(CipherState { cipher, salt });

        Ok(())
    }

    /// Lock the store. Clears the in-memory cipher key. Encrypted secrets become
    /// inaccessible until `unlock()` is called again.
    pub fn lock(&self) -> Result<(), SecretsError> {
        if !self.is_encrypted() {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "store is not encrypted — nothing to lock"
            )));
        }
        *self.cipher_state.write().expect("cipher lock poisoned") = None;
        Ok(())
    }

    /// Rotate the master key. Generates a new key, re-encrypts all secrets and
    /// the sentinel, updates the salt. Returns the new master key bytes.
    pub fn rotate_key(&self) -> Result<Vec<u8>, SecretsError> {
        if self.state() != StoreState::Unlocked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "store must be unlocked to rotate key"
            )));
        }

        // Generate new key and salt.
        let mut new_master_key = vec![0u8; 32];
        rand::rng().fill_bytes(&mut new_master_key);
        let mut new_salt = [0u8; 16];
        rand::rng().fill_bytes(&mut new_salt);

        let new_cipher = derive_cipher(&new_master_key, &new_salt)?;

        // Decrypt all secrets with old cipher, re-encrypt with new.
        let names = self.list()?;
        let mut re_encrypted: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
        for name in &names {
            let decrypted = self.get(name)?;
            let encrypted = encrypt_bytes(&new_cipher, decrypted.expose().as_bytes())?;
            re_encrypted.push((name.clone(), encrypted));
        }

        // Re-encrypt sentinel.
        let new_encrypted_sentinel = encrypt_bytes(&new_cipher, SENTINEL_PLAINTEXT)?;

        // Write everything atomically.
        let write_txn = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;
        {
            let mut secrets = write_txn.open_table(SECRETS_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;
            for (name, encrypted) in &re_encrypted {
                secrets
                    .insert(name.as_str(), encrypted.as_slice())
                    .map_err(|error| {
                        SecretsError::Other(anyhow::anyhow!(
                            "failed to write re-encrypted secret '{name}': {error}"
                        ))
                    })?;
            }

            let mut config = write_txn.open_table(STORE_CONFIG_TABLE).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!(
                    "failed to open store config table: {error}"
                ))
            })?;
            config
                .insert(CONFIG_KEY_SALT, &new_salt[..])
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to write new salt: {error}"))
                })?;
            config
                .insert(CONFIG_KEY_SENTINEL, new_encrypted_sentinel.as_slice())
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to write new sentinel: {error}"))
                })?;
        }
        write_txn.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to commit key rotation: {error}"))
        })?;

        // Update in-memory cipher.
        *self.cipher_state.write().expect("cipher lock poisoned") = Some(CipherState {
            cipher: new_cipher,
            salt: new_salt,
        });

        Ok(new_master_key)
    }

    /// Check if a secret exists.
    pub fn exists(&self, name: &str) -> bool {
        self.get_metadata(name).is_ok()
    }

    /// Export all secrets as a portable backup.
    ///
    /// Returns a JSON blob with all secret names, values, categories, and
    /// metadata. If encryption is enabled and the store is unlocked, the
    /// export contains decrypted values (the caller can re-encrypt the file
    /// at rest). If the store is locked, returns an error.
    pub fn export_all(&self) -> Result<ExportData, SecretsError> {
        if self.state() == StoreState::Locked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "secret store is locked — unlock before exporting"
            )));
        }

        let metadata = self.list_metadata()?;
        let mut entries = Vec::with_capacity(metadata.len());

        for (name, meta) in &metadata {
            let value = self.get(name)?;
            entries.push(ExportEntry {
                name: name.clone(),
                value: value.expose().to_string(),
                category: meta.category,
                created_at: meta.created_at,
                updated_at: meta.updated_at,
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(ExportData {
            version: 1,
            encrypted: self.is_encrypted(),
            entries,
        })
    }

    /// Import secrets from an export. Optionally overwrites existing secrets
    /// with the same name.
    ///
    /// Returns the count of imported and skipped secrets.
    pub fn import_all(
        &self,
        data: &ExportData,
        overwrite: bool,
    ) -> Result<ImportResult, SecretsError> {
        if self.state() == StoreState::Locked {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "secret store is locked — unlock before importing"
            )));
        }

        let mut imported = 0usize;
        let mut skipped = Vec::new();

        for entry in &data.entries {
            if self.exists(&entry.name) && !overwrite {
                skipped.push(entry.name.clone());
                continue;
            }

            self.set(&entry.name, &entry.value, entry.category)?;
            imported += 1;
        }

        Ok(ImportResult { imported, skipped })
    }

    /// Encode a plaintext value for storage. In encrypted mode, encrypts with
    /// the current cipher. In unencrypted mode, stores as raw UTF-8 bytes.
    fn encode_value(&self, plaintext: &str) -> Result<Vec<u8>, SecretsError> {
        let guard = self.cipher_state.read().expect("cipher lock poisoned");
        match guard.as_ref() {
            Some(state) => encrypt_bytes(&state.cipher, plaintext.as_bytes()),
            None => {
                // Unencrypted mode — store as raw UTF-8.
                Ok(plaintext.as_bytes().to_vec())
            }
        }
    }

    /// Decode a stored value. In encrypted mode, decrypts. In unencrypted mode,
    /// interprets as raw UTF-8.
    fn decode_value(&self, stored: &[u8]) -> Result<DecryptedSecret, SecretsError> {
        let guard = self.cipher_state.read().expect("cipher lock poisoned");
        match guard.as_ref() {
            Some(state) => {
                let plaintext = decrypt_bytes(&state.cipher, stored)?;
                let text = String::from_utf8(plaintext)
                    .map_err(|error| SecretsError::DecryptionFailed(error.to_string()))?;
                Ok(DecryptedSecret(text))
            }
            None => {
                // Unencrypted mode — raw UTF-8.
                let text = String::from_utf8(stored.to_vec())
                    .map_err(|error| SecretsError::DecryptionFailed(error.to_string()))?;
                Ok(DecryptedSecret(text))
            }
        }
    }
}

/// Portable backup format for all secrets in a store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportData {
    /// Format version (currently 1).
    pub version: u32,
    /// Whether the source store had encryption enabled (informational only —
    /// values in this struct are always plaintext).
    pub encrypted: bool,
    /// All secrets with their metadata.
    pub entries: Vec<ExportEntry>,
}

/// A single secret in the export format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEntry {
    pub name: String,
    pub value: String,
    pub category: SecretCategory,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Result of an import operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    /// Number of secrets successfully imported.
    pub imported: usize,
    /// Names of secrets that were skipped because they already existed (and
    /// overwrite was not requested).
    pub skipped: Vec<String>,
}

/// Derive an AES-256-GCM cipher key from a master key using Argon2id.
///
/// Argon2id is used instead of SHA-256 because self-hosted users may use a
/// passphrase as the master key. SHA-256 of a passphrase is trivially brutable;
/// Argon2id is memory-hard and resistant to GPU/ASIC attacks. The cost is a
/// one-time ~100ms at startup.
fn derive_cipher(master_key: &[u8], salt: &[u8; 16]) -> Result<Aes256Gcm, SecretsError> {
    if master_key.is_empty() {
        return Err(SecretsError::InvalidKey);
    }

    let params = argon2::Params::new(
        64 * 1024, // 64 MiB memory
        3,         // 3 iterations
        1,         // 1 degree of parallelism
        Some(32),  // 32 bytes output
    )
    .map_err(|error| SecretsError::Other(anyhow::anyhow!("argon2 params error: {error}")))?;

    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut derived_key = [0u8; 32];
    argon2
        .hash_password_into(master_key, salt, &mut derived_key)
        .map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("argon2 key derivation failed: {error}"))
        })?;

    Aes256Gcm::new_from_slice(&derived_key).map_err(|_| SecretsError::InvalidKey)
}

/// Encrypt bytes with AES-256-GCM. Returns nonce (12 bytes) + ciphertext.
fn encrypt_bytes(cipher: &Aes256Gcm, plaintext: &[u8]) -> Result<Vec<u8>, SecretsError> {
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|error| SecretsError::EncryptionFailed(error.to_string()))?;

    let mut stored = Vec::with_capacity(12 + ciphertext.len());
    stored.extend_from_slice(&nonce_bytes);
    stored.extend_from_slice(&ciphertext);
    Ok(stored)
}

/// Decrypt nonce+ciphertext bytes with AES-256-GCM.
fn decrypt_bytes(cipher: &Aes256Gcm, stored: &[u8]) -> Result<Vec<u8>, SecretsError> {
    if stored.len() < 12 {
        return Err(SecretsError::DecryptionFailed(
            "stored value too short for nonce".to_string(),
        ));
    }
    let nonce = Nonce::from_slice(&stored[..12]);
    let ciphertext = &stored[12..];
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|error| SecretsError::DecryptionFailed(error.to_string()))
}

/// Naming pattern for adapter secret fields that support named instances.
///
/// For a Discord adapter named `"alerts"` with pattern
/// `{ platform_prefix: "DISCORD", field_suffix: "BOT_TOKEN" }`, the derived
/// secret name is `"DISCORD_ALERTS_BOT_TOKEN"`.
#[derive(Debug, Clone, Copy)]
pub struct InstancePattern {
    /// Platform prefix (e.g. `"DISCORD"`, `"SLACK"`).
    pub platform_prefix: &'static str,
    /// Field suffix (e.g. `"BOT_TOKEN"`, `"APP_TOKEN"`).
    pub field_suffix: &'static str,
}

/// A credential field declared by a config section.
///
/// Used by both messaging adapters and other config sections (LLM providers,
/// search integrations) to declare which fields contain secrets. The secret
/// store uses these declarations for auto-categorization and migration.
///
/// For fields with an [`InstancePattern`], named adapter instances derive
/// secret names by inserting the uppercased instance name:
///
/// ```text
/// default:   DISCORD_BOT_TOKEN
/// instance:  DISCORD_ALERTS_BOT_TOKEN
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SecretField {
    /// TOML key within the config section (e.g. `"token"`, `"anthropic_key"`).
    pub toml_key: &'static str,
    /// Canonical secret name (e.g. `"DISCORD_BOT_TOKEN"`, `"ANTHROPIC_API_KEY"`).
    pub secret_name: &'static str,
    /// Instance naming pattern. Only set for messaging adapters that support
    /// `[[messaging.*.instances]]`. `None` for LLM keys, search keys, etc.
    pub instance_pattern: Option<InstancePattern>,
}

impl SecretField {
    /// Secret name for a named adapter instance.
    ///
    /// Given instance name `"alerts"` and pattern `DISCORD / BOT_TOKEN`,
    /// produces `"DISCORD_ALERTS_BOT_TOKEN"`.
    ///
    /// Returns `None` if this field has no instance pattern (e.g. LLM keys).
    pub fn instance_name(&self, instance: &str) -> Option<String> {
        let pattern = self.instance_pattern.as_ref()?;
        Some(format!(
            "{}_{}_{}",
            pattern.platform_prefix,
            instance.to_uppercase(),
            pattern.field_suffix
        ))
    }
}

/// Trait for config sections to declare their credential fields.
///
/// Implemented on messaging adapter configs (e.g. `DiscordConfig`), LLM config,
/// and any other config section that holds secrets. Used by the secret store for
/// auto-categorization (any matching name → System) and by the migration endpoint
/// to scan config.toml for plaintext credentials.
pub trait SystemSecrets {
    /// Section identifier used for TOML path construction.
    ///
    /// For messaging adapters: `"discord"`, `"slack"`, etc. (paths are
    /// `messaging.{section}.{toml_key}`).
    /// For other sections: `"llm"`, `"defaults"`, etc. (paths are
    /// `{section}.{toml_key}`).
    fn section() -> &'static str;

    /// Credential fields this config section uses.
    fn secret_fields() -> &'static [SecretField];

    /// Whether this section is a messaging adapter (has `instances` array).
    /// Default: `false`.
    fn is_messaging_adapter() -> bool {
        false
    }
}

/// Collect all system secret fields from every config section that implements
/// [`SystemSecrets`]. Returns the combined list used for auto-categorization
/// and migration.
///
/// This is the single source of truth — adding a new secret-bearing config
/// section only requires implementing [`SystemSecrets`] and listing the type
/// here.
pub fn system_secret_registry() -> Vec<&'static SecretField> {
    use crate::config::{
        DefaultsConfig, DiscordConfig, EmailConfig, LlmConfig, SlackConfig, TelegramConfig,
        TwitchConfig,
    };

    let mut fields = Vec::new();
    // LLM provider keys.
    fields.extend(LlmConfig::secret_fields());
    // Search / internal tool keys.
    fields.extend(DefaultsConfig::secret_fields());
    // Messaging adapters.
    fields.extend(DiscordConfig::secret_fields());
    fields.extend(SlackConfig::secret_fields());
    fields.extend(TelegramConfig::secret_fields());
    fields.extend(TwitchConfig::secret_fields());
    fields.extend(EmailConfig::secret_fields());
    fields
}

/// Auto-detect the category for a secret based on its name.
///
/// Secrets whose names match a known internal credential (LLM provider key,
/// messaging adapter token, etc.) are categorized as `System` — never exposed
/// to worker subprocesses. This includes:
///
/// - Exact matches against any [`SystemSecrets`] field's `secret_name`
/// - Named instance patterns (e.g. `DISCORD_ALERTS_BOT_TOKEN`) — any name that
///   matches `{PREFIX}_{ANYTHING}_{SUFFIX}` for a known adapter field with an
///   [`InstancePattern`]
///
/// Everything else defaults to `Tool` — exposed to workers as environment
/// variables. This is the safe default because user-added secrets are almost
/// always credentials for CLI tools that workers invoke.
pub fn auto_categorize(name: &str) -> SecretCategory {
    let upper = name.to_uppercase();

    for field in system_secret_registry() {
        // Exact match on the canonical secret name.
        if upper == field.secret_name {
            return SecretCategory::System;
        }

        // Named instance pattern: {PREFIX}_{ANYTHING}_{SUFFIX}
        // e.g. DISCORD_ALERTS_BOT_TOKEN matches DISCORD + BOT_TOKEN
        if let Some(pattern) = &field.instance_pattern {
            let prefix = format!("{}_", pattern.platform_prefix);
            let suffix = format!("_{}", pattern.field_suffix);
            if upper.starts_with(&prefix)
                && upper.ends_with(&suffix)
                && upper.len() > prefix.len() + suffix.len()
            {
                return SecretCategory::System;
            }
        }
    }

    // Unknown secrets default to tool — they're most likely credentials for
    // CLI tools that workers need (gh, npm, aws, docker, cargo, etc.).
    SecretCategory::Tool
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_store() -> (SecretsStore, NamedTempFile) {
        let file = NamedTempFile::new().expect("create temp file");
        let store = SecretsStore::new(file.path()).expect("create store");
        (store, file)
    }

    #[test]
    fn unencrypted_set_get_delete() {
        let (store, _file) = temp_store();
        assert_eq!(store.state(), StoreState::Unencrypted);

        store
            .set("MY_KEY", "my_value", SecretCategory::Tool)
            .expect("set");
        let secret = store.get("MY_KEY").expect("get");
        assert_eq!(secret.expose(), "my_value");

        let meta = store.get_metadata("MY_KEY").expect("metadata");
        assert_eq!(meta.category, SecretCategory::Tool);

        store.delete("MY_KEY").expect("delete");
        assert!(store.get("MY_KEY").is_err());
    }

    #[test]
    fn tool_env_vars_returns_only_tool_secrets() {
        let (store, _file) = temp_store();
        store
            .set("ANTHROPIC_API_KEY", "sk-ant-xxx", SecretCategory::System)
            .expect("set system");
        store
            .set("GH_TOKEN", "ghp_abc123", SecretCategory::Tool)
            .expect("set tool");

        let env_vars = store.tool_env_vars();
        assert_eq!(env_vars.len(), 1);
        assert_eq!(env_vars.get("GH_TOKEN").unwrap(), "ghp_abc123");
        assert!(!env_vars.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn tool_secret_names_returns_only_tool_names() {
        let (store, _file) = temp_store();
        store
            .set("OPENAI_API_KEY", "sk-xxx", SecretCategory::System)
            .expect("set system");
        store
            .set("NPM_TOKEN", "npm_xxx", SecretCategory::Tool)
            .expect("set tool");
        store
            .set("GH_TOKEN", "ghp_xxx", SecretCategory::Tool)
            .expect("set tool");

        let names = store.tool_secret_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"NPM_TOKEN".to_string()));
        assert!(names.contains(&"GH_TOKEN".to_string()));
    }

    #[test]
    fn encryption_lifecycle() {
        let (store, _file) = temp_store();

        // Store a secret in unencrypted mode.
        store
            .set("MY_SECRET", "plaintext_value", SecretCategory::System)
            .expect("set");

        // Enable encryption.
        let master_key = store.enable_encryption().expect("encrypt");
        assert_eq!(store.state(), StoreState::Unlocked);

        // Secret is still readable.
        let secret = store.get("MY_SECRET").expect("get after encrypt");
        assert_eq!(secret.expose(), "plaintext_value");

        // Lock the store.
        store.lock().expect("lock");
        assert_eq!(store.state(), StoreState::Locked);
        assert!(store.get("MY_SECRET").is_err());

        // Unlock with correct key.
        store.unlock(&master_key).expect("unlock");
        assert_eq!(store.state(), StoreState::Unlocked);
        let secret = store.get("MY_SECRET").expect("get after unlock");
        assert_eq!(secret.expose(), "plaintext_value");

        // Wrong key fails.
        store.lock().expect("lock again");
        assert!(store.unlock(b"wrong_key").is_err());
    }

    #[test]
    fn key_rotation() {
        let (store, _file) = temp_store();
        store
            .set("MY_SECRET", "value123", SecretCategory::Tool)
            .expect("set");
        let _old_key = store.enable_encryption().expect("encrypt");

        // Rotate.
        let new_key = store.rotate_key().expect("rotate");

        // Secret still readable with new cipher.
        let secret = store.get("MY_SECRET").expect("get after rotate");
        assert_eq!(secret.expose(), "value123");

        // Lock and unlock with new key.
        store.lock().expect("lock");
        store.unlock(&new_key).expect("unlock with new key");
        let secret = store.get("MY_SECRET").expect("get after re-unlock");
        assert_eq!(secret.expose(), "value123");
    }

    #[test]
    fn auto_categorize_known_patterns() {
        // Tool secrets — anything not recognized defaults to Tool.
        assert_eq!(auto_categorize("GH_TOKEN"), SecretCategory::Tool);
        assert_eq!(auto_categorize("GITHUB_TOKEN"), SecretCategory::Tool);
        assert_eq!(auto_categorize("NPM_TOKEN"), SecretCategory::Tool);
        assert_eq!(auto_categorize("AWS_ACCESS_KEY_ID"), SecretCategory::Tool);
        assert_eq!(auto_categorize("AWS_SESSION_TOKEN"), SecretCategory::Tool);
        assert_eq!(auto_categorize("UNKNOWN_CREDENTIAL"), SecretCategory::Tool);
        assert_eq!(auto_categorize("DOCKER_TOKEN"), SecretCategory::Tool);
        assert_eq!(
            auto_categorize("CARGO_REGISTRY_TOKEN"),
            SecretCategory::Tool
        );

        // Non-adapter system secrets (LLM provider keys):
        assert_eq!(auto_categorize("ANTHROPIC_API_KEY"), SecretCategory::System);
        assert_eq!(auto_categorize("OPENAI_API_KEY"), SecretCategory::System);
        assert_eq!(
            auto_categorize("OPENROUTER_API_KEY"),
            SecretCategory::System
        );
        assert_eq!(auto_categorize("GEMINI_API_KEY"), SecretCategory::System);
        assert_eq!(auto_categorize("DEEPSEEK_API_KEY"), SecretCategory::System);
        assert_eq!(auto_categorize("CEREBRAS_API_KEY"), SecretCategory::System);

        // Messaging adapter tokens (default adapters via AdapterSecrets):
        assert_eq!(auto_categorize("DISCORD_BOT_TOKEN"), SecretCategory::System);
        assert_eq!(auto_categorize("SLACK_BOT_TOKEN"), SecretCategory::System);
        assert_eq!(auto_categorize("SLACK_APP_TOKEN"), SecretCategory::System);
        assert_eq!(
            auto_categorize("TELEGRAM_BOT_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("TWITCH_OAUTH_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("TWITCH_CLIENT_SECRET"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("EMAIL_IMAP_PASSWORD"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("EMAIL_SMTP_USERNAME"),
            SecretCategory::System
        );

        // Named instance patterns — {PREFIX}_{INSTANCE}_{SUFFIX} → System:
        assert_eq!(
            auto_categorize("DISCORD_ALERTS_BOT_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("SLACK_SUPPORT_BOT_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("SLACK_SUPPORT_APP_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("TELEGRAM_NOTIFICATIONS_BOT_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("TWITCH_GAMING_OAUTH_TOKEN"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("TWITCH_GAMING_CLIENT_SECRET"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("EMAIL_SUPPORT_IMAP_PASSWORD"),
            SecretCategory::System
        );
        assert_eq!(
            auto_categorize("EMAIL_BILLING_SMTP_PASSWORD"),
            SecretCategory::System
        );

        // Search integrations:
        assert_eq!(
            auto_categorize("BRAVE_SEARCH_API_KEY"),
            SecretCategory::System
        );

        // Case-insensitive matching:
        assert_eq!(auto_categorize("anthropic_api_key"), SecretCategory::System);
        assert_eq!(auto_categorize("Openai_Api_Key"), SecretCategory::System);
        assert_eq!(
            auto_categorize("discord_alerts_bot_token"),
            SecretCategory::System
        );
    }

    #[test]
    fn decrypted_secret_redacts_display() {
        let secret = DecryptedSecret("super_secret_value".to_string());
        assert_eq!(format!("{secret}"), "***");
        assert_eq!(format!("{secret:?}"), "DecryptedSecret(***)");
        assert_eq!(secret.expose(), "super_secret_value");
    }

    #[test]
    fn list_metadata_works_in_all_states() {
        let (store, _file) = temp_store();
        store
            .set("KEY1", "val1", SecretCategory::System)
            .expect("set");
        store
            .set("KEY2", "val2", SecretCategory::Tool)
            .expect("set");

        // Unencrypted: metadata readable.
        let meta = store.list_metadata().expect("list");
        assert_eq!(meta.len(), 2);

        // Enable encryption.
        let master_key = store.enable_encryption().expect("encrypt");

        // Unlocked: metadata readable.
        let meta = store.list_metadata().expect("list");
        assert_eq!(meta.len(), 2);

        // Locked: metadata still readable.
        store.lock().expect("lock");
        let meta = store.list_metadata().expect("list");
        assert_eq!(meta.len(), 2);

        // But values are not.
        assert!(store.get("KEY1").is_err());

        // Unlock restores access.
        store.unlock(&master_key).expect("unlock");
        assert_eq!(store.get("KEY1").expect("get").expose(), "val1");
    }

    #[test]
    fn export_import_roundtrip() {
        let (store1, _file1) = temp_store();
        store1
            .set("KEY_A", "value_a", SecretCategory::System)
            .expect("set");
        store1
            .set("KEY_B", "value_b", SecretCategory::Tool)
            .expect("set");

        let export = store1.export_all().expect("export");
        assert_eq!(export.entries.len(), 2);
        assert_eq!(export.version, 1);

        // Import into a fresh store.
        let (store2, _file2) = temp_store();
        let result = store2.import_all(&export, false).expect("import");
        assert_eq!(result.imported, 2);
        assert!(result.skipped.is_empty());

        assert_eq!(store2.get("KEY_A").expect("get").expose(), "value_a");
        assert_eq!(store2.get("KEY_B").expect("get").expose(), "value_b");
        assert_eq!(
            store2.get_metadata("KEY_B").expect("meta").category,
            SecretCategory::Tool
        );
    }

    #[test]
    fn import_skips_existing_without_overwrite() {
        let (store, _file) = temp_store();
        store
            .set("EXISTING", "original", SecretCategory::System)
            .expect("set");

        let export = ExportData {
            version: 1,
            encrypted: false,
            entries: vec![
                ExportEntry {
                    name: "EXISTING".to_string(),
                    value: "new_value".to_string(),
                    category: SecretCategory::Tool,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                },
                ExportEntry {
                    name: "FRESH".to_string(),
                    value: "fresh_value".to_string(),
                    category: SecretCategory::Tool,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                },
            ],
        };

        let result = store.import_all(&export, false).expect("import");
        assert_eq!(result.imported, 1);
        assert_eq!(result.skipped, vec!["EXISTING"]);

        // Original value preserved.
        assert_eq!(store.get("EXISTING").expect("get").expose(), "original");
        // New secret imported.
        assert_eq!(store.get("FRESH").expect("get").expose(), "fresh_value");
    }

    #[test]
    fn import_overwrites_existing_when_requested() {
        let (store, _file) = temp_store();
        store
            .set("EXISTING", "original", SecretCategory::System)
            .expect("set");

        let export = ExportData {
            version: 1,
            encrypted: false,
            entries: vec![ExportEntry {
                name: "EXISTING".to_string(),
                value: "overwritten".to_string(),
                category: SecretCategory::Tool,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
        };

        let result = store.import_all(&export, true).expect("import");
        assert_eq!(result.imported, 1);
        assert!(result.skipped.is_empty());

        assert_eq!(store.get("EXISTING").expect("get").expose(), "overwritten");
    }

    #[test]
    fn set_updates_existing_preserves_created_at() {
        let (store, _file) = temp_store();
        store
            .set("MY_KEY", "v1", SecretCategory::Tool)
            .expect("set");
        let meta1 = store.get_metadata("MY_KEY").expect("meta");

        // Update value and category.
        store
            .set("MY_KEY", "v2", SecretCategory::System)
            .expect("update");
        let meta2 = store.get_metadata("MY_KEY").expect("meta");

        assert_eq!(meta2.created_at, meta1.created_at);
        assert!(meta2.updated_at >= meta1.updated_at);
        assert_eq!(meta2.category, SecretCategory::System);
        assert_eq!(store.get("MY_KEY").expect("get").expose(), "v2");
    }

    #[test]
    fn secret_field_name_generation() {
        // Adapter field with instance pattern.
        let field = SecretField {
            toml_key: "token",
            secret_name: "DISCORD_BOT_TOKEN",
            instance_pattern: Some(InstancePattern {
                platform_prefix: "DISCORD",
                field_suffix: "BOT_TOKEN",
            }),
        };

        assert_eq!(field.secret_name, "DISCORD_BOT_TOKEN");
        assert_eq!(
            field.instance_name("alerts"),
            Some("DISCORD_ALERTS_BOT_TOKEN".to_string())
        );
        assert_eq!(
            field.instance_name("my-bot"),
            Some("DISCORD_MY-BOT_BOT_TOKEN".to_string())
        );

        // Multi-field adapter (Slack).
        let bot = SecretField {
            toml_key: "bot_token",
            secret_name: "SLACK_BOT_TOKEN",
            instance_pattern: Some(InstancePattern {
                platform_prefix: "SLACK",
                field_suffix: "BOT_TOKEN",
            }),
        };
        let app = SecretField {
            toml_key: "app_token",
            secret_name: "SLACK_APP_TOKEN",
            instance_pattern: Some(InstancePattern {
                platform_prefix: "SLACK",
                field_suffix: "APP_TOKEN",
            }),
        };
        assert_eq!(bot.secret_name, "SLACK_BOT_TOKEN");
        assert_eq!(
            bot.instance_name("support"),
            Some("SLACK_SUPPORT_BOT_TOKEN".to_string())
        );
        assert_eq!(app.secret_name, "SLACK_APP_TOKEN");
        assert_eq!(
            app.instance_name("support"),
            Some("SLACK_SUPPORT_APP_TOKEN".to_string())
        );

        // Non-adapter field (LLM key) — no instance pattern.
        let llm_field = SecretField {
            toml_key: "anthropic_key",
            secret_name: "ANTHROPIC_API_KEY",
            instance_pattern: None,
        };
        assert_eq!(llm_field.secret_name, "ANTHROPIC_API_KEY");
        assert_eq!(llm_field.instance_name("anything"), None);
    }

    #[test]
    fn system_secret_registry_contains_all_sections() {
        let fields = system_secret_registry();

        // Helper: check if a secret name is in the registry.
        let has_secret = |name: &str| fields.iter().any(|f| f.secret_name == name);

        // LLM provider keys.
        assert!(has_secret("ANTHROPIC_API_KEY"), "missing ANTHROPIC_API_KEY");
        assert!(has_secret("OPENAI_API_KEY"), "missing OPENAI_API_KEY");
        assert!(has_secret("DEEPSEEK_API_KEY"), "missing DEEPSEEK_API_KEY");

        // Search / internal tool keys.
        assert!(
            has_secret("BRAVE_SEARCH_API_KEY"),
            "missing BRAVE_SEARCH_API_KEY"
        );

        // Messaging adapter defaults.
        assert!(has_secret("DISCORD_BOT_TOKEN"), "missing DISCORD_BOT_TOKEN");
        assert!(has_secret("SLACK_BOT_TOKEN"), "missing SLACK_BOT_TOKEN");
        assert!(has_secret("SLACK_APP_TOKEN"), "missing SLACK_APP_TOKEN");
        assert!(
            has_secret("TELEGRAM_BOT_TOKEN"),
            "missing TELEGRAM_BOT_TOKEN"
        );
        assert!(
            has_secret("TWITCH_OAUTH_TOKEN"),
            "missing TWITCH_OAUTH_TOKEN"
        );
        assert!(
            has_secret("EMAIL_IMAP_PASSWORD"),
            "missing EMAIL_IMAP_PASSWORD"
        );

        // Adapter fields have instance patterns, LLM fields don't.
        let discord_field = fields
            .iter()
            .find(|f| f.secret_name == "DISCORD_BOT_TOKEN")
            .expect("Discord field");
        assert!(discord_field.instance_pattern.is_some());

        let anthropic_field = fields
            .iter()
            .find(|f| f.secret_name == "ANTHROPIC_API_KEY")
            .expect("Anthropic field");
        assert!(anthropic_field.instance_pattern.is_none());
    }
}
