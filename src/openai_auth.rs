//! OpenAI ChatGPT Plus OAuth device code flow, token exchange, refresh, and storage.

use anyhow::{Context as _, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEFAULT_DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";

/// Stored OpenAI OAuth credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Expiry as Unix timestamp in milliseconds.
    pub expires_at: i64,
    pub account_id: Option<String>,
}

impl OAuthCredentials {
    /// Check if the access token is expired or about to expire (within 5 minutes).
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let buffer = 5 * 60 * 1000;
        now >= self.expires_at - buffer
    }

    /// Refresh the access token and return updated credentials.
    pub async fn refresh(&self) -> Result<Self> {
        let client = reqwest::Client::new();
        let response = client
            .post(OAUTH_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.as_str()),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .context("failed to send OpenAI OAuth refresh request")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read OpenAI OAuth refresh response")?;

        if !status.is_success() {
            anyhow::bail!("OpenAI OAuth refresh failed ({}): {}", status, body);
        }

        let token_response: TokenResponse =
            serde_json::from_str(&body).context("failed to parse OpenAI OAuth refresh response")?;

        let account_id = extract_account_id(&token_response).or_else(|| self.account_id.clone());
        let refresh_token = token_response
            .refresh_token
            .unwrap_or_else(|| self.refresh_token.clone());

        Ok(Self {
            access_token: token_response.access_token,
            refresh_token,
            expires_at: chrono::Utc::now().timestamp_millis()
                + token_response.expires_in.unwrap_or(3600) * 1000,
            account_id,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenClaims {
    chatgpt_account_id: Option<String>,
    organizations: Option<Vec<TokenOrganization>>,
    #[serde(rename = "https://api.openai.com/auth")]
    openai_auth: Option<TokenOpenAiAuthClaims>,
}

#[derive(Debug, Deserialize)]
struct TokenOrganization {
    id: String,
}

#[derive(Debug, Deserialize)]
struct TokenOpenAiAuthClaims {
    chatgpt_account_id: Option<String>,
}

fn parse_jwt_claims(token: &str) -> Option<TokenClaims> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<TokenClaims>(&decoded).ok()
}

fn extract_account_id(token_response: &TokenResponse) -> Option<String> {
    let from_claims = |claims: TokenClaims| {
        claims
            .chatgpt_account_id
            .or_else(|| claims.openai_auth.and_then(|auth| auth.chatgpt_account_id))
            .or_else(|| {
                claims
                    .organizations
                    .and_then(|organizations| organizations.into_iter().next())
                    .map(|organization| organization.id)
            })
    };

    token_response
        .id_token
        .as_deref()
        .and_then(parse_jwt_claims)
        .and_then(from_claims)
        .or_else(|| parse_jwt_claims(&token_response.access_token).and_then(from_claims))
}

fn deserialize_optional_u64<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<u64>, D::Error> {
    use serde::de::Error;

    let value: Option<serde_json::Value> = Option::deserialize(d)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| Error::custom("expected positive integer")),
        Some(serde_json::Value::String(value)) => value
            .parse()
            .map(Some)
            .map_err(|error| Error::custom(format!("invalid integer: {error}"))),
        Some(other) => Err(Error::custom(format!(
            "expected string or number, got {other}"
        ))),
    }
}

/// Response from the OpenAI device-code usercode endpoint.
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_auth_id: String,
    pub user_code: String,
    /// Recommended polling interval in seconds (API may return this as a string).
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub interval: Option<u64>,
    /// Time in seconds before the device code expires (API may return this as a string).
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub expires_in: Option<u64>,
    #[serde(default, alias = "verification_uri", alias = "verification_url")]
    pub verification_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenSuccessResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceTokenGrant {
    pub authorization_code: String,
    pub code_verifier: String,
}

#[derive(Debug, Clone)]
pub enum DeviceTokenPollResult {
    Pending,
    SlowDown,
    Approved(DeviceTokenGrant),
}

