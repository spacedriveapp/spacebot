//! Anthropic OAuth authentication: PKCE flow, token exchange, refresh, and storage.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Stored OAuth credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Expiry as Unix timestamp in milliseconds.
    pub expires_at: i64,
}

impl OAuthCredentials {
    /// Check if the access token is expired or about to expire (within 5 minutes).
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let buffer = 5 * 60 * 1000; // 5 minutes
        now >= self.expires_at - buffer
    }

    /// Refresh the access token. Returns new credentials with updated tokens.
    pub async fn refresh(&self) -> Result<Self> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": self.refresh_token,
            "client_id": CLIENT_ID,
        });

        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("failed to send refresh request")?;

        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read refresh response")?;

        if !status.is_success() {
            anyhow::bail!("token refresh failed ({}): {}", status, text);
        }

        let json: TokenResponse =
            serde_json::from_str(&text).context("failed to parse refresh response")?;

        Ok(Self {
            access_token: json.access_token,
            refresh_token: json.refresh_token,
            expires_at: chrono::Utc::now().timestamp_millis() + json.expires_in * 1000,
        })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

/// PKCE verifier/challenge pair.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a PKCE verifier (64 random bytes, base64url-encoded) and S256 challenge.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hash);

    Pkce {
        verifier,
        challenge,
    }
}

/// OAuth authorization mode.
#[derive(Debug, Clone, Copy)]
pub enum AuthMode {
    /// Claude Pro/Max subscription (claude.ai)
    Max,
    /// API console (console.anthropic.com)
    Console,
}

/// Build the authorization URL and return it with the PKCE verifier.
pub fn authorize_url(mode: AuthMode) -> (String, String) {
    let pkce = generate_pkce();

    let base = match mode {
        AuthMode::Max => "https://claude.ai/oauth/authorize",
        AuthMode::Console => "https://console.anthropic.com/oauth/authorize",
    };

    let url = format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        base,
        CLIENT_ID,
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode(SCOPES),
        pkce.challenge,
        pkce.verifier,
    );

    (url, pkce.verifier)
}

/// Exchange an authorization code for OAuth tokens.
///
/// The code from the browser is in the form `<code>#<state>`.
pub async fn exchange_code(code_with_state: &str, verifier: &str) -> Result<OAuthCredentials> {
    let (code, state) = code_with_state
        .split_once('#')
        .unwrap_or((code_with_state, ""));

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "code": code,
        "state": state,
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });

    let response = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("failed to send token exchange request")?;

    let status = response.status();
    let text = response
        .text()
        .await
        .context("failed to read token exchange response")?;

    if !status.is_success() {
        anyhow::bail!("token exchange failed ({}): {}", status, text);
    }

    let json: TokenResponse =
        serde_json::from_str(&text).context("failed to parse token exchange response")?;

    Ok(OAuthCredentials {
        access_token: json.access_token,
        refresh_token: json.refresh_token,
        expires_at: chrono::Utc::now().timestamp_millis() + json.expires_in * 1000,
    })
}

/// Path to the Anthropic OAuth credentials file within the instance directory.
pub fn credentials_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join("anthropic_oauth.json")
}

/// Load stored credentials from disk.
pub fn load_credentials(instance_dir: &Path) -> Result<Option<OAuthCredentials>> {
    let path = credentials_path(instance_dir);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let creds: OAuthCredentials =
        serde_json::from_str(&data).context("failed to parse auth.json")?;
    Ok(Some(creds))
}

/// Save credentials to disk with restricted permissions (0600).
pub fn save_credentials(instance_dir: &Path, creds: &OAuthCredentials) -> Result<()> {
    let path = credentials_path(instance_dir);
    let data = serde_json::to_string_pretty(creds).context("failed to serialize credentials")?;

    std::fs::write(&path, &data)
        .with_context(|| format!("failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

/// Run the interactive OAuth login flow. Prints URL, prompts for code, exchanges tokens.
pub async fn login_interactive(instance_dir: &Path, mode: AuthMode) -> Result<OAuthCredentials> {
    let (url, verifier) = authorize_url(mode);

    eprintln!("Open this URL in your browser:\n");
    eprintln!("  {url}\n");

    // Try to open the browser automatically
    if let Err(_error) = open::that(&url) {
        eprintln!("(Could not open browser automatically, please copy the URL above)");
    }

    eprintln!("After authorizing, paste the code here:");
    eprint!("> ");

    let mut code = String::new();
    std::io::stdin()
        .read_line(&mut code)
        .context("failed to read authorization code from stdin")?;
    let code = code.trim();

    if code.is_empty() {
        anyhow::bail!("no authorization code provided");
    }

    let creds = exchange_code(code, &verifier)
        .await
        .context("failed to exchange authorization code")?;

    save_credentials(instance_dir, &creds).context("failed to save credentials")?;

    eprintln!("Login successful. Credentials saved to {}", credentials_path(instance_dir).display());

    Ok(creds)
}
