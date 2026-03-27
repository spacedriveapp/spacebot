//! GitHub Copilot OAuth device code flow.
//!
//! Implements the standard GitHub OAuth 2.0 Device Authorization Grant (RFC 8628)
//! to obtain a GitHub token that can be exchanged for a Copilot API token via
//! the existing `github_copilot_auth::exchange_github_token()` flow.
//!
//! This allows users to authenticate via browser instead of providing a PAT.
//! The resulting GitHub OAuth token is stored separately from static PAT config
//! so it cannot shadow a manually configured key.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

/// GitHub OAuth App client ID used by OpenCode/Copilot CLI tools.
const CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

/// GitHub device code request endpoint.
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";

/// GitHub OAuth token endpoint.
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Default verification URL shown to the user.
const DEFAULT_VERIFICATION_URL: &str = "https://github.com/login/device";

/// OAuth scope requested — read:user is sufficient for Copilot token exchange.
const SCOPE: &str = "read:user";

/// Stored GitHub OAuth credentials from the device code flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    /// GitHub device flow tokens don't expire by default, but we store the
    /// token_type for completeness.
    pub token_type: String,
    /// OAuth scope granted.
    pub scope: String,
}

/// Response from GitHub's device code endpoint.
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Recommended polling interval in seconds.
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// Time in seconds before the device code expires.
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

fn default_expires_in() -> u64 {
    900
}

/// Result of a single poll attempt.
#[derive(Debug, Clone)]
pub enum DeviceTokenPollResult {
    /// User has not yet authorized — keep polling.
    Pending,
    /// Server asked us to slow down — increase interval.
    SlowDown,
    /// User authorized — here are the credentials.
    Approved(OAuthCredentials),
}

/// Step 1: Request a device code from GitHub.
pub async fn request_device_code() -> Result<DeviceCodeResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .context("failed to send GitHub device code request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read GitHub device code response")?;

    if !status.is_success() {
        anyhow::bail!("GitHub device code request failed ({}): {}", status, body);
    }

    serde_json::from_str::<DeviceCodeResponse>(&body)
        .context("failed to parse GitHub device code response")
}

/// GitHub token endpoint response (success case).
#[derive(Debug, Deserialize)]
struct TokenSuccessResponse {
    access_token: String,
    token_type: String,
    scope: String,
}

/// GitHub token endpoint error response.
///
/// GitHub returns errors as 200 OK with `error` and `error_description` fields
/// (not as HTTP error status codes).
#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

/// Step 2: Poll the GitHub token endpoint once.
pub async fn poll_device_token(device_code: &str) -> Result<DeviceTokenPollResult> {
    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .context("failed to send GitHub device token poll request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read GitHub device token poll response")?;

    if !status.is_success() {
        anyhow::bail!("GitHub device token poll failed ({}): {}", status, body);
    }

    // GitHub returns 200 for both success and pending/error states.
    // Try parsing as success first.
    if let Ok(success) = serde_json::from_str::<TokenSuccessResponse>(&body)
        && !success.access_token.is_empty()
    {
        return Ok(DeviceTokenPollResult::Approved(OAuthCredentials {
            access_token: success.access_token,
            token_type: success.token_type,
            scope: success.scope,
        }));
    }

    // Parse as error response
    if let Ok(error_response) = serde_json::from_str::<TokenErrorResponse>(&body) {
        match error_response.error.as_deref() {
            Some("authorization_pending") => return Ok(DeviceTokenPollResult::Pending),
            Some("slow_down") => return Ok(DeviceTokenPollResult::SlowDown),
            Some("expired_token") => {
                anyhow::bail!("Device code expired. Please start the authorization again.");
            }
            Some("access_denied") => {
                anyhow::bail!("Authorization was denied by the user.");
            }
            Some(error) => {
                let description = error_response
                    .error_description
                    .as_deref()
                    .unwrap_or("no description");
                anyhow::bail!(
                    "GitHub device token poll error: {} — {}",
                    error,
                    description
                );
            }
            None => {}
        }
    }

    anyhow::bail!(
        "GitHub device token poll returned unexpected response: {}",
        body
    );
}

/// Determine which verification URL to show the user.
pub fn device_verification_url(response: &DeviceCodeResponse) -> String {
    let url = response.verification_uri.trim();
    if url.is_empty() {
        DEFAULT_VERIFICATION_URL.to_string()
    } else {
        url.to_string()
    }
}

/// Path to GitHub Copilot OAuth credentials within the instance directory.
pub fn credentials_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join("github_copilot_oauth.json")
}

/// Load GitHub Copilot OAuth credentials from disk.
pub fn load_credentials(instance_dir: &Path) -> Result<Option<OAuthCredentials>> {
    let path = credentials_path(instance_dir);
    if !path.exists() {
        return Ok(None);
    }

    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let creds: OAuthCredentials =
        serde_json::from_str(&data).context("failed to parse GitHub Copilot OAuth credentials")?;
    Ok(Some(creds))
}

/// Save GitHub Copilot OAuth credentials to disk with restricted permissions (0600).
pub fn save_credentials(instance_dir: &Path, creds: &OAuthCredentials) -> Result<()> {
    let path = credentials_path(instance_dir);
    let data = serde_json::to_string_pretty(creds)
        .context("failed to serialize GitHub Copilot OAuth credentials")?;

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| {
                format!(
                    "failed to create {} with restricted permissions",
                    path.display()
                )
            })?;
        file.write_all(data.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(&path, &data)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let creds = OAuthCredentials {
            access_token: "ghu_test123".to_string(),
            token_type: "bearer".to_string(),
            scope: "read:user".to_string(),
        };

        save_credentials(dir.path(), &creds).unwrap();
        let loaded = load_credentials(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.access_token, "ghu_test123");
        assert_eq!(loaded.token_type, "bearer");
        assert_eq!(loaded.scope, "read:user");
    }

    #[test]
    fn load_credentials_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_credentials(dir.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn device_verification_url_uses_response_value() {
        let response = DeviceCodeResponse {
            device_code: "test".to_string(),
            user_code: "TEST-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            interval: 5,
            expires_in: 900,
        };
        assert_eq!(
            device_verification_url(&response),
            "https://github.com/login/device"
        );
    }

    #[test]
    fn device_verification_url_uses_default_when_empty() {
        let response = DeviceCodeResponse {
            device_code: "test".to_string(),
            user_code: "TEST-1234".to_string(),
            verification_uri: "".to_string(),
            interval: 5,
            expires_in: 900,
        };
        assert_eq!(device_verification_url(&response), DEFAULT_VERIFICATION_URL);
    }
}
