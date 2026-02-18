//! Auth path detection and header construction for Anthropic API requests.

use reqwest::RequestBuilder;

const BETA_FINE_GRAINED_STREAMING: &str = "fine-grained-tool-streaming-2025-05-14";
const BETA_INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
const BETA_CLAUDE_CODE: &str = "claude-code-20250219";
const BETA_OAUTH: &str = "oauth-2025-04-20";
const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.2 (external, cli)";

/// Which authentication path to use for an Anthropic API call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicAuthPath {
    /// Standard API key (sk-ant-api*) — uses x-api-key header.
    ApiKey,
    /// OAuth token (sk-ant-oat*) — uses Bearer auth with Claude Code identity.
    OAuthToken,
}

/// Detect the auth path from a token's prefix.
pub fn detect_auth_path(token: &str) -> AnthropicAuthPath {
    if token.starts_with("sk-ant-oat") {
        AnthropicAuthPath::OAuthToken
    } else {
        AnthropicAuthPath::ApiKey
    }
}

/// Apply authentication headers and beta headers to a request builder.
///
/// Returns the augmented builder and the detected auth path (so callers
/// know whether tool name normalization and identity injection apply).
pub fn apply_auth_headers(
    builder: RequestBuilder,
    token: &str,
    interleaved_thinking: bool,
) -> (RequestBuilder, AnthropicAuthPath) {
    let auth_path = detect_auth_path(token);

    let mut beta_parts: Vec<&str> = Vec::new();
    let builder = match auth_path {
        AnthropicAuthPath::ApiKey => {
            beta_parts.push(BETA_FINE_GRAINED_STREAMING);
            builder.header("x-api-key", token)
        }
        AnthropicAuthPath::OAuthToken => {
            beta_parts.push(BETA_CLAUDE_CODE);
            beta_parts.push(BETA_OAUTH);
            beta_parts.push(BETA_FINE_GRAINED_STREAMING);
            builder
                .header("Authorization", format!("Bearer {token}"))
                .header("user-agent", CLAUDE_CODE_USER_AGENT)
                .header("x-app", "cli")
        }
    };

    if interleaved_thinking {
        beta_parts.push(BETA_INTERLEAVED_THINKING);
    }

    let beta_header = beta_parts.join(",");
    let builder = builder.header("anthropic-beta", beta_header);

    (builder, auth_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_request(token: &str, thinking: bool) -> (reqwest::Request, AnthropicAuthPath) {
        let client = reqwest::Client::new();
        let builder = client.post("https://api.anthropic.com/v1/messages");
        let (builder, auth_path) = apply_auth_headers(builder, token, thinking);
        (builder.build().unwrap(), auth_path)
    }

    #[test]
    fn oauth_token_detected_correctly() {
        assert_eq!(detect_auth_path("sk-ant-oat01-abc123"), AnthropicAuthPath::OAuthToken);
    }

    #[test]
    fn api_key_detected_correctly() {
        assert_eq!(detect_auth_path("sk-ant-api03-xyz789"), AnthropicAuthPath::ApiKey);
    }

    #[test]
    fn unknown_prefix_defaults_to_api_key() {
        assert_eq!(detect_auth_path("some-random-key"), AnthropicAuthPath::ApiKey);
    }

    #[test]
    fn oauth_token_uses_bearer_header() {
        let (request, auth_path) = build_request("sk-ant-oat01-abc123", false);
        assert_eq!(auth_path, AnthropicAuthPath::OAuthToken);
        assert_eq!(
            request.headers().get("Authorization").unwrap(),
            "Bearer sk-ant-oat01-abc123"
        );
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn oauth_token_includes_identity_headers() {
        let (request, _) = build_request("sk-ant-oat01-abc123", false);
        assert_eq!(
            request.headers().get("user-agent").unwrap(),
            CLAUDE_CODE_USER_AGENT
        );
        assert_eq!(request.headers().get("x-app").unwrap(), "cli");
    }

    #[test]
    fn oauth_token_includes_claude_code_beta() {
        let (request, _) = build_request("sk-ant-oat01-abc123", false);
        let beta = request.headers().get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains(BETA_CLAUDE_CODE));
        assert!(beta.contains(BETA_OAUTH));
        assert!(beta.contains(BETA_FINE_GRAINED_STREAMING));
    }

    #[test]
    fn api_key_uses_x_api_key_header() {
        let (request, auth_path) = build_request("sk-ant-api03-xyz789", false);
        assert_eq!(auth_path, AnthropicAuthPath::ApiKey);
        assert_eq!(
            request.headers().get("x-api-key").unwrap(),
            "sk-ant-api03-xyz789"
        );
        assert!(request.headers().get("Authorization").is_none());
    }

    #[test]
    fn api_key_has_no_identity_headers() {
        let (request, _) = build_request("sk-ant-api03-xyz789", false);
        assert!(request.headers().get("x-app").is_none());
    }

    #[test]
    fn interleaved_thinking_appended_to_beta() {
        let (request, _) = build_request("sk-ant-api03-xyz789", true);
        let beta = request.headers().get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains(BETA_INTERLEAVED_THINKING));
    }

    #[test]
    fn no_interleaved_thinking_when_disabled() {
        let (request, _) = build_request("sk-ant-api03-xyz789", false);
        let beta = request.headers().get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(!beta.contains(BETA_INTERLEAVED_THINKING));
    }
}
