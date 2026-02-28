//! SSH server management.
//!
//! Manages the lifecycle of an sshd child process. When enabled via config,
//! generates host keys (persisted on the data volume) and starts sshd in
//! foreground mode as a tokio-managed child process.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::{Child, Command};

/// Manages an sshd child process.
pub struct SshManager {
    child: Option<Child>,
    ssh_dir: PathBuf,
}

impl SshManager {
    pub fn new(instance_dir: &Path) -> Self {
        Self {
            child: None,
            ssh_dir: instance_dir.join("ssh"),
        }
    }

    /// Start sshd if not already running. Generates host keys on first call.
    pub async fn start(&mut self, port: u16) -> Result<()> {
        if self.is_running() {
            return Ok(());
        }

        tokio::fs::create_dir_all(&self.ssh_dir)
            .await
            .context("failed to create ssh directory")?;

        // Generate host key if missing
        let host_key = self.ssh_dir.join("ssh_host_ed25519_key");
        if !host_key.exists() {
            let output = Command::new("ssh-keygen")
                .args([
                    "-t",
                    "ed25519",
                    "-f",
                    host_key.to_str().unwrap(),
                    "-N",
                    "",
                    "-q",
                ])
                .output()
                .await
                .context("failed to run ssh-keygen")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("ssh-keygen failed: {stderr}");
            }
            tracing::info!("generated SSH host key");
        }

        // sshd requires /run/sshd to exist
        if let Err(error) = tokio::fs::create_dir_all("/run/sshd").await {
            tracing::warn!(%error, "could not create /run/sshd (may not have permissions)");
        }

        let authorized_keys = self.ssh_dir.join("authorized_keys");
        let child = Command::new("/usr/sbin/sshd")
            .args([
                "-D", // foreground
                "-e", // log to stderr
                "-h",
                host_key.to_str().unwrap(),
                "-o",
                &format!("Port={port}"),
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "PermitRootLogin=prohibit-password",
                "-o",
                &format!("AuthorizedKeysFile={}", authorized_keys.to_str().unwrap()),
                "-o",
                "ListenAddress=[::]",
            ])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("failed to start sshd")?;

        tracing::info!(port, "sshd started");
        self.child = Some(child);
        Ok(())
    }

    /// Stop sshd if running.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill().await.context("failed to kill sshd")?;
            child.wait().await.context("failed to wait for sshd")?;
            tracing::info!("sshd stopped");
        }
        Ok(())
    }

    /// Write an authorized public key for root access.
    pub async fn set_authorized_key(&self, pubkey: &str) -> Result<()> {
        tokio::fs::create_dir_all(&self.ssh_dir)
            .await
            .context("failed to create ssh directory")?;

        let path = self.ssh_dir.join("authorized_keys");
        tokio::fs::write(&path, format!("{}\n", pubkey.trim()))
            .await
            .context("failed to write authorized_keys")?;

        // ssh requires strict permissions on authorized_keys
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            tokio::fs::set_permissions(&path, perms)
                .await
                .context("failed to set authorized_keys permissions")?;
        }

        tracing::info!("SSH authorized key updated");
        Ok(())
    }

    /// Remove all authorized keys.
    pub async fn clear_authorized_keys(&self) -> Result<()> {
        let path = self.ssh_dir.join("authorized_keys");
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .context("failed to remove authorized_keys")?;
            tracing::info!("SSH authorized keys cleared");
        }
        Ok(())
    }

    /// Returns true if sshd is running.
    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    // Process exited
                    self.child = None;
                    false
                }
                Ok(None) => true,
                Err(_) => {
                    self.child = None;
                    false
                }
            },
            None => false,
        }
    }

    /// Returns true if an authorized_keys file exists and is non-empty.
    pub fn has_authorized_key(&self) -> bool {
        let path = self.ssh_dir.join("authorized_keys");
        path.exists()
            && std::fs::metadata(&path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
    }
}
