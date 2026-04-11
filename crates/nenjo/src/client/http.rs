//! HTTP client implementation.

use super::types::ActiveAgentHeartbeatState;
use reqwest::{Client, StatusCode, header};
use tracing::{debug, error, warn};
use uuid::Uuid;

use super::error::ApiClientError;
use super::types::*;
use crate::manifest::*;

/// Result alias for client operations.
pub type Result<T> = std::result::Result<T, ApiClientError>;

/// Typed HTTP client for the Nenjo backend.
///
/// Every request automatically includes the `X-API-Key` header.
#[derive(Debug, Clone)]
pub struct NenjoClient {
    http: Client,
    base_url: String,
    api_key: String,
}

impl NenjoClient {
    /// Create a new client pointing at the given backend URL.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");

        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    /// Create a client with a custom reqwest [`Client`] (useful for testing).
    pub fn with_http_client(
        http: Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    /// Return the base URL this client targets.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // -----------------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------------

    /// Fetch the full manifest (projects, routines, models, agents, etc.).
    pub async fn fetch_manifest(&self) -> Result<Manifest> {
        let url = format!("{}/api/v1/manifest", self.base_url);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let text = resp.text().await.map_err(|e| {
                    error!(error = %e, "Bootstrap: failed to read response body");
                    ApiClientError::Http(e)
                })?;

                match serde_json::from_str::<Manifest>(&text) {
                    Ok(data) => {
                        debug!(
                            projects = data.projects.len(),
                            routines = data.routines.len(),
                            models = data.models.len(),
                            agents = data.agents.len(),
                            "Bootstrap data fetched"
                        );
                        Ok(data)
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            line = e.line(),
                            column = e.column(),
                            body_len = text.len(),
                            body_preview = &text[..text.len().min(500)],
                            "Bootstrap: failed to deserialize response"
                        );
                        Err(ApiClientError::Parse(e.to_string()))
                    }
                }
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    // -----------------------------------------------------------------------
    // Single-resource fetch (for incremental bootstrap sync)
    // -----------------------------------------------------------------------

    pub async fn fetch_model(&self, id: Uuid) -> Result<Option<ModelManifest>> {
        self.fetch_resource(&format!("/api/v1/models/{id}")).await
    }

    pub async fn fetch_project(&self, id: Uuid) -> Result<Option<ProjectManifest>> {
        self.fetch_resource(&format!("/api/v1/projects/{id}")).await
    }

    pub async fn fetch_routine(&self, id: Uuid) -> Result<Option<RoutineManifest>> {
        self.fetch_resource(&format!("/api/v1/routines/{id}")).await
    }

    pub async fn list_active_cron_routines(&self) -> Result<Vec<ActiveCronRoutineState>> {
        let url = format!("{}/api/v1/routines/cron-state", self.base_url);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => resp.json().await.map_err(ApiClientError::Http),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn list_active_agent_heartbeats(&self) -> Result<Vec<ActiveAgentHeartbeatState>> {
        let url = format!("{}/api/v1/agents/heartbeat-state", self.base_url);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => resp.json().await.map_err(ApiClientError::Http),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_lambda(&self, id: Uuid) -> Result<Option<LambdaManifest>> {
        self.fetch_resource(&format!("/api/v1/lambdas/{id}")).await
    }

    pub async fn fetch_domain(&self, id: Uuid) -> Result<Option<DomainManifest>> {
        self.fetch_resource(&format!("/api/v1/domains/{id}")).await
    }

    pub async fn fetch_mcp_server(&self, id: Uuid) -> Result<Option<McpServerManifest>> {
        self.fetch_resource(&format!("/api/v1/mcp-servers/{id}"))
            .await
    }

    pub async fn fetch_ability(&self, id: Uuid) -> Result<Option<AbilityManifest>> {
        self.fetch_resource(&format!("/api/v1/abilities/{id}"))
            .await
    }

    pub async fn fetch_context_block(&self, id: Uuid) -> Result<Option<ContextBlockManifest>> {
        self.fetch_resource(&format!("/api/v1/context-blocks/{id}"))
            .await
    }

    pub async fn fetch_agent(&self, id: Uuid) -> Result<Option<AgentManifest>> {
        self.fetch_resource(&format!("/api/v1/agents/{id}")).await
    }

    pub async fn fetch_council(&self, id: Uuid) -> Result<Option<CouncilManifest>> {
        let detail: Option<CouncilDetailResponse> = self
            .fetch_resource(&format!("/api/v1/councils/{id}"))
            .await?;
        Ok(detail.map(|d| d.into()))
    }

    // -----------------------------------------------------------------------
    // Document sync
    // -----------------------------------------------------------------------

    /// List all documents for a project.
    pub async fn list_project_documents(&self, project_id: Uuid) -> Result<Vec<DocumentSyncMeta>> {
        let url = format!("{}/api/v1/projects/{}/documents", self.base_url, project_id);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let docs: Vec<DocumentSyncMeta> = resp.json().await?;
                debug!(project_id = %project_id, count = docs.len(), "Listed project documents");
                Ok(docs)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    /// Get the text content of a single project document.
    pub async fn get_document_content(&self, project_id: Uuid, doc_id: Uuid) -> Result<String> {
        let url = format!(
            "{}/api/v1/projects/{}/documents/{}/content",
            self.base_url, project_id, doc_id
        );
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let content = resp.text().await.map_err(ApiClientError::Http)?;
                debug!(project_id = %project_id, doc_id = %doc_id, "Fetched document content");
                Ok(content)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    async fn fetch_resource<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<T>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.get(&url).await?;
        match resp.status() {
            StatusCode::OK => {
                let item = resp
                    .json::<T>()
                    .await
                    .map_err(|e| ApiClientError::Parse(format!("Failed to parse {path}: {e}")))?;
                Ok(Some(item))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(self.api_error(status, resp).await),
        }
    }

    fn auth_headers(&self) -> header::HeaderMap {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "X-API-Key",
            header::HeaderValue::from_str(&self.api_key).expect("invalid API key for header"),
        );
        headers
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        self.http
            .get(url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(ApiClientError::Http)
    }

    async fn api_error(&self, status: StatusCode, resp: reqwest::Response) -> ApiClientError {
        let body = resp.text().await.unwrap_or_default();

        if let Ok(err_resp) = serde_json::from_str::<ApiErrorResponse>(&body) {
            return ApiClientError::Api {
                status: status.as_u16(),
                code: err_resp.error.code,
                message: err_resp.error.message,
            };
        }

        warn!(status = %status, body_len = body.len(), "Unstructured API error");
        ApiClientError::Api {
            status: status.as_u16(),
            code: "unknown".into(),
            message: body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = NenjoClient::new("http://localhost:8080", "test-key");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_trailing_slash_stripped() {
        let client = NenjoClient::new("http://localhost:8080/", "key");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_auth_headers() {
        let client = NenjoClient::new("http://localhost", "my-secret");
        let headers = client.auth_headers();
        assert_eq!(
            headers.get("X-API-Key").unwrap().to_str().unwrap(),
            "my-secret"
        );
    }
}
