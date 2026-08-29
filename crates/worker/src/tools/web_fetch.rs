//! Fetch web pages and convert HTML to clean plain text for LLM consumption.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{FirecrawlConfig, WebFetchConfig, WebFetchProvider};
use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlScrapeRequest<'a> {
    url: &'a str,
    formats: [&'static str; 1],
    only_main_content: bool,
    max_age: u64,
    timeout: u64,
}

#[derive(Debug, Deserialize)]
struct FirecrawlScrapeResponse {
    success: bool,
    #[serde(default)]
    data: Option<FirecrawlScrapeData>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FirecrawlScrapeData {
    #[serde(default)]
    markdown: Option<String>,
}

/// Web fetch tool: fetches a web page and converts HTML to plain text for LLM consumption.
///
/// Unlike `http_request` (an API client returning raw responses), this tool returns
/// extracted text through either direct HTTP retrieval or a configured Firecrawl backend.
/// Direct retrieval follows redirects and converts HTML with `nanohtml2text`; Firecrawl
/// retrieval requests Markdown from `/v2/scrape`.
pub struct WebFetchTool {
    client: Result<reqwest::Client, String>,
    security: Arc<SecurityPolicy>,
    provider: WebFetchProvider,
    firecrawl: FirecrawlConfig,
    allowed_hosts: Vec<String>,
    blocked_hosts: Vec<String>,
    allow_private_hosts: bool,
    max_response_size: usize,
    timeout_secs: u64,
}

impl WebFetchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_hosts: Vec<String>,
        blocked_hosts: Vec<String>,
        allow_private_hosts: bool,
        max_response_size: usize,
        timeout_secs: u64,
    ) -> Self {
        let allowed_hosts = normalize_allowed_hosts_or_public_default(allowed_hosts);
        let blocked_hosts = normalize_allowed_hosts(blocked_hosts);
        let timeout_secs = effective_timeout_secs(timeout_secs);
        Self {
            client: build_web_fetch_client(
                WebFetchProvider::Direct,
                &allowed_hosts,
                &blocked_hosts,
                allow_private_hosts,
                timeout_secs,
            ),
            security,
            provider: WebFetchProvider::Direct,
            firecrawl: FirecrawlConfig::default(),
            allowed_hosts,
            blocked_hosts,
            allow_private_hosts,
            max_response_size,
            timeout_secs,
        }
    }

    /// Construct the tool from worker configuration.
    pub fn from_config(
        security: Arc<SecurityPolicy>,
        config: &WebFetchConfig,
        allow_private_hosts: bool,
    ) -> Self {
        let allowed_hosts = normalize_allowed_hosts_or_public_default(config.allowed_hosts.clone());
        let blocked_hosts = normalize_allowed_hosts(config.blocked_hosts.clone());
        let timeout_secs = effective_timeout_secs(config.timeout_secs);
        Self {
            client: build_web_fetch_client(
                config.provider,
                &allowed_hosts,
                &blocked_hosts,
                allow_private_hosts,
                timeout_secs,
            ),
            security,
            provider: config.provider,
            firecrawl: config.firecrawl.clone(),
            allowed_hosts,
            blocked_hosts,
            allow_private_hosts,
            max_response_size: config.max_response_size,
            timeout_secs,
        }
    }

    fn client(&self) -> anyhow::Result<&reqwest::Client> {
        self.client
            .as_ref()
            .map_err(|error| anyhow::anyhow!("Failed to build web fetch HTTP client: {error}"))
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        validate_target_url(
            raw_url,
            &self.allowed_hosts,
            &self.blocked_hosts,
            self.allow_private_hosts,
            "web_fetch",
        )
    }

    fn truncate_response(&self, text: &str) -> String {
        if text.len() > self.max_response_size {
            let mut truncated = text
                .chars()
                .take(self.max_response_size)
                .collect::<String>();
            truncated.push_str("\n\n... [Response truncated due to size limit] ...");
            truncated
        } else {
            text.to_string()
        }
    }

    async fn read_response_text_limited(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<String> {
        let mut bytes_stream = response.bytes_stream();
        let hard_cap = self.max_response_size.saturating_add(1);
        let mut bytes = Vec::new();

        while let Some(chunk_result) = bytes_stream.next().await {
            let chunk = chunk_result?;
            if append_chunk_with_cap(&mut bytes, &chunk, hard_cap) {
                break;
            }
        }

        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn firecrawl_endpoint(&self) -> anyhow::Result<String> {
        let base_url = self.firecrawl.base_url.trim().trim_end_matches('/');
        if base_url.is_empty() {
            anyhow::bail!("Firecrawl base_url cannot be empty");
        }

        let endpoint = format!("{base_url}/v2/scrape");
        let parsed = reqwest::Url::parse(&endpoint)
            .map_err(|error| anyhow::anyhow!("Invalid Firecrawl base_url: {error}"))?;
        match parsed.scheme() {
            "http" | "https" => Ok(endpoint),
            scheme => anyhow::bail!("Unsupported Firecrawl URL scheme: {scheme}"),
        }
    }

    async fn fetch_with_firecrawl(&self, url: &str) -> anyhow::Result<ToolResult> {
        let payload = FirecrawlScrapeRequest {
            url,
            formats: ["markdown"],
            only_main_content: self.firecrawl.only_main_content,
            max_age: self.firecrawl.max_age_ms,
            timeout: self.timeout_secs.saturating_mul(1_000),
        };

        let mut request = self
            .client()?
            .post(self.firecrawl_endpoint()?)
            .json(&payload);
        if let Some(api_key) = self.firecrawl.api_key.as_deref() {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                request = request.bearer_auth(api_key);
            }
        }

        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let error = response
                .text()
                .await
                .ok()
                .filter(|body| !body.trim().is_empty())
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("Unknown").to_string());
            return Ok(ToolResult::failure(format!(
                "Firecrawl HTTP {}: {error}",
                status.as_u16()
            )));
        }

        let response: FirecrawlScrapeResponse = response.json().await?;
        if !response.success {
            return Ok(ToolResult::failure(
                response
                    .error
                    .unwrap_or_else(|| "Firecrawl scrape failed".to_string()),
            ));
        }

        let markdown = response
            .data
            .and_then(|data| data.markdown)
            .filter(|markdown| !markdown.trim().is_empty());
        let Some(markdown) = markdown else {
            return Ok(ToolResult::failure(
                "Firecrawl returned no Markdown content",
            ));
        };

        Ok(ToolResult::success(self.truncate_response(&markdown)))
    }
}