/// Step 1: Request a device code and user code from OpenAI.
pub async fn request_device_code() -> Result<DeviceCodeResponse> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "client_id": CLIENT_ID });

    let response = client
        .post(DEVICE_USERCODE_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("failed to send OpenAI device-code usercode request")?;

    let status = response.status();
    let text = response
        .text()
        .await
        .context("failed to read OpenAI device-code usercode response")?;

    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "Device code login is not enabled. Please enable it in your ChatGPT security settings at https://chatgpt.com/security-settings and try again."
        );
    }

    if !status.is_success() {
        anyhow::bail!(
            "OpenAI device-code usercode request failed ({}): {}",
            status,
            text
        );
    }

    serde_json::from_str::<DeviceCodeResponse>(&text)
        .context("failed to parse OpenAI device-code usercode response")
}

/// Step 2: Poll the device token endpoint once.
pub async fn poll_device_token(
    device_auth_id: &str,
    user_code: &str,
) -> Result<DeviceTokenPollResult> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "device_auth_id": device_auth_id,
        "user_code": user_code,
    });

    let response = client
        .post(DEVICE_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("failed to send OpenAI device-code token poll request")?;

    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(DeviceTokenPollResult::Pending);
    }

    let body = response
        .text()
        .await
        .context("failed to read OpenAI device-code token poll response")?;

    if status.is_success() {
        let device_token: DeviceTokenSuccessResponse = serde_json::from_str(&body)
            .context("failed to parse OpenAI device-code token poll response")?;

        return Ok(DeviceTokenPollResult::Approved(DeviceTokenGrant {
            authorization_code: device_token.authorization_code,
            code_verifier: device_token.code_verifier,
        }));
    }

    if (status == reqwest::StatusCode::BAD_REQUEST
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
        && let Ok(error_response) = serde_json::from_str::<DeviceTokenErrorResponse>(&body)
    {
        if matches!(
            error_response.error.as_deref(),
            Some("authorization_pending")
        ) {
            return Ok(DeviceTokenPollResult::Pending);
        }
        if matches!(error_response.error.as_deref(), Some("slow_down")) {
            return Ok(DeviceTokenPollResult::SlowDown);
        }
        if let Some(description) = error_response.error_description.as_deref() {
            anyhow::bail!(
                "OpenAI device-code token poll failed ({}): {}",
                status,
                description
            );
        }
    }

    anyhow::bail!(
        "OpenAI device-code token poll failed ({}): {}",
        status,
        body
    );
}

/// Determine which verification URL to show the user.
pub fn device_verification_url(response: &DeviceCodeResponse) -> String {
    response
        .verification_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_DEVICE_VERIFICATION_URL.to_string())
}

/// Step 3: Exchange the device authorization code for OAuth tokens.
pub async fn exchange_device_code(
    authorization_code: &str,
    code_verifier: &str,
) -> Result<OAuthCredentials> {
    let client = reqwest::Client::new();
    let response = client
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .context("failed to send OpenAI device-code token exchange request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read OpenAI device-code token exchange response")?;

    if !status.is_success() {
        anyhow::bail!(
            "OpenAI device-code token exchange failed ({}): {}",
            status,
            body
        );
    }

    let token_response: TokenResponse = serde_json::from_str(&body)
        .context("failed to parse OpenAI device-code token exchange response")?;
    let account_id = extract_account_id(&token_response);
    let refresh_token = token_response
        .refresh_token
        .context("OpenAI device-code token response did not include refresh_token")?;

    Ok(OAuthCredentials {
        access_token: token_response.access_token,
        refresh_token,
        expires_at: chrono::Utc::now().timestamp_millis()
            + token_response.expires_in.unwrap_or(3600) * 1000,
        account_id,
    })
}

/// Path to OpenAI OAuth credentials within the instance directory.
pub fn credentials_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join("openai_chatgpt_oauth.json")
}

/// Load OpenAI OAuth credentials from disk.
pub fn load_credentials(instance_dir: &Path) -> Result<Option<OAuthCredentials>> {
    let path = credentials_path(instance_dir);
    if !path.exists() {
        return Ok(None);
    }

    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let creds: OAuthCredentials =
        serde_json::from_str(&data).context("failed to parse OpenAI OAuth credentials")?;
    Ok(Some(creds))
}

/// Save OpenAI OAuth credentials to disk with restricted permissions (0600).
pub fn save_credentials(instance_dir: &Path, creds: &OAuthCredentials) -> Result<()> {
    let path = credentials_path(instance_dir);
    let data = serde_json::to_string_pretty(creds)
        .context("failed to serialize OpenAI OAuth credentials")?;

    std::fs::write(&path, &data).with_context(|| format!("failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}
