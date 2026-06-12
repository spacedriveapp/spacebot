//! Output scrubbing and leak detection for streaming content.
//!
//! Two complementary layers:
//! 1. **`StreamScrubber`** — exact-match redaction of known secret values.
//!    Proactive: replaces tool secret values with `[REDACTED:<name>]` before
//!    content leaves the process.
//! 2. **`scan_for_leaks()`** — regex-based leak detection for unknown secrets.
//!    Reactive: catches secrets not in the store by matching known API key formats.
//!
//! Sequencing: scrubbing runs first, then leak detection on the scrubbed output.
//! This ensures stored tool secrets don't trigger leak detection (they're already
//! redacted), and leak detection only fires on unknown/unstored secrets.

use regex::Regex;
use std::sync::LazyLock;

/// Regex patterns for known API key formats. Used by `scan_for_leaks()` to
/// detect secrets that aren't in the store.
static LEAK_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"sk-[a-zA-Z0-9]{20,}").expect("hardcoded regex"),
        Regex::new(r"sk-ant-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
        Regex::new(r"sk-or-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
        Regex::new(r"-----BEGIN.*PRIVATE KEY-----").expect("hardcoded regex"),
        Regex::new(r"ghp_[a-zA-Z0-9]{36}").expect("hardcoded regex"),
        Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("hardcoded regex"),
        Regex::new(r"[MN][A-Za-z0-9]{23,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,}")
            .expect("hardcoded regex"),
        Regex::new(r"xoxb-[0-9]{10,}-[0-9A-Za-z-]+").expect("hardcoded regex"),
        Regex::new(r"xapp-[0-9]-[A-Z0-9]+-[0-9]+-[a-f0-9]+").expect("hardcoded regex"),
        Regex::new(r"\d{8,}:[A-Za-z0-9_-]{35}").expect("hardcoded regex"),
        Regex::new(r"BSA[a-zA-Z0-9]{20,}").expect("hardcoded regex"),
    ]
});

/// Full PEM block pattern (header + base64 body + footer). Used by `scrub_leaks()`
/// to redact entire private key blocks, not just the header line matched by
/// `LEAK_PATTERNS`.
static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN[A-Z \r\n]*PRIVATE KEY-----.*?-----END[A-Z \r\n]*PRIVATE KEY-----")
        .expect("hardcoded regex")
});

static BASE64_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/]{24,}={0,2}").expect("hardcoded regex"));

static HEX_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:0x)?([0-9a-f]{40,})").expect("hardcoded regex"));

/// Check content against known API key patterns (plaintext only).
pub fn match_leak_patterns(content: &str) -> Option<String> {
    for pattern in LEAK_PATTERNS.iter() {
        if let Some(matched) = pattern.find(content) {
            return Some(matched.as_str().to_string());
        }
    }
    None
}

/// Scan content for potential secret leaks, including encoded forms.
///
/// Checks raw content first, then attempts URL-decoding, base64-decoding,
/// and hex-decoding to catch secrets that an LLM might encode to evade
/// plaintext pattern matching.
pub fn scan_for_leaks(content: &str) -> Option<String> {
    use base64::Engine;

    if let Some(matched) = match_leak_patterns(content) {
        return Some(matched);
    }

    // Percent-encoded secrets (e.g. sk%2Dant%2D...)
    let url_decoded = urlencoding::decode(content).unwrap_or(std::borrow::Cow::Borrowed(""));
    if url_decoded != content
        && let Some(matched) = match_leak_patterns(&url_decoded)
    {
        return Some(format!("url-encoded: {matched}"));
    }

    // Base64-wrapped secrets
    for segment in BASE64_SEGMENT.find_iter(content) {
        if let Ok(decoded_bytes) =
            base64::engine::general_purpose::STANDARD.decode(segment.as_str())
            && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
            && let Some(matched) = match_leak_patterns(decoded)
        {
            return Some(format!("base64-encoded: {matched}"));
        }
        if let Ok(decoded_bytes) =
            base64::engine::general_purpose::URL_SAFE.decode(segment.as_str())
            && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
            && let Some(matched) = match_leak_patterns(decoded)
        {
            return Some(format!("base64-encoded: {matched}"));
        }
    }

    // Hex-encoded secrets
    for caps in HEX_SEGMENT.captures_iter(content) {
        let hex_str = caps.get(1).map_or("", |m| m.as_str());
        if let Ok(decoded_bytes) = hex::decode(hex_str)
            && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
            && let Some(matched) = match_leak_patterns(decoded)
        {
            return Some(format!("hex-encoded: {matched}"));
        }
    }

    None
}