fn effective_timeout_secs(timeout_secs: u64) -> u64 {
    if timeout_secs == 0 {
        tracing::warn!("fetch_web_page: timeout_secs is 0, using safe default of 30s");
        30
    } else {
        timeout_secs
    }
}

fn build_web_fetch_client(
    provider: WebFetchProvider,
    allowed_hosts: &[String],
    blocked_hosts: &[String],
    allow_private_hosts: bool,
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    let user_agent = match provider {
        WebFetchProvider::Direct => format!("Nenjo/{} (fetch_web_page)", env!("CARGO_PKG_VERSION")),
        WebFetchProvider::Firecrawl => format!(
            "Nenjo/{} (fetch_web_page/firecrawl)",
            env!("CARGO_PKG_VERSION")
        ),
    };
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(user_agent);

    let builder = match provider {
        WebFetchProvider::Direct => {
            let allowed_hosts = allowed_hosts.to_vec();
            let blocked_hosts = blocked_hosts.to_vec();
            builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 10 {
                    return attempt.error(std::io::Error::other("Too many redirects (max 10)"));
                }

                if let Err(error) = validate_target_url(
                    attempt.url().as_str(),
                    &allowed_hosts,
                    &blocked_hosts,
                    allow_private_hosts,
                    "web_fetch",
                ) {
                    return attempt.error(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Blocked redirect target: {error}"),
                    ));
                }

                attempt.follow()
            }))
        }
        WebFetchProvider::Firecrawl => builder,
    };

    builder.build().map_err(|error| error.to_string())
}

