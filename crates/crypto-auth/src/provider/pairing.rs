//! Harness onboarding terminal link (server-minted pairing tokens).
//!
//! See the BOO-50 plan: while the user's browser has an outstanding onboarding
//! attempt, the platform mints a high-entropy one-time token and returns it in
//! the enrollment registration response. The harness prints it as a dashboard
//! link; the token travels in the URL fragment (`#pair=`) so it is never sent
//! to the server in navigation requests, referrer headers, or access logs.
//!
//! The token is never logged and never persisted to enrollment state.

/// Environment variable overriding the dashboard origin used for the pairing
/// link. When unset, the origin is derived from the platform API origin.
pub const DASHBOARD_ORIGIN_ENV: &str = "NENJO_DASHBOARD_URL";

/// Environment variable overriding the local development dashboard port used
/// when the API origin is on localhost.
pub const DASHBOARD_LOCAL_PORT_ENV: &str = "NENJO_DASHBOARD_PORT";

/// Default local development dashboard port.
pub const DASHBOARD_LOCAL_PORT: u16 = 3000;

/// Derive the dashboard origin from the platform API origin.
///
/// - `http(s)://localhost[:port]` → `http://localhost:<dashboard port>`
///   (default 3000, configurable via `NENJO_DASHBOARD_PORT`).
/// - `https://api.nenjo.ai` → `https://nenjo.ai`.
/// - Anything else → the API origin itself (self-hosted single-origin
///   deployments).
///
/// An explicit `NENJO_DASHBOARD_URL` always wins.
pub fn dashboard_origin(api_base_url: &str) -> String {
    let explicit = std::env::var(DASHBOARD_ORIGIN_ENV)
        .ok()
        .map(|origin| origin.trim().trim_end_matches('/').to_string())
        .filter(|origin| !origin.is_empty());
    dashboard_origin_with_override(explicit.as_deref(), api_base_url)
}

fn dashboard_origin_with_override(explicit: Option<&str>, api_base_url: &str) -> String {
    if let Some(origin) = explicit {
        return origin.to_string();
    }

    let trimmed = api_base_url.trim_end_matches('/');
    let Ok(url) = url::Url::parse(trimmed) else {
        return trimmed.to_string();
    };
    let host = url.host_str().unwrap_or_default();

    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        let port = std::env::var(DASHBOARD_LOCAL_PORT_ENV)
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or(DASHBOARD_LOCAL_PORT);
        let scheme = url.scheme();
        return format!("{scheme}://localhost:{port}");
    }

    if host == "api.nenjo.ai" {
        return format!("{}://nenjo.ai", url.scheme());
    }

    trimmed.to_string()
}

/// Build the full terminal pairing link from a server-minted one-time token.
pub fn pairing_link(dashboard_origin: &str, token: &str) -> String {
    format!("{dashboard_origin}/dashboard/onboarding/harness#pair={token}")
}

