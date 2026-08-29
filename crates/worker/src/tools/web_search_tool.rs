//! Worker-native web search across DuckDuckGo, Brave, and Parallel.

use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{ParallelSearchConfig, ParallelSearchMode, WebSearchConfig, WebSearchProvider};
use crate::tools::{Tool, ToolCategory, ToolResult};

const WEB_SEARCH_OUTPUT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WebSearchOutputKind {
    #[default]
    WebSearchResults,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WebSearchOutput {
    #[serde(rename = "type")]
    kind: WebSearchOutputKind,
    schema_version: u8,
    objective: String,
    provenance: SearchProvenance,
    sources: Vec<WebSource>,
}

impl WebSearchOutput {
    fn new(
        objective: &str,
        provider: WebSearchProvider,
        request_id: Option<String>,
        sources: Vec<WebSource>,
    ) -> Self {
        Self {
            kind: WebSearchOutputKind::WebSearchResults,
            schema_version: WEB_SEARCH_OUTPUT_SCHEMA_VERSION,
            objective: objective.to_string(),
            provenance: SearchProvenance {
                provider,
                request_id,
            },
            sources,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SearchProvenance {
    provider: WebSearchProvider,
    request_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WebSource {
    source_id: String,
    label: String,
    rank: usize,
    title: String,
    url: String,
    published_at: Option<String>,
    excerpts: Vec<String>,
}

impl WebSource {
    fn new(
        rank: usize,
        title: &str,
        url: &str,
        published_at: Option<&str>,
        excerpts: impl IntoIterator<Item = String>,
        request_id: Option<&str>,
    ) -> Option<Self> {
        let url = citable_url(url)?;
        let title = title.trim();
        let title = if title.is_empty() {
            "Untitled source".to_string()
        } else {
            title.to_string()
        };
        let request_id = request_id.map(str::trim).filter(|value| !value.is_empty());
        let source_id = request_id
            .map(|request_id| format!("{request_id}:{rank}"))
            .unwrap_or_else(|| url.clone());
        let published_at = published_at
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let excerpts = excerpts
            .into_iter()
            .map(|excerpt| excerpt.trim().to_string())
            .filter(|excerpt| !excerpt.is_empty())
            .collect();

        Some(Self {
            source_id,
            label: format!("S{rank}"),
            rank,
            title,
            url,
            published_at,
            excerpts,
        })
    }
}

#[derive(Debug, Serialize)]
struct ParallelSearchRequest<'a> {
    objective: &'a str,
    search_queries: &'a [String],
    mode: ParallelSearchMode,
    max_chars_total: usize,
}

#[derive(Debug, Deserialize)]
struct ParallelSearchResponse {
    search_id: String,
    results: Vec<ParallelSearchResult>,
}

#[derive(Debug, Deserialize)]
struct ParallelSearchResult {
    url: String,
    title: String,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Vec<String>,
}

/// Web search tool for searching the internet.
/// Supports DuckDuckGo, Brave Search, and Parallel Search.
pub struct WebSearchTool {
    client: Result<reqwest::Client, String>,
    provider: WebSearchProvider,
    brave_api_key: Option<String>,
    parallel: ParallelSearchConfig,
    max_results: usize,
}

impl WebSearchTool {
    pub fn new(
        provider: WebSearchProvider,
        brave_api_key: Option<String>,
        max_results: usize,
        timeout_secs: u64,
    ) -> Self {
        let timeout_secs = timeout_secs.max(1);
        Self {
            client: build_search_client(timeout_secs),
            provider,
            brave_api_key,
            parallel: ParallelSearchConfig::default(),
            max_results: max_results.clamp(1, 10),
        }
    }

    /// Construct the tool from worker configuration.
    pub fn from_config(config: &WebSearchConfig) -> Self {
        let mut parallel = config.parallel.clone();
        parallel.max_chars_total = parallel.max_chars_total.max(1);
        let timeout_secs = config.timeout_secs.max(1);
        Self {
            client: build_search_client(timeout_secs),
            provider: config.provider,
            brave_api_key: config.brave_api_key.clone(),
            parallel,
            max_results: config.max_results.clamp(1, 10),
        }
    }

    fn client(&self) -> anyhow::Result<&reqwest::Client> {
        self.client
            .as_ref()
            .map_err(|error| anyhow::anyhow!("Failed to build web search HTTP client: {error}"))
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<WebSearchOutput> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let response = self
            .client()?
            .get(&search_url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DuckDuckGo search failed with status: {}",
                response.status()
            );
        }

        let html = response.text().await?;
        self.parse_duckduckgo_results(&html, query)
    }

    fn parse_duckduckgo_results(&self, html: &str, query: &str) -> anyhow::Result<WebSearchOutput> {
        // Extract result links: <a class="result__a" href="...">Title</a>
        let link_regex = Regex::new(
            r#"<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
        )?;

        // Extract snippets: <a class="result__snippet">...</a>
        let snippet_regex = Regex::new(r#"<a class="result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)?;

        let link_matches: Vec<_> = link_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        let snippet_matches: Vec<_> = snippet_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        let sources = link_matches
            .iter()
            .take(self.max_results)
            .enumerate()
            .filter_map(|(index, caps)| {
                let url_str = decode_ddg_redirect_url(&caps[1]);
                let title = strip_tags(&caps[2]);
                let excerpts = snippet_matches
                    .get(index)
                    .map(|snippet| vec![strip_tags(&snippet[1])])
                    .unwrap_or_default();
                WebSource::new(index + 1, &title, &url_str, None, excerpts, None)
            })
            .collect();

        Ok(WebSearchOutput::new(
            query,
            WebSearchProvider::DuckDuckGo,
            None,
            sources,
        ))
    }

    async fn search_brave(&self, query: &str) -> anyhow::Result<WebSearchOutput> {
        let api_key = self
            .brave_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let response = self
            .client()?
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Brave search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_brave_results(&json, query)
    }

    fn parse_brave_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<WebSearchOutput> {
        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid Brave API response"))?;

        let sources = results
            .iter()
            .take(self.max_results)
            .enumerate()
            .filter_map(|(index, result)| {
                let title = result
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("No title");
                let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let description = result
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                WebSource::new(
                    index + 1,
                    title,
                    url,
                    None,
                    vec![description.to_string()],
                    None,
                )
            })
            .collect();

        Ok(WebSearchOutput::new(
            query,
            WebSearchProvider::Brave,
            None,
            sources,
        ))
    }

    fn parallel_endpoint(&self) -> anyhow::Result<String> {
        let base_url = self.parallel.base_url.trim().trim_end_matches('/');
        if base_url.is_empty() {
            anyhow::bail!("Parallel base_url cannot be empty");
        }

        let endpoint = format!("{base_url}/v1/search");
        let parsed = reqwest::Url::parse(&endpoint)
            .map_err(|error| anyhow::anyhow!("Invalid Parallel base_url: {error}"))?;
        match parsed.scheme() {
            "http" | "https" => Ok(endpoint),
            scheme => anyhow::bail!("Unsupported Parallel URL scheme: {scheme}"),
        }
    }

    async fn search_parallel(
        &self,
        objective: &str,
        search_queries: &[String],
    ) -> anyhow::Result<WebSearchOutput> {
        let api_key = self
            .parallel
            .api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Parallel API key not configured"))?
            .trim();
        if api_key.is_empty() {
            anyhow::bail!("Parallel API key not configured");
        }
        let payload = ParallelSearchRequest {
            objective,
            search_queries,
            mode: self.parallel.mode,
            max_chars_total: self.parallel.max_chars_total,
        };
        let response = self
            .client()?
            .post(self.parallel_endpoint()?)
            .header("x-api-key", api_key)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let error = response
                .text()
                .await
                .ok()
                .filter(|body| !body.trim().is_empty())
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("Unknown").to_string());
            anyhow::bail!("Parallel HTTP {}: {error}", status.as_u16());
        }

        let response: ParallelSearchResponse = response.json().await?;
        Ok(self.parallel_output(objective, response))
    }

    fn parallel_output(
        &self,
        objective: &str,
        response: ParallelSearchResponse,
    ) -> WebSearchOutput {
        let request_id = response.search_id;
        let sources = response
            .results
            .into_iter()
            .take(self.max_results)
            .enumerate()
            .filter_map(|(index, result)| {
                WebSource::new(
                    index + 1,
                    &result.title,
                    &result.url,
                    result.publish_date.as_deref(),
                    result.excerpts,
                    Some(&request_id),
                )
            })
            .collect();

        WebSearchOutput::new(
            objective,
            WebSearchProvider::Parallel,
            Some(request_id),
            sources,
        )
    }
}

fn decode_ddg_redirect_url(raw_url: &str) -> String {
    if let Some(index) = raw_url.find("uddg=") {
        let encoded = &raw_url[index + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            return decoded.into_owned();
        }
    }

    raw_url.to_string()
}

fn strip_tags(content: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(content, "").to_string()
}

fn citable_url(raw_url: &str) -> Option<String> {
    let raw_url = raw_url.trim();
    let parsed = reqwest::Url::parse(raw_url).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(raw_url.to_string()),
        _ => None,
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "search_web"
    }

    fn description(&self) -> &str {
        "Search the web for current information, news, or research. Returns versioned JSON with ranked sources, titles, URLs, dates, and evidence excerpts. When using these results, cite supporting sources as Markdown links in the form [source title](source URL). Only cite URLs present in the tool result."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The self-contained search objective or a single search query."
                },
                "search_queries": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 200
                    },
                    "minItems": 1,
                    "maxItems": 5,
                    "description": "Optional concise keyword queries covering different angles. Provide 2-3 for broad research."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }
        let query = query.trim();
        let search_queries = parse_search_queries(&args, query)?;

        tracing::info!(query_len = query.len(), "Searching web");

        let output = match self.provider {
            WebSearchProvider::DuckDuckGo => self.search_duckduckgo(query).await?,
            WebSearchProvider::Brave => self.search_brave(query).await?,
            WebSearchProvider::Parallel => self.search_parallel(query, &search_queries).await?,
        };
        let output = serde_json::to_string(&output)?;

        Ok(ToolResult::success(output))
    }
}