#[async_trait]
impl Tool for WebFetchTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "fetch_web_page"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return its content as clean plain text. \
         HTML pages are automatically converted to readable text. \
         JSON and plain text responses are returned as-is. \
         Only GET requests; follows redirects. \
         Security: allowlist-only hosts, no local/private hosts unless explicitly enabled and allowlisted."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch"
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
                output: String::new().into(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(e.to_string()),
                });
            }
        };

        match self.provider {
            WebFetchProvider::Firecrawl => {
                return match self.fetch_with_firecrawl(&url).await {
                    Ok(result) => Ok(result),
                    Err(error) => Ok(ToolResult::failure(format!(
                        "Firecrawl request failed: {error}"
                    ))),
                };
            }
            WebFetchProvider::Direct => {}
        }

        let client = match self.client() {
            Ok(client) => client,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(error.to_string()),
                });
            }
        };

        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(format!("HTTP request failed: {e}")),
                });
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(format!(
                    "HTTP {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                )),
            });
        }

        // Determine content type for processing strategy
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body_mode = if content_type.contains("text/html") || content_type.is_empty() {
            "html"
        } else if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            "plain"
        } else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(format!(
                    "Unsupported content type: {content_type}. \
                     fetch_web_page supports text/html, text/plain, text/markdown, and application/json."
                )),
            });
        };

        let body = match self.read_response_text_limited(response).await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(format!("Failed to read response body: {e}")),
                });
            }
        };

        let text = if body_mode == "html" {
            nanohtml2text::html2text(&body)
        } else {
            body
        };

        let output = self.truncate_response(&text);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

// ── Helper functions (independent from http_request.rs per DRY rule-of-three) ──

fn validate_target_url(
    raw_url: &str,
    allowed_hosts: &[String],
    blocked_hosts: &[String],
    allow_private_hosts: bool,
    tool_name: &str,
) -> anyhow::Result<String> {
    let url = raw_url.trim();

    if url.is_empty() {
        anyhow::bail!("URL cannot be empty");
    }

    if url.chars().any(char::is_whitespace) {
        anyhow::bail!("URL cannot contain whitespace");
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("Only http:// and https:// URLs are allowed");
    }

    if allowed_hosts.is_empty() {
        anyhow::bail!(
            "{tool_name} tool is enabled but no allowed_hosts are configured. \
             Add [{tool_name}].allowed_hosts in config.toml"
        );
    }

    let target = extract_host(url)?;
    let host = target.host.as_str();

    let private_or_local = is_private_or_local_host(host);
    if private_or_local && !allow_private_hosts {
        anyhow::bail!("Blocked local/private host: {host}");
    }

    if host_matches_allowlist(&target, blocked_hosts, true) {
        anyhow::bail!("Host '{host}' is in {tool_name}.blocked_hosts");
    }

    let matches_allowed_host = host_matches_allowlist(&target, allowed_hosts, true);
    let matches_explicit_allowed_host = host_matches_allowlist(&target, allowed_hosts, false);

    if private_or_local && !matches_explicit_allowed_host {
        anyhow::bail!(
            "Blocked local/private host: {host}. Private hosts must be explicitly listed in {tool_name}.allowed_hosts"
        );
    }

    if !matches_allowed_host {
        anyhow::bail!("Host '{host}' is not in {tool_name}.allowed_hosts");
    }

    if !(allow_private_hosts && matches_explicit_allowed_host) {
        validate_resolved_host_is_public(host)?;
    }

    Ok(url.to_string())
}

