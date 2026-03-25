//! GitHub Copilot token exchange, caching, and base URL derivation.
//!
//! The user provides a GitHub PAT (e.g. from `gh auth token`). This module
//! exchanges it for a short-lived Copilot API token via the internal GitHub
//! endpoint, caches the result on disk, and derives the provider base URL
//! from the token's embedded `proxy-ep` field.

use anyhow::{Context as _, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

/// Default base URL when `proxy-ep` cannot be extracted from the token.
pub const DEFAULT_COPILOT_API_BASE_URL: &str = "https://api.individual.githubcopilot.com";

/// Safety margin (milliseconds) subtracted from the expiry time when checking
/// whether a cached token is still usable. Matches OpenCode's 5-minute buffer.
const EXPIRY_SAFETY_MARGIN_MS: i64 = 5 * 60 * 1000;

static PROXY_EP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|;)\s*proxy-ep=([^;\s]+)").unwrap());

/// Cached Copilot API token stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotToken {
    pub token: String,
    /// SHA-256 hex digest of the GitHub PAT used to obtain this token.
    pub pat_hash: String,
    /// Expiry as Unix timestamp in milliseconds.
    pub expires_at_ms: i64,
    /// When this cache entry was last updated (Unix milliseconds).
    pub updated_at_ms: i64,
}

impl CopilotToken {
    /// Check if the token is expired or about to expire (within 5 minutes).
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        now >= self.expires_at_ms - EXPIRY_SAFETY_MARGIN_MS
    }
}

/// Response from the GitHub Copilot token endpoint.
#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    token: String,
    expires_at: serde_json::Value,
}