fn build_search_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())
}

fn parse_search_queries(args: &serde_json::Value, fallback: &str) -> anyhow::Result<Vec<String>> {
    let Some(values) = args.get("search_queries") else {
        return Ok(vec![fallback.to_string()]);
    };
    let values = values
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("search_queries must be an array of strings"))?;
    if values.is_empty() || values.len() > 5 {
        anyhow::bail!("search_queries must contain between 1 and 5 queries");
    }

    values
        .iter()
        .map(|value| {
            let query = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("search_queries must contain only strings"))?
                .trim();
            if query.is_empty() {
                anyhow::bail!("search_queries cannot contain empty queries");
            }
            if query.len() > 200 {
                anyhow::bail!("search_queries cannot contain queries longer than 200 bytes");
            }
            Ok(query.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        assert_eq!(tool.name(), "search_web");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["search_queries"].is_object());
    }

    #[test]
    fn test_strip_tags() {
        let html = "<b>Hello</b> <i>World</i>";
        assert_eq!(strip_tags(html), "Hello World");
    }

    #[test]
    fn test_parse_duckduckgo_results_empty() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert_eq!(result.kind, WebSearchOutputKind::WebSearchResults);
        assert_eq!(result.schema_version, WEB_SEARCH_OUTPUT_SCHEMA_VERSION);
        assert_eq!(result.objective, "test");
        assert_eq!(result.provenance.provider, WebSearchProvider::DuckDuckGo);
        assert!(result.sources.is_empty());
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert_eq!(result.sources.len(), 1);
        assert_eq!(result.sources[0].label, "S1");
        assert_eq!(result.sources[0].title, "Example Title");
        assert_eq!(result.sources[0].url, "https://example.com");
        assert_eq!(result.sources[0].excerpts, vec!["This is a description"]);
    }

    #[test]
    fn test_parse_duckduckgo_results_decodes_redirect_url() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1&amp;rut=test">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert_eq!(result.sources[0].url, "https://example.com/path?a=1");
        assert!(!result.sources[0].url.contains("rut=test"));
    }

    #[test]
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 0, 0);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert_eq!(result.sources.len(), 1);
        assert_eq!(result.sources[0].title, "Example Title");
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new(WebSearchProvider::DuckDuckGo, None, 5, 15);
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new(WebSearchProvider::Brave, None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn search_queries_default_to_the_objective() {
        let queries = parse_search_queries(&json!({}), "rust async changes").unwrap();

        assert_eq!(queries, vec!["rust async changes"]);
    }

    #[test]
    fn search_queries_reject_empty_entries() {
        let error = parse_search_queries(
            &json!({"search_queries": ["rust async", "  "]}),
            "rust changes",
        )
        .unwrap_err();

        assert!(error.to_string().contains("empty"));
    }

    #[tokio::test]
    async fn parallel_search_sends_multi_query_request_and_formats_sources() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8_192];
            let bytes_read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..bytes_read]);

            assert!(request.starts_with("POST /v1/search HTTP/1.1"));
            assert!(request.contains("x-api-key: test-parallel-key"));
            assert!(request.contains("\"objective\":\"Current Rust async ecosystem changes\""));
            assert!(request.contains(
                "\"search_queries\":[\"Tokio recent releases\",\"Rust async performance\"]"
            ));
            assert!(request.contains("\"mode\":\"fast\""));
            assert!(request.contains("\"max_chars_total\":12000"));

            let body = r##"{"search_id":"search_test","results":[{"url":"https://example.com/rust","title":"Rust Async Update","publish_date":"2026-08-20","excerpts":["A relevant source excerpt."]}],"session_id":"session_test"}"##;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let config = WebSearchConfig {
            provider: WebSearchProvider::Parallel,
            parallel: ParallelSearchConfig {
                base_url: format!("http://{address}"),
                api_key: Some("test-parallel-key".into()),
                mode: ParallelSearchMode::Fast,
                max_chars_total: 12_000,
            },
            ..WebSearchConfig::default()
        };
        let tool = WebSearchTool::from_config(&config);

        let result = tool
            .execute(json!({
                "query": "Current Rust async ecosystem changes",
                "search_queries": ["Tokio recent releases", "Rust async performance"]
            }))
            .await
            .unwrap();

        server.await.unwrap();
        assert!(result.success);
        let output: WebSearchOutput =
            serde_json::from_str(result.output.as_text().unwrap()).unwrap();
        assert_eq!(output.kind, WebSearchOutputKind::WebSearchResults);
        assert_eq!(output.schema_version, WEB_SEARCH_OUTPUT_SCHEMA_VERSION);
        assert_eq!(output.objective, "Current Rust async ecosystem changes");
        assert_eq!(output.provenance.provider, WebSearchProvider::Parallel);
        assert_eq!(output.provenance.request_id.as_deref(), Some("search_test"));
        assert_eq!(output.sources.len(), 1);
        assert_eq!(output.sources[0].source_id, "search_test:1");
        assert_eq!(output.sources[0].label, "S1");
        assert_eq!(output.sources[0].rank, 1);
        assert_eq!(output.sources[0].title, "Rust Async Update");
        assert_eq!(output.sources[0].url, "https://example.com/rust");
        assert_eq!(
            output.sources[0].published_at.as_deref(),
            Some("2026-08-20")
        );
        assert_eq!(
            output.sources[0].excerpts,
            vec!["A relevant source excerpt."]
        );
    }
}
