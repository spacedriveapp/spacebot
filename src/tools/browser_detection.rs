//! Pure detection heuristics for captcha / login wall / WAF block pages.
//!
//! Runs against post-navigation page state (HTML + final URL). DOM-only for
//! v1 — chromiumoxide response-header inspection is a separate workstream.
//! Detection is conservative: false positives on legitimate Cloudflare-hosted
//! pages would short-circuit valid worker runs, so each heuristic requires a
//! specific signal rather than a generic vendor marker.

use crate::agent::worker::{BlockEvidence, BlockReason};

/// Result of running detection over a page snapshot.
#[derive(Debug, Clone)]
pub struct Detection {
    pub reason: BlockReason,
    pub evidence: BlockEvidence,
}

/// Inspect the page snapshot and return a detection if any heuristic fires.
/// Caller passes the raw page HTML (full content), the final URL after any
/// redirects, and optionally the originally-requested URL so the login-wall
/// detector can distinguish a deliberate sign-in navigation from an
/// unexpected redirect into a login page.
pub fn classify(
    html: &str,
    final_url: Option<&str>,
    requested_url: Option<&str>,
) -> Option<Detection> {
    if let Some(reason) = detect_captcha(html) {
        return Some(Detection {
            reason,
            evidence: build_evidence(html, final_url),
        });
    }
    if let Some(reason) = detect_cloudflare_challenge(html) {
        return Some(Detection {
            reason,
            evidence: build_evidence(html, final_url),
        });
    }
    if let Some(reason) = detect_login_wall(final_url, requested_url) {
        return Some(Detection {
            reason,
            evidence: build_evidence(html, final_url),
        });
    }
    None
}

/// Detect a captcha challenge by scanning for known provider iframe srcs and
/// form-input markers. Order matters — provider-specific markers come first
/// so the `provider` field in `BlockReason::Captcha` is precise.
pub fn detect_captcha(html: &str) -> Option<BlockReason> {
    if html.contains("challenges.cloudflare.com/turnstile/") {
        return Some(BlockReason::Captcha {
            provider: "cloudflare-turnstile".to_string(),
        });
    }
    if html.contains("hcaptcha.com/captcha/") || html.contains("js.hcaptcha.com/1/api.js") {
        return Some(BlockReason::Captcha {
            provider: "hcaptcha".to_string(),
        });
    }
    if html.contains("google.com/recaptcha/")
        || html.contains("gstatic.com/recaptcha/")
        || html.contains("g-recaptcha-response")
    {
        return Some(BlockReason::Captcha {
            provider: "recaptcha".to_string(),
        });
    }
    None
}

/// Detect a Cloudflare interstitial challenge page (the "checking your
/// browser before accessing" interstitial, distinct from sites merely fronted
/// by Cloudflare). Anchored on body text + a CF-specific cookie/header marker
/// to avoid firing on every CF-hosted site.
pub fn detect_cloudflare_challenge(html: &str) -> Option<BlockReason> {
    let interstitial_markers = [
        "Checking your browser before accessing",
        "challenge-platform",
        "cf-mitigated",
        "cf_chl_opt",
    ];
    let mut matched = 0;
    for marker in interstitial_markers.iter() {
        if html.contains(marker) {
            matched += 1;
        }
    }
    if matched >= 2 {
        return Some(BlockReason::FraudDetect {
            vendor: "cloudflare".to_string(),
        });
    }
    None
}

/// Detect that the navigation landed on a login URL distinct from where the
/// caller meant to go. Conservative on purpose — a worker that intentionally
/// navigates to `/login` or clicks a "Sign in" button would otherwise be
/// classified as blocked, breaking normal auth flows.
///
/// Heuristic: only fire when the *final* path matches a login signal AND
/// the caller's *requested* path does not. If the requested URL was itself
/// a login page, treat the navigation as intentional. If no requested URL
/// is supplied (e.g. follow-up click without context), we err toward not
/// firing — the cost of a false positive (worker dies mid-task) is higher
/// than the cost of a false negative (LLM sees the login page and decides
/// what to do).
pub fn detect_login_wall(
    final_url: Option<&str>,
    requested_url: Option<&str>,
) -> Option<BlockReason> {
    let final_parsed = url::Url::parse(final_url?).ok()?;
    let final_path = final_parsed.path().to_lowercase();
    if !path_signals_login(&final_path) {
        return None;
    }

    // Need a requested URL to distinguish unexpected redirect from
    // intentional login navigation.
    let requested = requested_url?;
    let requested_parsed = url::Url::parse(requested).ok()?;
    let requested_host = requested_parsed.host_str().unwrap_or("");
    let final_host = final_parsed.host_str().unwrap_or("");
    if !final_host.eq_ignore_ascii_case(requested_host) {
        // Cross-host redirect to login (e.g. SSO bounce) — not an
        // unexpected block, just a normal auth handoff.
        return None;
    }

    let requested_path = requested_parsed.path().to_lowercase();
    if path_signals_login(&requested_path) {
        // Caller asked to go to a login page — they meant it.
        return None;
    }

    Some(BlockReason::LoginWall)
}

