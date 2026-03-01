//! OS credential store abstraction for master key storage.
//!
//! The master key never exists as an environment variable or a file on disk.
//! It lives in kernel-level credential storage that is inaccessible to worker
//! subprocesses regardless of sandbox state.
//!
//! - **macOS:** Keychain via the Security framework. Access controlled by code
//!   signature — worker subprocesses (bash, python, etc.) are different binaries
//!   and cannot retrieve the key.
//! - **Linux:** Kernel keyring via `keyctl`. The key lives in kernel memory,
//!   scoped to a session keyring. Workers are spawned with a fresh empty session
//!   keyring via `pre_exec`.

use crate::error::SecretsError;

/// Service name used for Keychain/keyring identification.
const SERVICE_NAME: &str = "sh.spacebot.master-key";

/// Trait for OS-level credential storage.
///
/// Implementations must be Send + Sync for use in async contexts.
pub trait KeyStore: Send + Sync {
    /// Store the master key for the given instance.
    fn store_key(&self, instance_id: &str, key: &[u8]) -> Result<(), SecretsError>;

    /// Retrieve the master key for the given instance.
    fn load_key(&self, instance_id: &str) -> Result<Option<Vec<u8>>, SecretsError>;

    /// Remove the master key from the credential store.
    fn delete_key(&self, instance_id: &str) -> Result<(), SecretsError>;
}

/// Create the platform-appropriate keystore.
pub fn platform_keystore() -> Box<dyn KeyStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(MacOSKeyStore)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxKeyStore)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        tracing::warn!("no OS keystore available on this platform — master key storage disabled");
        Box::new(NoopKeyStore)
    }
}

#[cfg(target_os = "macos")]
pub struct MacOSKeyStore;

#[cfg(target_os = "macos")]
impl KeyStore for MacOSKeyStore {
    fn store_key(&self, instance_id: &str, key: &[u8]) -> Result<(), SecretsError> {
        use security_framework::passwords::{delete_generic_password, set_generic_password};

        // Delete any existing entry first (set_generic_password fails on duplicate).
        let _ = delete_generic_password(SERVICE_NAME, instance_id);

        set_generic_password(SERVICE_NAME, instance_id, key)
            .map_err(|error| SecretsError::Other(anyhow::anyhow!("keychain store failed: {error}")))
    }

    fn load_key(&self, instance_id: &str) -> Result<Option<Vec<u8>>, SecretsError> {
        use security_framework::passwords::get_generic_password;

        match get_generic_password(SERVICE_NAME, instance_id) {
            Ok(data) => Ok(Some(data.to_vec())),
            Err(error) => {
                // errSecItemNotFound means no key stored — not an error.
                let code = error.code();
                if code == -25300 {
                    // errSecItemNotFound
                    Ok(None)
                } else {
                    Err(SecretsError::Other(anyhow::anyhow!(
                        "keychain load failed: {error}"
                    )))
                }
            }
        }
    }

    fn delete_key(&self, instance_id: &str) -> Result<(), SecretsError> {
        use security_framework::passwords::delete_generic_password;

        match delete_generic_password(SERVICE_NAME, instance_id) {
            Ok(()) => Ok(()),
            Err(error) => {
                let code = error.code();
                if code == -25300 {
                    Ok(()) // Already gone
                } else {
                    Err(SecretsError::Other(anyhow::anyhow!(
                        "keychain delete failed: {error}"
                    )))
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
use std::ffi::CString;

#[cfg(target_os = "linux")]
pub struct LinuxKeyStore;

#[cfg(target_os = "linux")]
impl KeyStore for LinuxKeyStore {
    fn store_key(&self, instance_id: &str, key: &[u8]) -> Result<(), SecretsError> {
        let description =
            CString::new(format!("{SERVICE_NAME}:{instance_id}")).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("invalid key description: {error}"))
            })?;

        // Get the session keyring.
        let session_keyring = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x00_i64, // KEYCTL_GET_KEYRING_ID
                -3_i64,   // KEY_SPEC_SESSION_KEYRING
                0_i64,    // don't create
            )
        };
        if session_keyring < 0 {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "failed to get session keyring: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Add the key. If it already exists with the same description,
        // this replaces it (add_key with same type+description = update).
        let result = unsafe {
            libc::syscall(
                libc::SYS_add_key,
                b"user\0".as_ptr(),
                description.as_ptr(),
                key.as_ptr(),
                key.len(),
                session_keyring,
            )
        };
        if result < 0 {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "keyctl add_key failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        Ok(())
    }

    fn load_key(&self, instance_id: &str) -> Result<Option<Vec<u8>>, SecretsError> {
        let description =
            CString::new(format!("{SERVICE_NAME}:{instance_id}")).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("invalid key description: {error}"))
            })?;