/// Tokens already presented on stdout, so repeated enrollment registrations
/// with an unchanged token do not reprint the notice.
static LAST_PRESENTED_TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Present the pairing link on stdout, once per token.
///
/// Uses plain terminal output (not tracing) so the link stays clean and
/// clickable in the terminal. The notice states what the link is for and is
/// suppressed on repeated registrations with the same token. Both approval
/// paths are shown: the one-time link (automatic) and the verification code
/// (manual fallback).
pub fn print_pairing_notice(
    link: &str,
    verification_code: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) {
    let token = link.rsplit("#pair=").next().unwrap_or(link).to_string();
    if !should_present_token(&token) {
        return;
    }

    let rule = "\u{2500}".repeat(46);
    println!();
    println!("{rule}");
    println!("Finish setting up this harness");
    println!();
    println!("Option 1 (automatic): open this one-time link in the browser");
    println!("where you created your workspace to connect automatically:");
    println!();
    println!("  {link}");
    println!();
    println!("Option 2 (manual): enter this verification code on the");
    println!("dashboard when it asks for the harness code:");
    println!();
    println!("  Verification code: {verification_code}");
    println!();
    println!(
        "The link expires at {}. Waiting for approval…",
        expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    println!("{rule}");
}

/// Whether this token has not been presented yet; records it when true.
fn should_present_token(token: &str) -> bool {
    match LAST_PRESENTED_TOKEN.lock() {
        Ok(mut last) => {
            if last.as_deref() == Some(token) {
                false
            } else {
                *last = Some(token.to_string());
                true
            }
        }
        Err(_) => true,
    }
}

/// Redact a pairing token for diagnostics: enough to correlate, not enough to
/// replay.
#[cfg(test)]
pub fn redacted_token_hint(token: &str) -> String {
    let mut hint = token.chars().take(4).collect::<String>();
    hint.push('…');
    hint
}

/// Validate that a token looks like server-minted pairing material before
/// building a link (defensive; the platform verifies the real token).
pub fn token_is_plausible(token: &str) -> bool {
    !token.trim().is_empty()
        && token.len() >= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL};

    #[test]
    fn dashboard_origin_derives_local_dev_port() {
        assert_eq!(
            dashboard_origin("http://localhost:8080"),
            "http://localhost:3000"
        );
        assert_eq!(
            dashboard_origin("http://127.0.0.1:9000/api"),
            "http://localhost:3000"
        );
    }

    #[test]
    fn dashboard_origin_derives_production_host() {
        assert_eq!(dashboard_origin("https://api.nenjo.ai"), "https://nenjo.ai");
        assert_eq!(
            dashboard_origin("https://api.nenjo.ai/"),
            "https://nenjo.ai"
        );
    }

    #[test]
    fn dashboard_origin_keeps_unknown_hosts() {
        assert_eq!(
            dashboard_origin("https://api.acme.example.com"),
            "https://api.acme.example.com"
        );
    }

    #[test]
    fn dashboard_origin_prefers_explicit_override() {
        assert_eq!(
            dashboard_origin_with_override(
                Some("https://dash.example.com"),
                "https://api.nenjo.ai"
            ),
            "https://dash.example.com"
        );
    }

    #[test]
    fn dashboard_origin_local_port_override() {
        assert_eq!(
            dashboard_origin_with_override(None, "http://localhost:8080"),
            "http://localhost:3000"
        );
    }

    #[test]
    fn link_embeds_token_in_fragment() {
        let token = BASE64URL.encode([7_u8; 32]);
        let link = pairing_link("https://app.example.com", &token);
        assert!(link.starts_with("https://app.example.com/dashboard/onboarding/harness#pair="));
        assert!(link.ends_with(&token));
    }

    #[test]
    fn dashboard_origin_trims_trailing_slash() {
        assert_eq!(
            dashboard_origin("https://app.example.com/"),
            "https://app.example.com"
        );
    }

    #[test]
    fn token_plausibility_checks() {
        let token = BASE64URL.encode([7_u8; 32]);
        assert!(token_is_plausible(&token));
        assert!(!token_is_plausible(""));
        assert!(!token_is_plausible("short"));
        assert!(!token_is_plausible("has spaces and; punctuation!"));
    }

    #[test]
    fn presentation_is_deduped_per_token() {
        let token = BASE64URL.encode([5_u8; 32]);
        assert!(should_present_token(&token), "first presentation");
        assert!(!should_present_token(&token), "duplicate suppressed");
        let other = BASE64URL.encode([6_u8; 32]);
        assert!(should_present_token(&other), "new token presented");
    }

    #[test]
    fn redaction_never_reveals_full_token() {
        let token = BASE64URL.encode([9_u8; 32]);
        let hint = redacted_token_hint(&token);
        assert!(hint.len() < token.len());
        assert!(token.starts_with(hint.trim_end_matches('…')));
    }
}