fn path_signals_login(path: &str) -> bool {
    const LOGIN_PATH_SIGNALS: [&str; 5] = [
        "/login",
        "/signin",
        "/auth/",
        "/account/login",
        "/users/sign_in",
    ];
    LOGIN_PATH_SIGNALS
        .iter()
        .any(|signal| path.contains(signal))
}

/// Build the captured evidence payload (truncated HTML + URL).
fn build_evidence(html: &str, final_url: Option<&str>) -> BlockEvidence {
    const MAX_SNIPPET: usize = 4096;
    let snippet = if html.len() > MAX_SNIPPET {
        // Truncate at a char boundary to avoid splitting a UTF-8 sequence.
        let mut cut = MAX_SNIPPET;
        while cut > 0 && !html.is_char_boundary(cut) {
            cut -= 1;
        }
        Some(html[..cut].to_string())
    } else {
        Some(html.to_string())
    };
    BlockEvidence {
        final_url: final_url.map(String::from),
        html_snippet: snippet,
        status: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudflare_turnstile_iframe_detected() {
        let html =
            r#"<iframe src="https://challenges.cloudflare.com/turnstile/v0/widget"></iframe>"#;
        match detect_captcha(html) {
            Some(BlockReason::Captcha { provider }) => {
                assert_eq!(provider, "cloudflare-turnstile");
            }
            other => panic!("expected cloudflare-turnstile, got {other:?}"),
        }
    }

    #[test]
    fn hcaptcha_detected() {
        let html = r#"<script src="https://js.hcaptcha.com/1/api.js"></script>"#;
        match detect_captcha(html) {
            Some(BlockReason::Captcha { provider }) => assert_eq!(provider, "hcaptcha"),
            other => panic!("expected hcaptcha, got {other:?}"),
        }
    }

    #[test]
    fn recaptcha_detected() {
        let html = r#"<input type="hidden" name="g-recaptcha-response">"#;
        match detect_captcha(html) {
            Some(BlockReason::Captcha { provider }) => assert_eq!(provider, "recaptcha"),
            other => panic!("expected recaptcha, got {other:?}"),
        }
    }

    #[test]
    fn vanilla_page_not_classified_as_captcha() {
        let html = "<html><body><h1>Welcome</h1><p>Just a normal page.</p></body></html>";
        assert!(detect_captcha(html).is_none());
    }

    #[test]
    fn cloudflare_hosted_site_does_not_trigger_challenge_detection() {
        // Pages merely served via Cloudflare often include `cf-mitigated`-style
        // markers in headers but not in body. A single body marker should not
        // fire the challenge-page detector.
        let html = "<html><body>cf-mitigated: a-real-page</body></html>";
        assert!(detect_cloudflare_challenge(html).is_none());
    }

    #[test]
    fn cloudflare_interstitial_with_two_markers_detected() {
        let html = r#"<html><head><title>Just a moment...</title></head>
            <body>
            <h1>Checking your browser before accessing</h1>
            <script src="/cdn-cgi/challenge-platform/x/y/z.js"></script>
            </body></html>"#;
        match detect_cloudflare_challenge(html) {
            Some(BlockReason::FraudDetect { vendor }) => assert_eq!(vendor, "cloudflare"),
            other => panic!("expected cloudflare fraud-detect, got {other:?}"),
        }
    }

    #[test]
    fn unexpected_redirect_to_login_detected() {
        let reason = detect_login_wall(
            Some("https://example.com/login?return_to=/dashboard"),
            Some("https://example.com/dashboard"),
        );
        assert!(matches!(reason, Some(BlockReason::LoginWall)));
    }

    #[test]
    fn intentional_login_navigation_not_classified() {
        // Caller asked to go to /login. This is a deliberate sign-in flow,
        // not a block.
        let reason = detect_login_wall(
            Some("https://example.com/login"),
            Some("https://example.com/login"),
        );
        assert!(reason.is_none());
    }

    #[test]
    fn cross_host_sso_redirect_not_classified() {
        // SSO bounce is a normal auth handoff, not a block.
        let reason = detect_login_wall(
            Some("https://identity-provider.com/login"),
            Some("https://example.com/dashboard"),
        );
        assert!(reason.is_none());
    }

    #[test]
    fn dashboard_url_not_login_wall() {
        let reason = detect_login_wall(
            Some("https://example.com/dashboard"),
            Some("https://example.com/dashboard"),
        );
        assert!(reason.is_none());
    }

    #[test]
    fn no_requested_url_treats_as_intentional() {
        // Without context, err toward not firing — false positives kill
        // valid worker runs; false negatives just let the LLM see a login
        // page and decide.
        let reason = detect_login_wall(Some("https://example.com/account/login"), None);
        assert!(reason.is_none());
    }

    #[test]
    fn evidence_truncates_long_html_at_char_boundary() {
        let html = "x".repeat(8192);
        let detection = classify(
            &format!(
                r#"<iframe src="https://challenges.cloudflare.com/turnstile/v0/widget"></iframe>{html}"#
            ),
            Some("https://example.com"),
            None,
        );
        let evidence = detection.expect("captcha should fire").evidence;
        let snippet = evidence.html_snippet.expect("snippet present");
        assert!(snippet.len() <= 4096);
    }
}
