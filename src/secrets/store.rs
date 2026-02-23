//! Encrypted credentials storage (AES-256-GCM, redb).

use crate::error::SecretsError;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use rand::RngCore;
use redb::{Database, ReadableTable, TableDefinition};
use sha2::{Digest, Sha256};
use std::fmt::{Debug, Display, Formatter};
use std::path::Path;

const SECRETS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("secrets");

pub struct DecryptedSecret(String);

impl DecryptedSecret {
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

pub struct SecretsStore {
    db: Database,
}

impl SecretsStore {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SecretsError> {
        let db = Database::create(path.as_ref()).map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to open secrets database: {error}"))
        })?;

        let write_transaction = db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to initialize secrets table transaction: {error}"
            ))
        })?;
        {
            let _table = write_transaction
                .open_table(SECRETS_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
                })?;
        }
        write_transaction.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to commit secrets table initialization: {error}"
            ))
        })?;

        Ok(Self { db })
    }

    pub fn set(&self, key: &str, value: &str, master_key: &[u8]) -> Result<(), SecretsError> {
        let cipher = build_cipher(master_key)?;

        let mut nonce_bytes = [0_u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, value.as_bytes())
            .map_err(|error| SecretsError::EncryptionFailed(error.to_string()))?;

        let mut stored_value = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
        stored_value.extend_from_slice(&nonce_bytes);
        stored_value.extend_from_slice(&ciphertext);

        let write_transaction = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;

        {
            let mut table = write_transaction
                .open_table(SECRETS_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
                })?;

            table
                .insert(key, stored_value.as_slice())
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to insert secret '{key}': {error}"))
                })?;
        }

        write_transaction.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to commit secret '{key}': {error}"))
        })?;

        Ok(())
    }

    pub fn get(&self, key: &str, master_key: &[u8]) -> Result<DecryptedSecret, SecretsError> {
        let cipher = build_cipher(master_key)?;

        let read_transaction = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_transaction
            .open_table(SECRETS_TABLE)
            .map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;

        let value = table
            .get(key)
            .map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to read key '{key}': {error}"))
            })?
            .ok_or_else(|| SecretsError::NotFound {
                key: key.to_string(),
            })?;

        let encrypted_value = value.value();
        if encrypted_value.len() < 12 {
            return Err(SecretsError::DecryptionFailed(
                "stored secret is missing nonce prefix".to_string(),
            ));
        }

        let nonce = Nonce::from_slice(&encrypted_value[..12]);
        let ciphertext = &encrypted_value[12..];
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|error| SecretsError::DecryptionFailed(error.to_string()))?;

        let plaintext = String::from_utf8(plaintext)
            .map_err(|error| SecretsError::DecryptionFailed(error.to_string()))?;

        Ok(DecryptedSecret(plaintext))
    }

    pub fn delete(&self, key: &str) -> Result<(), SecretsError> {
        let write_transaction = self.db.begin_write().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to begin write transaction: {error}"
            ))
        })?;

        {
            let mut table = write_transaction
                .open_table(SECRETS_TABLE)
                .map_err(|error| {
                    SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
                })?;

            table.remove(key).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to remove key '{key}': {error}"))
            })?;
        }

        write_transaction.commit().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!(
                "failed to commit delete for '{key}': {error}"
            ))
        })?;

        Ok(())
    }

    pub fn list(&self) -> Result<Vec<String>, SecretsError> {
        let read_transaction = self.db.begin_read().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to begin read transaction: {error}"))
        })?;
        let table = read_transaction
            .open_table(SECRETS_TABLE)
            .map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to open secrets table: {error}"))
            })?;

        let mut keys = Vec::new();
        let iter = table.iter().map_err(|error| {
            SecretsError::Other(anyhow::anyhow!("failed to iterate secrets table: {error}"))
        })?;

        for entry in iter {
            let (key, _value) = entry.map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("failed to read secrets entry: {error}"))
            })?;
            keys.push(key.value().to_string());
        }

        Ok(keys)
    }
}

fn build_cipher(master_key: &[u8]) -> Result<Aes256Gcm, SecretsError> {
    if master_key.is_empty() {
        return Err(SecretsError::InvalidKey);
    }

    let mut hasher = Sha256::new();
    hasher.update(master_key);
    let digest = hasher.finalize();

    Aes256Gcm::new_from_slice(&digest).map_err(|_| SecretsError::InvalidKey)
}