/// Exact-match stream scrubber for redacting known secret values in chunked output.
///
/// Handles the case where a secret value is split across SSE chunk boundaries
/// by maintaining a rolling buffer of the last N characters (where N is the
/// length of the longest secret value being scrubbed).
pub struct StreamScrubber {
    /// Secret name → value pairs for exact-match redaction.
    secrets: Vec<(String, String)>,
    /// Rolling tail buffer to catch secrets split across chunk boundaries.
    /// Contains the last `max_secret_len` characters from the previous chunk.
    tail: String,
    /// Length of the longest secret value.
    max_secret_len: usize,
}

impl StreamScrubber {
    /// Create a new scrubber with the given secret name/value pairs.
    pub fn new(secrets: Vec<(String, String)>) -> Self {
        let max_secret_len = secrets
            .iter()
            .map(|(_, value)| value.len())
            .max()
            .unwrap_or(0);
        Self {
            secrets,
            tail: String::new(),
            max_secret_len,
        }
    }

    /// Scrub a chunk of text, returning the sanitized output.
    ///
    /// Holds back the last `max_secret_len` characters of each chunk to handle
    /// secrets that span chunk boundaries. The held-back portion is prepended
    /// to the next chunk before scanning. Call `flush()` when the stream ends
    /// to emit any remaining buffered content.
    pub fn scrub(&mut self, chunk: &str) -> String {
        if self.secrets.is_empty() {
            return chunk.to_string();
        }

        // Combine the held-back tail from the previous chunk with the new chunk.
        let combined = format!("{}{}", self.tail, chunk);

        // Apply exact-match redaction on the combined text.
        let mut scrubbed = combined;
        for (name, value) in &self.secrets {
            if !value.is_empty() {
                scrubbed = scrubbed.replace(value, &format!("[REDACTED:{name}]"));
            }
        }

        // Hold back the last max_secret_len chars to catch secrets that might
        // be split at the next boundary. Emit everything before that.
        if self.max_secret_len > 0 && scrubbed.len() > self.max_secret_len {
            let mut split_at = scrubbed.len() - self.max_secret_len;
            while split_at > 0 && !scrubbed.is_char_boundary(split_at) {
                split_at -= 1;
            }
            self.tail = scrubbed[split_at..].to_string();
            scrubbed[..split_at].to_string()
        } else {
            // The entire scrubbed text fits within the hold-back window.
            // Buffer it all, emit nothing yet.
            self.tail = scrubbed;
            String::new()
        }
    }

    /// Flush any remaining buffered content. Call this when the stream ends.
    ///
    /// Applies one final round of scrubbing on the remaining buffer.
    pub fn flush(&mut self) -> String {
        let tail = std::mem::take(&mut self.tail);
        if tail.is_empty() {
            return String::new();
        }

        // Final scrub pass on the remaining buffer.
        let mut result = tail;
        for (name, value) in &self.secrets {
            if !value.is_empty() {
                result = result.replace(value, &format!("[REDACTED:{name}]"));
            }
        }
        result
    }
}

/// Scrub all tool secret values from a text string in one pass.
///
/// Convenience wrapper over `StreamScrubber` for non-streaming content (worker
/// results, branch conclusions, status text). Replaces each tool secret value
/// with `[REDACTED:<name>]`.
pub fn scrub_secrets(text: &str, tool_secrets: &[(String, String)]) -> String {
    if tool_secrets.is_empty() {
        return text.to_string();
    }
    // Sort by descending value length so longer secrets are replaced first.
    // This prevents partial replacement when one secret value is a prefix of another.
    let mut sorted: Vec<&(String, String)> = tool_secrets.iter().collect();
    sorted.sort_by_key(|secret| std::cmp::Reverse(secret.1.len()));
    let mut result = text.to_string();
    for (name, value) in sorted {
        if !value.is_empty() {
            result = result.replace(value.as_str(), &format!("[REDACTED:{name}]"));
        }
    }
    result
}

/// Scrub tool secret values from text using a `SecretsStore`.
///
/// Reads the tool secrets visible to `agent_id` (the union of
/// `InstanceShared(Tool)` and the agent's own scope) and performs
/// exact-match redaction. Scoped per agent so a worker's output is only
/// scrubbed against the secret values that worker could plausibly have
/// seen — avoids incidentally redacting another tenant's secret value
/// that happened to appear verbatim in the text.
pub fn scrub_with_store(
    text: &str,
    store: &crate::secrets::store::SecretsStore,
    agent_id: &crate::AgentId,
) -> String {
    let pairs = store.tool_secret_pairs(agent_id);
    scrub_secrets(text, &pairs)
}

