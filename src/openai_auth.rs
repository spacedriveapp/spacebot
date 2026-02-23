//! OpenAI ChatGPT Plus OAuth browser flow, token exchange, refresh, and storage.

use anyhow::{Context as _, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const BROWSER_SCOPES: &str = "openid profile email offline_access";

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

/// Data needed to complete OpenAI browser OAuth.
#[derive(Debug, Clone, Serialize)]
pub struct BrowserAuthorization {
    pub authorization_url: String,
    pub state: String,
    pub pkce_verifier: String,
}

fn generate_random_urlsafe_string(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_pkce() -> (String, String) {
    let verifier = generate_random_urlsafe_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Build a browser-based OAuth authorization URL using PKCE.
pub fn start_browser_authorization(redirect_uri: &str) -> BrowserAuthorization {
    let (pkce_verifier, pkce_challenge) = generate_pkce();
    let state = generate_random_urlsafe_string(32);

    let authorization_url = format!(
        "{authorize}?response_type=code&client_id={client_id}&redirect_uri={redirect_uri}&scope={scope}&code_challenge={challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=opencode&state={state}",
        authorize = AUTHORIZE_URL,
        client_id = urlencoding::encode(CLIENT_ID),
        redirect_uri = urlencoding::encode(redirect_uri),
        scope = urlencoding::encode(BROWSER_SCOPES),
        challenge = urlencoding::encode(&pkce_challenge),
        state = urlencoding::encode(&state),
    );

    BrowserAuthorization {
        authorization_url,
        state,
        pkce_verifier,
    }
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

/// Exchange an OAuth authorization code from browser flow for tokens.
pub async fn exchange_browser_code(
    code: &str,
    redirect_uri: &str,
    pkce_verifier: &str,
) -> Result<OAuthCredentials> {
    let client = reqwest::Client::new();
    let response = client
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", pkce_verifier),
        ])
        .send()
        .await
        .context("failed to exchange OpenAI browser authorization code for tokens")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read OpenAI browser token exchange response")?;

    if !status.is_success() {
        anyhow::bail!(
            "OpenAI browser token exchange failed ({}): {}",
            status,
            body
        );
    }

    let token_response: TokenResponse = serde_json::from_str(&body)
        .context("failed to parse OpenAI browser token exchange response")?;
    let account_id = extract_account_id(&token_response);
    let refresh_token = token_response
        .refresh_token
        .context("OpenAI browser token response did not include refresh_token")?;

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