/// Compute the SHA-256 hex digest of a GitHub PAT.
pub fn hash_pat(pat: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pat.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Exchange a GitHub PAT for a Copilot API token.
pub async fn exchange_github_token(
    client: &reqwest::Client,
    github_pat: &str,
    pat_hash: String,
) -> Result<CopilotToken> {
    let response = client
        .get(COPILOT_TOKEN_URL)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {github_pat}"))
        .header(
            "User-Agent",
            format!("spacebot/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .context("failed to send Copilot token exchange request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read Copilot token exchange response")?;

    if !status.is_success() {
        let hint = if status == reqwest::StatusCode::NOT_FOUND {
            " (hint: gh auth tokens may not have Copilot access)"
        } else {
            ""
        };
        anyhow::bail!(
            "Copilot token exchange failed ({}): {}{}",
            status,
            body,
            hint,
        );
    }

    let exchange: TokenExchangeResponse =
        serde_json::from_str(&body).context("failed to parse Copilot token exchange response")?;

    let expires_at_ms = parse_expires_at(&exchange.expires_at)
        .context("Copilot token response has invalid expires_at")?;

    Ok(CopilotToken {
        token: exchange.token,
        pat_hash,
        expires_at_ms,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Parse `expires_at` from the token response. GitHub returns a Unix timestamp
/// in seconds, but we defensively accept milliseconds too (heuristic: values
/// greater than 10 billion are treated as milliseconds).
fn parse_expires_at(value: &serde_json::Value) -> Option<i64> {
    let raw = match value {
        serde_json::Value::Number(number) => number.as_i64()?,
        serde_json::Value::String(string) => string.trim().parse::<i64>().ok()?,
        _ => return None,
    };

    // Heuristic: 10^10 seconds ≈ year 2286, so values above this are likely milliseconds.
    // This safely handles both formats for any reasonable token expiry.
    if raw > 10_000_000_000 {
        Some(raw) // already milliseconds
    } else {
        Some(raw * 1000) // convert seconds to milliseconds
    }
}

/// Derive the Copilot API base URL from the token's embedded `proxy-ep` field.
///
/// The token is a semicolon-delimited set of key=value pairs. One of them is
/// `proxy-ep=proxy.individual.githubcopilot.com`. We extract the hostname,
/// replace `proxy.` with `api.`, and prefix `https://`.
///
/// Security: validates the hostname ends with `.githubcopilot.com` to prevent
/// a tampered cache file from redirecting requests to an arbitrary host.
pub fn derive_base_url_from_token(token: &str) -> Option<String> {
    let captures = PROXY_EP_RE.captures(token.trim())?;
    let proxy_ep = captures.get(1)?.as_str().trim();
    if proxy_ep.is_empty() {
        return None;
    }

    // Strip any protocol prefix
    let host = proxy_ep
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    // Extract just the hostname (no path, no port)
    let hostname = host.split('/').next()?.split(':').next()?;
    if hostname.is_empty() {
        return None;
    }

    // Security: validate expected suffix to prevent token exfiltration
    if !hostname.ends_with(".githubcopilot.com") {
        tracing::warn!(%hostname, "proxy-ep hostname has unexpected suffix; rejecting");
        return None;
    }

    // Replace proxy. → api.
    let api_host = if let Some(rest) = hostname.strip_prefix("proxy.") {
        format!("api.{rest}")
    } else {
        format!("api.{hostname}")
    };

    Some(format!("https://{api_host}"))
}

/// Path to the cached Copilot token within the instance directory.
pub fn credentials_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join("github_copilot_token.json")
}

/// Load a cached Copilot token from disk.
pub fn load_cached_token(instance_dir: &Path) -> Result<Option<CopilotToken>> {
    let path = credentials_path(instance_dir);
    if !path.exists() {
        return Ok(None);
    }

    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let token: CopilotToken =
        serde_json::from_str(&data).context("failed to parse cached Copilot token")?;
    Ok(Some(token))
}

/// Save a Copilot token to disk with restricted permissions (0600).
///
/// On Unix, creates the file atomically with mode 0o600 to avoid a brief
/// window where the file is readable by others. On non-Unix platforms,
/// falls back to a best-effort write.
pub fn save_cached_token(instance_dir: &Path, token: &CopilotToken) -> Result<()> {
    let path = credentials_path(instance_dir);
    let data = serde_json::to_string_pretty(token).context("failed to serialize Copilot token")?;

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
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
        use std::io::Write;
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
    fn derive_base_url_from_proxy_ep() {
        let token =
            "tid=abc123;exp=1234567890;proxy-ep=proxy.individual.githubcopilot.com;st=dotcom";
        assert_eq!(
            derive_base_url_from_token(token),
            Some("https://api.individual.githubcopilot.com".to_string())
        );
    }

    #[test]
    fn derive_base_url_no_proxy_ep() {
        let token = "tid=abc123;exp=1234567890;st=dotcom";
        assert_eq!(derive_base_url_from_token(token), None);
    }

    #[test]
    fn derive_base_url_empty_token() {
        assert_eq!(derive_base_url_from_token(""), None);
    }

    #[test]
    fn derive_base_url_rejects_invalid_suffix() {
        // Security test: reject proxy endpoints not ending with .githubcopilot.com
        let token = "tid=abc123;exp=1234567890;proxy-ep=proxy.evil.com;st=dotcom";
        assert_eq!(derive_base_url_from_token(token), None);
    }

    #[test]
    fn parse_expires_at_seconds() {
        let value = serde_json::json!(1700000000);
        assert_eq!(parse_expires_at(&value), Some(1700000000000));
    }

    #[test]
    fn parse_expires_at_milliseconds() {
        let value = serde_json::json!(1700000000000_i64);
        assert_eq!(parse_expires_at(&value), Some(1700000000000));
    }

    #[test]
    fn parse_expires_at_string() {
        let value = serde_json::json!("1700000000");
        assert_eq!(parse_expires_at(&value), Some(1700000000000));
    }

    #[test]
    fn hash_pat_deterministic() {
        let hash1 = hash_pat("ghu_abc123");
        let hash2 = hash_pat("ghu_abc123");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 hex = 64 chars
        // Different PAT produces different hash
        assert_ne!(hash1, hash_pat("ghu_different"));
    }

    #[test]
    fn token_expired_check() {
        let expired = CopilotToken {
            token: "test".to_string(),
            pat_hash: hash_pat("test_pat"),
            expires_at_ms: 0,
            updated_at_ms: 0,
        };
        assert!(expired.is_expired());

        let future = CopilotToken {
            token: "test".to_string(),
            pat_hash: hash_pat("test_pat"),
            expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
            updated_at_ms: 0,
        };
        assert!(!future.is_expired());
    }
}