/// Replace all detected leak patterns in `content` with a redaction marker.
///
/// Unlike `scan_for_leaks()` which returns the first match, this function
/// replaces **all** plaintext matches of known API key patterns with
/// `[LEAKED_SECRET_REDACTED]`. Used on egress paths (worker results, branch
/// conclusions, status updates) to sanitize content that may contain secrets
/// the LLM encountered during tool execution.
///
/// Does NOT check encoded forms (base64, hex, URL-encoded) — those are
/// unlikely to appear in LLM-generated output text.
pub fn scrub_leaks(content: &str) -> String {
    // First, redact full PEM blocks (header + body + footer) so the base64
    // key material is removed, not just the header line.
    let mut result = PEM_BLOCK
        .replace_all(content, "[LEAKED_SECRET_REDACTED]")
        .into_owned();
    for pattern in LEAK_PATTERNS.iter() {
        result = pattern
            .replace_all(&result, "[LEAKED_SECRET_REDACTED]")
            .into_owned();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubber_redacts_exact_match() {
        let mut scrubber =
            StreamScrubber::new(vec![("API_KEY".to_string(), "secret123".to_string())]);
        let part = scrubber.scrub("the key is secret123 here");
        let rest = scrubber.flush();
        let result = format!("{part}{rest}");
        assert_eq!(result, "the key is [REDACTED:API_KEY] here");
    }

    #[test]
    fn scrubber_handles_split_across_chunks() {
        let mut scrubber = StreamScrubber::new(vec![("TOKEN".to_string(), "abcdef".to_string())]);
        let part1 = scrubber.scrub("prefix abc");
        let part2 = scrubber.scrub("def suffix");
        let rest = scrubber.flush();
        let combined = format!("{part1}{part2}{rest}");
        assert!(
            combined.contains("[REDACTED:TOKEN]"),
            "expected redaction in: {combined}"
        );
        assert!(
            !combined.contains("abcdef"),
            "secret should not appear in: {combined}"
        );
    }

    #[test]
    fn scrubber_no_secrets_passes_through() {
        let mut scrubber = StreamScrubber::new(vec![]);
        assert_eq!(scrubber.scrub("hello world"), "hello world");
    }

    #[test]
    fn scrubber_split_respects_utf8_boundaries() {
        let mut scrubber = StreamScrubber::new(vec![("DUMMY".to_string(), "abc".to_string())]);
        let part = scrubber.scrub("ééééé");
        let rest = scrubber.flush();
        assert_eq!(format!("{part}{rest}"), "ééééé");
    }

    #[test]
    fn leak_detection_catches_anthropic_key() {
        let result = scan_for_leaks("key is sk-ant-abc123456789012345678");
        assert!(result.is_some());
    }

    #[test]
    fn leak_detection_catches_github_token() {
        let result = scan_for_leaks("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(result.is_some());
    }

    #[test]
    fn leak_detection_ignores_normal_text() {
        let result = scan_for_leaks("this is normal output with no secrets");
        assert!(result.is_none());
    }

    #[test]
    fn scrub_leaks_redacts_anthropic_key() {
        let input = "found key sk-ant-abc123456789012345678 in config";
        let result = scrub_leaks(input);
        assert!(
            !result.contains("sk-ant-abc123456789012345678"),
            "secret should be redacted in: {result}"
        );
        assert!(
            result.contains("[LEAKED_SECRET_REDACTED]"),
            "redaction marker missing in: {result}"
        );
        assert!(
            result.contains("found key"),
            "surrounding text should be preserved in: {result}"
        );
    }

    #[test]
    fn scrub_leaks_redacts_multiple_secrets() {
        let input =
            "keys: sk-ant-abc123456789012345678 and ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let result = scrub_leaks(input);
        assert!(
            !result.contains("sk-ant-"),
            "first secret should be redacted in: {result}"
        );
        assert!(
            !result.contains("ghp_"),
            "second secret should be redacted in: {result}"
        );
    }

    #[test]
    fn scrub_leaks_passes_through_clean_text() {
        let input = "this is normal output with no secrets";
        assert_eq!(scrub_leaks(input), input);
    }

    #[test]
    fn scrub_leaks_redacts_full_pem_block() {
        let input = "config:\n-----BEGIN RSA PRIVATE KEY-----\n\
                      MIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHB7MhgHcTz6sE2I2yPB\n\
                      aFDrBz9vFqU4zK3L3hUfVnEy\n\
                      -----END RSA PRIVATE KEY-----\nmore text";
        let result = scrub_leaks(input);
        assert!(
            !result.contains("MIIEpAIBAAK"),
            "PEM body should be redacted in: {result}"
        );
        assert!(
            !result.contains("BEGIN RSA PRIVATE KEY"),
            "PEM header should be redacted in: {result}"
        );
        assert!(
            result.contains("[LEAKED_SECRET_REDACTED]"),
            "redaction marker missing in: {result}"
        );
        assert!(
            result.contains("more text"),
            "surrounding text should be preserved in: {result}"
        );
    }
}
