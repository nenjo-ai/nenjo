//! HTTP client implementation.

use std::sync::Arc;

use super::types::ActiveAgentHeartbeatState;
use async_trait::async_trait;
use nenjo_events::EncryptedPayload;
use reqwest::{Client, StatusCode, header};
use tracing::{debug, error, warn};
use uuid::Uuid;

use super::error::ApiClientError;
use super::types::*;
use crate::manifest::*;

/// Result alias for client operations.
pub type Result<T> = std::result::Result<T, ApiClientError>;

/// Encrypted payload codec used to normalize platform fetch responses.
///
/// The client owns HTTP and response shaping, but not key storage. Worker or
/// embedded runtimes can provide a codec backed by secure-envelope, KMS, or
/// another crypto provider.
#[async_trait]
pub trait PayloadCodec: Send + Sync {
    async fn decode_text(&self, payload: &EncryptedPayload) -> anyhow::Result<Option<String>>;
}

/// Default payload codec for clients that do not decrypt platform payloads.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPayloadCodec;

#[async_trait]
impl PayloadCodec for NoopPayloadCodec {
    async fn decode_text(&self, _payload: &EncryptedPayload) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

/// Typed HTTP client for the Nenjo backend.
///
/// Every request automatically includes the `X-API-Key` header.
#[derive(Clone)]
pub struct NenjoClient {
    http: Client,
    base_url: String,
    api_key: String,
    payload_codec: Arc<dyn PayloadCodec>,
}

impl std::fmt::Debug for NenjoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NenjoClient")
            .field("base_url", &self.base_url)
            .field("payload_codec", &"<configured>")
            .finish_non_exhaustive()
    }
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
            payload_codec: Arc::new(NoopPayloadCodec),
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
            payload_codec: Arc::new(NoopPayloadCodec),
        }
    }

    /// Return a client that decodes encrypted platform payloads in fetch responses.
    pub fn with_payload_codec<C>(mut self, codec: C) -> Self
    where
        C: PayloadCodec + 'static,
    {
        self.payload_codec = Arc::new(codec);
        self
    }

    /// Return a client using a shared encrypted platform payload codec.
    pub fn with_shared_payload_codec(mut self, codec: Arc<dyn PayloadCodec>) -> Self {
        self.payload_codec = codec;
        self
    }

    /// Return the base URL this client targets.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // -----------------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------------

    /// Fetch the raw manifest bootstrap payload.
    pub async fn fetch_manifest_json(&self) -> Result<serde_json::Value> {
        let url = format!("{}/api/v1/manifest", self.base_url);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => resp.json().await.map_err(ApiClientError::Http),
            status => Err(self.api_error(status, resp).await),
        }
    }

    /// Fetch the full manifest (projects, routines, models, agents, etc.).
    pub async fn fetch_manifest(&self) -> Result<Manifest> {
        let value = self.fetch_manifest_json().await?;
        let text = serde_json::to_string(&value).map_err(|e| {
            ApiClientError::Parse(format!("Failed to serialize manifest JSON: {e}"))
        })?;

        match serde_json::from_value::<Manifest>(value) {
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
        let detail: Option<RoutineDetailResponse> = self
            .fetch_resource(&format!("/api/v1/routines/{id}"))
            .await?;
        Ok(detail.map(Into::into))
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

    pub async fn register_worker_enrollment(
        &self,
        request: &WorkerEnrollmentRequest,
    ) -> Result<WorkerEnrollmentStatusResponse> {
        let url = format!("{}/api/v1/workers/enrollment", self.base_url);
        let resp = self.post_json(&url, request).await?;

        match resp.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::ACCEPTED => {
                resp.json().await.map_err(ApiClientError::Http)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_worker_enrollment_status(
        &self,
        api_key_id: Uuid,
    ) -> Result<Option<WorkerEnrollmentStatusResponse>> {
        self.fetch_resource(&format!("/api/v1/workers/enrollment/{api_key_id}"))
            .await
    }

    pub async fn fetch_domain(&self, id: Uuid) -> Result<Option<DomainManifest>> {
        let url = format!("{}/api/v1/domains/{id}/manifest", self.base_url);
        let resp = self.get(&url).await?;
        match resp.status() {
            StatusCode::OK => {
                let response: DomainManifestResponse = resp.json().await.map_err(|source| {
                    ApiClientError::Parse(format!(
                        "Failed to parse /api/v1/domains/{id}/manifest: {source}"
                    ))
                })?;
                Ok(Some(response.into()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_mcp_server(&self, id: Uuid) -> Result<Option<McpServerManifest>> {
        self.fetch_resource(&format!("/api/v1/mcp-servers/{id}"))
            .await
    }

    pub async fn fetch_ability(&self, id: Uuid) -> Result<Option<AbilityManifest>> {
        self.fetch_resource(&format!("/api/v1/abilities/{id}"))
            .await
    }

    pub async fn fetch_context_block_summary(
        &self,
        id: Uuid,
    ) -> Result<Option<ContextBlockSummaryResponse>> {
        self.fetch_resource(&format!("/api/v1/context-blocks/{id}"))
            .await
    }

    pub async fn fetch_context_block_content(
        &self,
        id: Uuid,
    ) -> Result<Option<ContextBlockContentResponse>> {
        let content = self
            .fetch_resource::<ContextBlockContentResponse>(&format!(
                "/api/v1/context-blocks/{id}/content"
            ))
            .await?;
        self.decode_context_block_content(content).await
    }

    pub async fn fetch_agent(&self, id: Uuid) -> Result<Option<AgentManifest>> {
        let detail: Option<AgentDetailResponse> =
            self.fetch_resource(&format!("/api/v1/agents/{id}")).await?;
        Ok(detail.map(Into::into))
    }

    pub async fn fetch_agent_prompt_config(
        &self,
        id: Uuid,
    ) -> Result<Option<AgentPromptConfigResponse>> {
        let url = format!("{}/api/v1/agents/{}/prompt", self.base_url, id);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let response = resp
                    .json::<AgentPromptConfigResponse>()
                    .await
                    .map_err(ApiClientError::Http)?;
                self.decode_agent_prompt_config(Some(response)).await
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_council(&self, id: Uuid) -> Result<Option<CouncilManifest>> {
        let detail: Option<CouncilDetailResponse> = self
            .fetch_resource(&format!("/api/v1/councils/{id}"))
            .await?;
        Ok(detail.map(|d| d.into()))
    }

    // -----------------------------------------------------------------------
    // Knowledge sync
    // -----------------------------------------------------------------------

    pub async fn list_knowledge_packs(&self) -> Result<Vec<KnowledgePackSyncMeta>> {
        let url = format!("{}/api/v1/knowledge", self.base_url);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let packs: Vec<KnowledgePackSyncMeta> = resp.json().await?;
                debug!(count = packs.len(), "Listed knowledge packs");
                Ok(packs)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn list_knowledge_items(&self, pack_id: Uuid) -> Result<Vec<KnowledgeItemSyncMeta>> {
        let url = format!("{}/api/v1/knowledge/{}/items", self.base_url, pack_id);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let items: Vec<KnowledgeItemSyncMeta> = resp.json().await?;
                debug!(pack_id = %pack_id, count = items.len(), "Listed knowledge items");
                Ok(items)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn get_knowledge_item_content(
        &self,
        pack_id: Uuid,
        item_id: Uuid,
    ) -> Result<KnowledgeItemSyncContent> {
        let url = format!(
            "{}/api/v1/knowledge/{}/items/{}/content",
            self.base_url, pack_id, item_id
        );
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let content = resp.json().await.map_err(ApiClientError::Http)?;
                let content = self.decode_document_content(content).await?;
                debug!(pack_id = %pack_id, item_id = %item_id, "Fetched knowledge item content");
                Ok(content)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn list_knowledge_item_edges(
        &self,
        pack_id: Uuid,
        item_id: Uuid,
    ) -> Result<Vec<KnowledgeItemSyncEdge>> {
        let url = format!(
            "{}/api/v1/knowledge/{}/items/{}/edges",
            self.base_url, pack_id, item_id
        );
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let edges: Vec<KnowledgeItemSyncEdge> = resp.json().await?;
                debug!(pack_id = %pack_id, item_id = %item_id, count = edges.len(), "Listed knowledge item edges");
                Ok(edges)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    #[deprecated(note = "Use workspace knowledge APIs")]
    pub async fn list_project_documents(&self, project_id: Uuid) -> Result<Vec<DocumentSyncMeta>> {
        let _ = project_id;
        self.list_knowledge_packs().await.map(|_| Vec::new())
    }

    #[deprecated(note = "Use get_knowledge_item_content")]
    pub async fn get_document_content(
        &self,
        project_id: Uuid,
        doc_id: Uuid,
    ) -> Result<DocumentSyncContent> {
        self.get_knowledge_item_content(project_id, doc_id).await
    }

    #[deprecated(note = "Use list_knowledge_item_edges")]
    pub async fn list_project_document_edges(
        &self,
        project_id: Uuid,
        doc_id: Uuid,
    ) -> Result<Vec<DocumentSyncEdge>> {
        self.list_knowledge_item_edges(project_id, doc_id).await
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

    async fn post_json<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<reqwest::Response> {
        self.http
            .post(url)
            .headers(self.auth_headers())
            .json(body)
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

    async fn decode_agent_prompt_config(
        &self,
        response: Option<AgentPromptConfigResponse>,
    ) -> Result<Option<AgentPromptConfigResponse>> {
        let Some(mut response) = response else {
            return Ok(None);
        };
        let Some(payload) = response.encrypted_payload.as_ref() else {
            return Ok(Some(response));
        };
        let Some(plaintext) = self
            .payload_codec
            .decode_text(payload)
            .await
            .map_err(|error| {
                ApiClientError::Parse(format!("Failed to decrypt agent prompt: {error}"))
            })?
        else {
            return Ok(Some(response));
        };

        response.prompt_config = Some(serde_json::from_str(&plaintext).map_err(|error| {
            ApiClientError::Parse(format!("Failed to parse decrypted agent prompt: {error}"))
        })?);
        response.encrypted_payload = None;
        Ok(Some(response))
    }

    async fn decode_context_block_content(
        &self,
        response: Option<ContextBlockContentResponse>,
    ) -> Result<Option<ContextBlockContentResponse>> {
        let Some(mut response) = response else {
            return Ok(None);
        };
        let Some(payload) = response.encrypted_payload.as_ref() else {
            return Ok(Some(response));
        };
        let Some(plaintext) = self
            .payload_codec
            .decode_text(payload)
            .await
            .map_err(|error| {
                ApiClientError::Parse(format!("Failed to decrypt context block content: {error}"))
            })?
        else {
            return Ok(Some(response));
        };

        response.template = Some(serde_json::from_str(&plaintext).map_err(|error| {
            ApiClientError::Parse(format!(
                "Failed to parse decrypted context block content: {error}"
            ))
        })?);
        response.encrypted_payload = None;
        Ok(Some(response))
    }

    async fn decode_document_content(
        &self,
        mut content: DocumentSyncContent,
    ) -> Result<DocumentSyncContent> {
        let Some(payload) = content.encrypted_payload.as_ref() else {
            return Ok(content);
        };
        let Some(plaintext) = self
            .payload_codec
            .decode_text(payload)
            .await
            .map_err(|error| {
                ApiClientError::Parse(format!("Failed to decrypt document content: {error}"))
            })?
        else {
            return Ok(content);
        };

        content.content = Some(serde_json::from_str(&plaintext).map_err(|error| {
            ApiClientError::Parse(format!(
                "Failed to parse decrypted document content: {error}"
            ))
        })?);
        content.encrypted_payload = None;
        Ok(content)
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
