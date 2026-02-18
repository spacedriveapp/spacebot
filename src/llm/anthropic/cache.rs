//! Cache control for Anthropic API requests.

/// Cache retention policy for Anthropic prompt caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRetention {
    /// No cache_control attached to blocks.
    None,
    /// Ephemeral caching (default).
    Short,
    /// Extended caching with optional TTL on api.anthropic.com.
    Long,
}

/// Resolve cache retention from the environment, with a caller-provided override.
///
/// Precedence: explicit override > `PI_CACHE_RETENTION` env var > Short.
pub fn resolve_cache_retention(override_value: Option<CacheRetention>) -> CacheRetention {
    if let Some(value) = override_value {
        return value;
    }
    if let Ok(env_val) = std::env::var("PI_CACHE_RETENTION") {
        if env_val.eq_ignore_ascii_case("long") {
            return CacheRetention::Long;
        }
        if env_val.eq_ignore_ascii_case("none") {
            return CacheRetention::None;
        }
    }
    CacheRetention::Short
}

/// Build a `cache_control` JSON value appropriate for the retention policy
/// and base URL. Returns `None` when retention is `None`.
pub fn get_cache_control(
    base_url: &str,
    retention: CacheRetention,
) -> Option<serde_json::Value> {
    match retention {
        CacheRetention::None => None,
        CacheRetention::Short => Some(serde_json::json!({"type": "ephemeral"})),
        CacheRetention::Long => {
            if base_url.contains("api.anthropic.com") {
                Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"}))
            } else {
                Some(serde_json::json!({"type": "ephemeral"}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_short() {
        // Rust 2024 edition requires unsafe for env mutation (not thread-safe)
        unsafe { std::env::remove_var("PI_CACHE_RETENTION") };
        assert_eq!(resolve_cache_retention(None), CacheRetention::Short);
    }

    #[test]
    fn explicit_override_wins() {
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::Long)),
            CacheRetention::Long
        );
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::None)),
            CacheRetention::None
        );
    }

    #[test]
    fn short_returns_ephemeral() {
        let cc = get_cache_control("https://api.anthropic.com/v1/messages", CacheRetention::Short);
        assert_eq!(cc, Some(serde_json::json!({"type": "ephemeral"})));
    }

    #[test]
    fn long_returns_ttl_for_anthropic() {
        let cc = get_cache_control("https://api.anthropic.com/v1/messages", CacheRetention::Long);
        assert_eq!(cc, Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"})));
    }

    #[test]
    fn long_returns_ephemeral_for_other_hosts() {
        let cc = get_cache_control("https://custom-proxy.example.com/v1/messages", CacheRetention::Long);
        assert_eq!(cc, Some(serde_json::json!({"type": "ephemeral"})));
    }

    #[test]
    fn none_returns_no_cache_control() {
        let cc = get_cache_control("https://api.anthropic.com/v1/messages", CacheRetention::None);
        assert!(cc.is_none());
    }
}
