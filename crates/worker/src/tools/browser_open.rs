//! Open URLs in Brave Browser with domain allowlisting.

use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Open approved HTTPS URLs in Brave Browser (no scraping, no DOM automation).
pub struct BrowserOpenTool {
    security: Arc<SecurityPolicy>,
    allowed_hosts: Vec<String>,
    allow_private_hosts: bool,
}

impl BrowserOpenTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_hosts: Vec<String>,
        allow_private_hosts: bool,
    ) -> Self {
        Self {
            security,
            allowed_hosts: normalize_allowed_hosts(allowed_hosts),
            allow_private_hosts,
        }
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        let url = raw_url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !url.starts_with("https://") {
            anyhow::bail!("Only https:// URLs are allowed");
        }

        if self.allowed_hosts.is_empty() {
            anyhow::bail!(
                "Browser tool is enabled but no allowed_hosts are configured. Add [browser].allowed_hosts in config.toml"
            );
        }

        let target = extract_host(url)?;
        let host = target.host.as_str();

        let private_or_local = is_private_or_local_host(host);
        if private_or_local && !self.allow_private_hosts {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        let matches_allowed_host = host_matches_allowlist(&target, &self.allowed_hosts, true);
        let matches_explicit_allowed_host =
            host_matches_allowlist(&target, &self.allowed_hosts, false);

        if private_or_local && !matches_explicit_allowed_host {
            anyhow::bail!(
                "Blocked local/private host: {host}. Private hosts must be explicitly listed in browser.allowed_hosts"
            );
        }

        if !matches_allowed_host {
            anyhow::bail!("Host '{host}' is not in browser.allowed_hosts");
        }

        Ok(url.to_string())
    }
}

#[async_trait]
impl Tool for BrowserOpenTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn name(&self) -> &str {
        "browser_open"
    }

    fn description(&self) -> &str {
        "Open an approved HTTPS URL in Brave Browser. Security constraints: allowlist-only hosts, no local/private hosts unless explicitly enabled and allowlisted, no scraping."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTPS URL to open in Brave Browser"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' parameter"))?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        match open_in_brave(&url).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Opened in Brave: {url}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to open Brave Browser: {e}")),
            }),
        }
    }
}

async fn open_in_brave(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        for app in ["Brave Browser", "Brave"] {
            let status = tokio::process::Command::new("open")
                .arg("-a")
                .arg(app)
                .arg(url)
                .status()
                .await;

            if let Ok(s) = status
                && s.success()
            {
                return Ok(());
            }
        }
        anyhow::bail!(
            "Brave Browser was not found (tried macOS app names 'Brave Browser' and 'Brave')"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let mut last_error = String::new();
        for cmd in ["brave-browser", "brave"] {
            match tokio::process::Command::new(cmd).arg(url).status().await {
                Ok(status) if status.success() => return Ok(()),
                Ok(status) => {
                    last_error = format!("{cmd} exited with status {status}");
                }
                Err(e) => {
                    last_error = format!("{cmd} not runnable: {e}");
                }
            }
        }
        anyhow::bail!("{last_error}");
    }

    #[cfg(target_os = "windows")]
    {
        let status = tokio::process::Command::new("cmd")
            .args(["/C", "start", "", "brave", url])
            .status()
            .await?;

        if status.success() {
            return Ok(());
        }

        anyhow::bail!("cmd start brave exited with status {status}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        anyhow::bail!("browser_open is not supported on this OS");
    }
}

fn normalize_allowed_hosts(hosts: Vec<String>) -> Vec<String> {
    let mut normalized = hosts
        .into_iter()
        .filter_map(|d| normalize_host_rule(&d))
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

fn normalize_host_rule(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    if d.is_empty() {
        return None;
    }

    if d == "*" {
        return Some(d);
    }

    if let Some(stripped) = d.strip_prefix("https://") {
        d = stripped.to_string();
    } else if let Some(stripped) = d.strip_prefix("http://") {
        d = stripped.to_string();
    }

    if let Some((host, _)) = d.split_once('/') {
        d = host.to_string();
    }

    d = d.trim_start_matches('.').trim_end_matches('.').to_string();

    if d.is_empty() || d.chars().any(char::is_whitespace) {
        return None;
    }

    Some(d)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetHost {
    host: String,
    port: Option<u16>,
}

fn extract_host(url: &str) -> anyhow::Result<TargetHost> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("Only https:// URLs are allowed"))?;

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL"))?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported in browser_open");
    }

    let (host, port) = split_host_port(authority)?;

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(TargetHost { host, port })
}