fn append_chunk_with_cap(buffer: &mut Vec<u8>, chunk: &[u8], hard_cap: usize) -> bool {
    if buffer.len() >= hard_cap {
        return true;
    }

    let remaining = hard_cap - buffer.len();
    if chunk.len() > remaining {
        buffer.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    buffer.extend_from_slice(chunk);
    buffer.len() >= hard_cap
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

fn normalize_allowed_hosts_or_public_default(hosts: Vec<String>) -> Vec<String> {
    if hosts.is_empty() {
        vec!["*".into()]
    } else {
        normalize_allowed_hosts(hosts)
    }
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
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("Only http:// and https:// URLs are allowed"))?;

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
        anyhow::bail!("IPv6 hosts are not supported in fetch_web_page");
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
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    let has_local_tld = bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if bare == "localhost" || bare.ends_with(".localhost") || has_local_tld {
        return true;
    }

    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(v6),
        };
    }

    false
}

#[cfg(not(test))]
fn validate_resolved_host_is_public(host: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    let ips = (host, 0)
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("Failed to resolve host '{host}': {e}"))?
        .map(|addr| addr.ip())
        .collect::<Vec<_>>();

    validate_resolved_ips_are_public(host, &ips)
}

#[cfg(test)]
fn validate_resolved_host_is_public(_host: &str) -> anyhow::Result<()> {
    // DNS checks are covered by validate_resolved_ips_are_public unit tests.
    Ok(())
}

fn validate_resolved_ips_are_public(host: &str, ips: &[std::net::IpAddr]) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        let non_global = match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(*v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(*v6),
        };
        if non_global {
            anyhow::bail!("Blocked host '{host}' resolved to non-global address {ip}");
        }
    }

    Ok(())
}

fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || a >= 240
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        || (a == 198 && (18..=19).contains(&b))
}

fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00
        || (segs[0] & 0xffc0) == 0xfe80
        || (segs[0] == 0x2001 && segs[1] == 0x0db8)
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::config::{FirecrawlConfig, WebFetchConfig, WebFetchProvider};
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

    use super::*;

    fn test_tool(allowed_hosts: Vec<&str>) -> WebFetchTool {
        test_tool_with_blocklist(allowed_hosts, vec![])
    }

    fn test_tool_with_blocklist(
        allowed_hosts: Vec<&str>,
        blocked_hosts: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_hosts.into_iter().map(String::from).collect(),
            blocked_hosts.into_iter().map(String::from).collect(),
            false,
            500_000,
            30,
        )
    }

    // ── Name and schema ──────────────────────────────────────────

    #[test]
    fn name_is_fetch_web_page() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "fetch_web_page");
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    #[tokio::test]
    async fn local_firecrawl_fetches_markdown_without_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8_192];
            let bytes_read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..bytes_read]);

            assert!(request.starts_with("POST /v2/scrape HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(request.contains("\"url\":\"https://example.com/article\""));
            assert!(request.contains("\"onlyMainContent\":true"));

            let body = r##"{"success":true,"data":{"markdown":"# Local result\n\nFetched by Firecrawl."}}"##;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let config = WebFetchConfig {
            provider: WebFetchProvider::Firecrawl,
            firecrawl: FirecrawlConfig {
                base_url: format!("http://{address}"),
                ..FirecrawlConfig::default()
            },
            allowed_hosts: vec!["example.com".into()],
            ..WebFetchConfig::default()
        };
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::from_config(security, &config, false);

        let result = tool
            .execute(json!({"url": "https://example.com/article"}))
            .await
            .unwrap();

        server.await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Local result"));
        assert!(result.output.contains("Fetched by Firecrawl"));
    }

    // ── HTML to text conversion ──────────────────────────────────

    #[test]
    fn html_to_text_conversion() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = nanohtml2text::html2text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    // ── URL validation ───────────────────────────────────────────

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://docs.example.com/guide").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_honors_allowed_host_ports() {
        let tool = test_tool(vec!["example.com:8443"]);
        assert!(tool.validate_url("https://example.com:8443/page").is_ok());
        assert!(tool.validate_url("https://example.com:443/page").is_err());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("  ").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_ftp_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
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
    fn empty_allowlist_defaults_to_public_hosts() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(security, vec![], vec![], false, 500_000, 30);
        assert_eq!(tool.allowed_hosts, vec!["*"]);
        assert!(tool.validate_url("https://example.com").is_ok());
        assert!(tool.validate_url("http://127.0.0.1").is_err());
    }

    // ── SSRF protection ──────────────────────────────────────────

    #[test]
    fn ssrf_blocks_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_loopback() {
        assert!(is_private_or_local_host("127.0.0.1"));
        assert!(is_private_or_local_host("127.0.0.2"));
    }

    #[test]
    fn ssrf_blocks_rfc1918() {
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("172.16.0.1"));
        assert!(is_private_or_local_host("192.168.1.1"));
    }

    #[test]
    fn ssrf_wildcard_still_blocks_private() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn private_hosts_require_global_setting_and_explicit_host() {
        let security = Arc::new(SecurityPolicy::default());
        let wildcard = WebFetchTool::new(
            security.clone(),
            vec!["*".into()],
            vec![],
            true,
            500_000,
            30,
        );
        let err = wildcard
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("explicitly listed"));

        let explicit = WebFetchTool::new(
            security,
            vec!["localhost:8080".into()],
            vec![],
            true,
            500_000,
            30,
        );
        assert!(explicit.validate_url("https://localhost:8080").is_ok());
        assert!(explicit.validate_url("https://localhost:3000").is_err());
    }

    #[test]
    fn redirect_target_validation_allows_permitted_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        assert!(
            validate_target_url(
                "https://docs.example.com/page",
                &allowed,
                &blocked,
                false,
                "web_fetch"
            )
            .is_ok()
        );
    }

    #[test]
    fn redirect_target_validation_blocks_private_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        let err = validate_target_url(
            "https://127.0.0.1/admin",
            &allowed,
            &blocked,
            false,
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn redirect_target_validation_blocks_blocklisted_host() {
        let allowed = vec!["*".to_string()];
        let blocked = vec!["evil.com".to_string()];
        let err = validate_target_url(
            "https://evil.com/phish",
            &allowed,
            &blocked,
            false,
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("blocked_hosts"));
    }

    // ── Security policy ──────────────────────────────────────────

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["example.com".into()],
            vec![],
            false,
            500_000,
            30,
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["example.com".into()],
            vec![],
            false,
            500_000,
            30,
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    // ── Response truncation ──────────────────────────────────────

    #[test]
    fn truncate_within_limit() {
        let tool = test_tool(vec!["example.com"]);
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_over_limit() {
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            false,
            10,
            30,
        );
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

    // ── Domain normalization ─────────────────────────────────────

    #[test]
    fn normalize_host_strips_scheme_and_case() {
        let got = normalize_host_rule("  HTTPS://Docs.Example.com/path ").unwrap();
        assert_eq!(got, "docs.example.com");
    }

    #[test]
    fn normalize_deduplicates() {
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

    // ── Blocked hosts ──────────────────────────────────────────

    #[test]
    fn blocklist_rejects_exact_match() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_hosts"));
    }

    #[test]
    fn blocklist_rejects_subdomain() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://api.evil.com/v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_hosts"));
    }

    #[test]
    fn blocklist_wins_over_allowlist() {
        let tool = test_tool_with_blocklist(vec!["evil.com"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_hosts"));
    }

    #[test]
    fn blocklist_allows_non_blocked() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        assert!(tool.validate_url("https://example.com").is_ok());
    }

    #[test]
    fn append_chunk_with_cap_truncates_and_stops() {
        let mut buffer = Vec::new();
        assert!(!append_chunk_with_cap(&mut buffer, b"hello", 8));
        assert!(append_chunk_with_cap(&mut buffer, b"world", 8));
        assert_eq!(buffer, b"hellowor");
    }

    #[test]
    fn resolved_private_ip_is_rejected() {
        let ips = vec!["127.0.0.1".parse().unwrap()];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_mixed_ips_are_rejected() {
        let ips = vec![
            "93.184.216.34".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_public_ips_are_allowed() {
        let ips = vec!["93.184.216.34".parse().unwrap(), "1.1.1.1".parse().unwrap()];
        assert!(validate_resolved_ips_are_public("example.com", &ips).is_ok());
    }
}