        // Search the session keyring for a "user" type key with our description.
        let key_id = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x0a_i64, // KEYCTL_SEARCH
                -3_i64,   // KEY_SPEC_SESSION_KEYRING
                b"user\0".as_ptr(),
                description.as_ptr(),
                0_i64, // don't link to a destination keyring
            )
        };
        if key_id < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOKEY) {
                return Ok(None);
            }
            return Err(SecretsError::Other(anyhow::anyhow!(
                "keyctl search failed: {error}"
            )));
        }

        // Read the key payload.
        // First call with null buffer to get the size.
        let size = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x0b_i64, // KEYCTL_READ
                key_id,
                std::ptr::null::<u8>(),
                0_usize,
            )
        };
        if size < 0 {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "keyctl read (size) failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mut buffer = vec![0u8; size as usize];
        let read = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x0b_i64, // KEYCTL_READ
                key_id,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if read < 0 {
            return Err(SecretsError::Other(anyhow::anyhow!(
                "keyctl read failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        buffer.truncate(read as usize);
        Ok(Some(buffer))
    }

    fn delete_key(&self, instance_id: &str) -> Result<(), SecretsError> {
        let description =
            CString::new(format!("{SERVICE_NAME}:{instance_id}")).map_err(|error| {
                SecretsError::Other(anyhow::anyhow!("invalid key description: {error}"))
            })?;

        // Find the key first.
        let key_id = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x0a_i64, // KEYCTL_SEARCH
                -3_i64,   // KEY_SPEC_SESSION_KEYRING
                b"user\0".as_ptr(),
                description.as_ptr(),
                0_i64,
            )
        };
        if key_id < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOKEY) {
                // Not found — already deleted.
                return Ok(());
            }
            return Err(SecretsError::Other(anyhow::anyhow!(
                "keyctl search failed: {error}"
            )));
        }

        // Invalidate the key.
        let result = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                0x15_i64, // KEYCTL_INVALIDATE
                key_id,
            )
        };
        if result < 0 {
            // KEYCTL_INVALIDATE may not be available on older kernels.
            // Fall back to KEYCTL_REVOKE.
            let revoke = unsafe {
                libc::syscall(
                    libc::SYS_keyctl,
                    0x03_i64, // KEYCTL_REVOKE
                    key_id,
                )
            };
            if revoke < 0 {
                return Err(SecretsError::Other(anyhow::anyhow!(
                    "keyctl revoke failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }

        Ok(())
    }
}

/// Spawn a worker subprocess with a fresh empty session keyring (Linux only).
///
/// Must be called via `Command::pre_exec()` before `spawn()`. The child gets
/// a new session keyring and cannot access the parent's keyring (which holds
/// the master key).
///
/// # Safety
///
/// This function is intended to be called from `pre_exec` which runs between
/// fork and exec — only async-signal-safe operations are permitted. `keyctl`
/// is a direct syscall and is safe in this context.
#[cfg(target_os = "linux")]
pub unsafe fn pre_exec_new_session_keyring() -> std::io::Result<()> {
    // KEYCTL_JOIN_SESSION_KEYRING with NULL name creates a new anonymous
    // session keyring for this process.
    let result = libc::syscall(
        libc::SYS_keyctl,
        0x01_i64, // KEYCTL_JOIN_SESSION_KEYRING
        std::ptr::null::<libc::c_char>(),
    );
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
struct NoopKeyStore;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl KeyStore for NoopKeyStore {
    fn store_key(&self, _instance_id: &str, _key: &[u8]) -> Result<(), SecretsError> {
        Err(SecretsError::Other(anyhow::anyhow!(
            "no OS keystore available on this platform"
        )))
    }

    fn load_key(&self, _instance_id: &str) -> Result<Option<Vec<u8>>, SecretsError> {
        Ok(None)
    }

    fn delete_key(&self, _instance_id: &str) -> Result<(), SecretsError> {
        Ok(())
    }
}