fn split_host_port(authority: &str) -> anyhow::Result<(String, Option<u16>)> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let parsed_port = port
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("URL port is out of range"))?;
            (host, Some(parsed_port))
        }
        _ => (authority, None),
    };

    Ok((host.trim().trim_end_matches('.').to_lowercase(), port))
}

fn host_matches_allowlist(
    target: &TargetHost,
    allowed_hosts: &[String],
    include_wildcard: bool,
) -> bool {
    allowed_hosts.iter().any(|rule| {
        if rule == "*" {
            return include_wildcard;
        }

        let Ok((rule_host, rule_port)) = split_host_port(rule) else {
            return false;
        };

        if rule_port.is_some() && rule_port != target.port {
            return false;
        }

        target.host == rule_host
            || target
                .host
                .strip_suffix(&rule_host)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn is_private_or_local_host(host: &str) -> bool {
    let has_local_tld = host
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if host == "localhost" || host.ends_with(".localhost") || has_local_tld || host == "::1" {
        return true;
    }

    if let Some([a, b, _, _]) = parse_ipv4(host) {
        return a == 0
            || a == 10
            || a == 127
            || (a == 169 && b == 254)
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 100 && (64..=127).contains(&b));
    }

    false
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }

    let mut octets = [0_u8; 4];
    for (i, part) in parts.iter().enumerate() {
        octets[i] = part.parse::<u8>().ok()?;
    }
    Some(octets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(allowed_hosts: Vec<&str>) -> BrowserOpenTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        BrowserOpenTool::new(
            security,
            allowed_hosts.into_iter().map(String::from).collect(),
            false,
        )
    }

    #[test]
    fn normalize_host_strips_scheme_path_and_case() {
        let got = normalize_host_rule("  HTTPS://Docs.Example.com/path ").unwrap();
        assert_eq!(got, "docs.example.com");
    }

    #[test]
    fn normalize_allowed_hosts_deduplicates() {
        let got = normalize_allowed_hosts(vec![
            "example.com".into(),
            "EXAMPLE.COM".into(),
            "https://example.com/".into(),
        ]);
        assert_eq!(got, vec!["example.com".to_string()]);
    }

    #[test]
    fn normalize_allowed_hosts_preserves_ports() {
        let got = normalize_allowed_hosts(vec!["https://Localhost:3000/path".into()]);
        assert_eq!(got, vec!["localhost:3000".to_string()]);
    }

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/docs").unwrap();
        assert_eq!(got, "https://example.com/docs");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_honors_allowed_host_ports() {
        let tool = test_tool(vec!["example.com:8443"]);
        assert!(tool.validate_url("https://example.com:8443/v1").is_ok());
        assert!(tool.validate_url("https://example.com:443/v1").is_err());
    }

    #[test]
    fn validate_rejects_http() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("http://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("https://"));
    }

    #[test]
    fn validate_rejects_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn private_hosts_require_global_setting_and_explicit_host() {
        let security = Arc::new(SecurityPolicy::default());
        let wildcard = BrowserOpenTool::new(security.clone(), vec!["*".into()], true);
        let err = wildcard
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("explicitly listed"));

        let explicit = BrowserOpenTool::new(security, vec!["localhost:8080".into()], true);
        assert!(explicit.validate_url("https://localhost:8080").is_ok());
        assert!(explicit.validate_url("https://localhost:3000").is_err());
    }

    #[test]
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_hosts"));
    }

    #[test]
    fn validate_rejects_whitespace() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn validate_rejects_userinfo() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://user@example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserOpenTool::new(security, vec![], false);
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_hosts"));
    }

    #[test]
    fn parse_ipv4_valid() {
        assert_eq!(parse_ipv4("1.2.3.4"), Some([1, 2, 3, 4]));
    }

    #[test]
    fn parse_ipv4_invalid() {
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1.2.3.999"), None);
        assert_eq!(parse_ipv4("not-an-ip"), None);
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()], false);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_when_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()], false);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }
}
