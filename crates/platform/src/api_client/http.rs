//! HTTP client implementation.

use std::sync::Arc;

use super::types::ActiveAgentHeartbeatState;
use async_trait::async_trait;
use nenjo_events::EncryptedPayload;
use crate::manifest_contract::ProjectRecord;
use reqwest::{Client, StatusCode, header};
use tracing::{debug, error, warn};
use uuid::Uuid;

use super::error::ApiClientError;
use super::types::*;
use nenjo::Slug;
use nenjo::manifest::*;

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
pub struct ApiClient {
    http: Client,
    base_url: String,
    api_key: String,
    payload_codec: Arc<dyn PayloadCodec>,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("base_url", &self.base_url)
            .field("payload_codec", &"<configured>")
            .finish_non_exhaustive()
    }
}

impl ApiClient {
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

    pub async fn fetch_model(&self, resource: &Slug) -> Result<Option<ModelManifest>> {
        self.fetch_resource(&format!("/api/v1/models/{resource}"))
            .await
    }

    pub async fn fetch_project(&self, resource: &Slug) -> Result<Option<ProjectRecord>> {
        self.fetch_resource(&format!("/api/v1/projects/{resource}"))
            .await
    }

    pub async fn fetch_routine(&self, resource: &Slug) -> Result<Option<RoutineRecord>> {
        self.fetch_resource(&format!("/api/v1/routines/{resource}"))
            .await
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

    pub async fn fetch_domain(&self, resource: &Slug) -> Result<Option<DomainManifest>> {
        let url = format!("{}/api/v1/domains/{resource}/prompt", self.base_url);
        let resp = self.get(&url).await?;
        match resp.status() {
            StatusCode::OK => {
                let response = resp.json::<DomainPromptRecord>().await.map_err(|source| {
                    ApiClientError::Parse(format!(
                        "Failed to parse /api/v1/domains/{resource}/prompt: {source}"
                    ))
                })?;
                Ok(self
                    .decode_domain_prompt(Some(response))
                    .await?
                    .map(|record| record.to_manifest()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_mcp_server(&self, resource: &Slug) -> Result<Option<McpServerManifest>> {
        self.fetch_resource(&format!("/api/v1/mcp-servers/{resource}"))
            .await
    }

    pub async fn fetch_ability(&self, resource: &Slug) -> Result<Option<AbilityManifest>> {
        self.fetch_resource(&format!("/api/v1/abilities/{resource}"))
            .await
    }

    pub async fn fetch_context_block_summary(
        &self,
        resource: &Slug,
    ) -> Result<Option<ContextBlockRecord>> {
        self.fetch_resource(&format!("/api/v1/context-blocks/{resource}"))
            .await
    }

    pub async fn fetch_context_block_content(
        &self,
        resource: &Slug,
    ) -> Result<Option<ContextBlockContentRecord>> {
        let content = self
            .fetch_resource::<ContextBlockContentRecord>(&format!(
                "/api/v1/context-blocks/{resource}/content"
            ))
            .await?;
        self.decode_context_block_content(content).await
    }

    pub async fn fetch_agent(&self, resource: &Slug) -> Result<Option<AgentRecord>> {
        self.fetch_resource(&format!("/api/v1/agents/{resource}"))
            .await
    }

    pub async fn fetch_agent_prompt_config(
        &self,
        resource: &Slug,
    ) -> Result<Option<AgentPromptRecord>> {
        let url = format!("{}/api/v1/agents/{}/prompt", self.base_url, resource);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let response = resp
                    .json::<AgentPromptRecord>()
                    .await
                    .map_err(ApiClientError::Http)?;
                self.decode_agent_prompt_config(Some(response)).await
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn fetch_council(&self, resource: &Slug) -> Result<Option<CouncilRecord>> {
        self.fetch_resource(&format!("/api/v1/councils/{resource}"))
            .await
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

    pub async fn list_knowledge_docs(&self, pack: &str) -> Result<Vec<KnowledgeDocumentRecord>> {
        let url = format!("{}/api/v1/knowledge/{}/items", self.base_url, pack);
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let mut docs: Vec<KnowledgeDocumentRecord> = resp.json().await?;
                for doc in &mut docs {
                    if doc.pack_slug.is_empty() {
                        doc.pack_slug = pack.to_string();
                    }
                }
                debug!(pack = %pack, count = docs.len(), "Listed knowledge documents");
                Ok(docs)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn get_knowledge_doc_content(
        &self,
        pack: &str,
        doc: &str,
    ) -> Result<KnowledgeDocSyncContent> {
        let url = format!(
            "{}/api/v1/knowledge/{}/items/{}/content",
            self.base_url, pack, doc
        );
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let content = resp.json().await.map_err(ApiClientError::Http)?;
                let content = self.decode_document_content(content).await?;
                debug!(pack = %pack, doc = %doc, "Fetched knowledge document content");
                Ok(content)
            }
            status => Err(self.api_error(status, resp).await),
        }
    }

    pub async fn list_knowledge_doc_edges(
        &self,
        pack: &str,
        doc: &str,
    ) -> Result<Vec<KnowledgeDocumentEdgeRecord>> {
        let url = format!(
            "{}/api/v1/knowledge/{}/items/{}/edges",
            self.base_url, pack, doc
        );
        let resp = self.get(&url).await?;

        match resp.status() {
            StatusCode::OK => {
                let edges: Vec<KnowledgeDocumentEdgeRecord> = resp.json().await?;
                debug!(pack = %pack, doc = %doc, count = edges.len(), "Listed knowledge document edges");
                Ok(edges)
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
        response: Option<AgentPromptRecord>,
    ) -> Result<Option<AgentPromptRecord>> {
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

    async fn decode_domain_prompt(
        &self,
        response: Option<DomainPromptRecord>,
    ) -> Result<Option<DomainPromptRecord>> {
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
                ApiClientError::Parse(format!("Failed to decrypt domain prompt: {error}"))
            })?
        else {
            return Ok(Some(response));
        };

        response.prompt_config = Some(serde_json::from_str(&plaintext).map_err(|error| {
            ApiClientError::Parse(format!("Failed to parse decrypted domain prompt: {error}"))
        })?);
        response.encrypted_payload = None;
        Ok(Some(response))
    }

    async fn decode_context_block_content(
        &self,
        response: Option<ContextBlockContentRecord>,
    ) -> Result<Option<ContextBlockContentRecord>> {
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
    use anyhow::Result as AnyhowResult;

    #[derive(Debug)]
    struct StaticPayloadCodec {
        plaintext: Option<String>,
    }

    #[async_trait]
    impl PayloadCodec for StaticPayloadCodec {
        async fn decode_text(&self, _payload: &EncryptedPayload) -> AnyhowResult<Option<String>> {
            Ok(self.plaintext.clone())
        }
    }

    fn encrypted_payload(object_id: Uuid) -> EncryptedPayload {
        EncryptedPayload {
            account_id: Uuid::new_v4(),
            encryption_scope: Some("org".to_string()),
            object_id,
            object_type: "project.settings".to_string(),
            algorithm: "AES-256-GCM".to_string(),
            key_version: 1,
            nonce: "nonce".to_string(),
            ciphertext: "ciphertext".to_string(),
        }
    }

    #[test]
    fn test_client_creation() {
        let client = ApiClient::new("http://localhost:8080", "test-key");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_trailing_slash_stripped() {
        let client = ApiClient::new("http://localhost:8080/", "key");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_auth_headers() {
        let client = ApiClient::new("http://localhost", "my-secret");
        let headers = client.auth_headers();
        assert_eq!(
            headers.get("X-API-Key").unwrap().to_str().unwrap(),
            "my-secret"
        );
    }

    #[tokio::test]
    async fn decode_domain_prompt_decrypts_encrypted_payload() {
        let client =
            ApiClient::new("http://localhost", "key").with_payload_codec(StaticPayloadCodec {
                plaintext: Some(
                    serde_json::json!({
                        "developer_prompt_addon": "You are helpful."
                    })
                    .to_string(),
                ),
            });
        let record = DomainPromptRecord {
            domain: DomainRecord {
                id: Uuid::new_v4(),
                org_id: Uuid::new_v4(),
                slug: "eng".to_string(),
                name: "Engineering".to_string(),
                path: "".to_string(),
                description: None,
                command: "bash".to_string(),
                platform_scopes: vec![],
                abilities: vec![],
                mcp_servers: vec![],
                script_tools: vec![],
                source_type: "native".to_string(),
                read_only: false,
                metadata: serde_json::json!({}),
                created_by: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            prompt_config: None,
            encrypted_payload: Some(encrypted_payload(Uuid::new_v4())),
        };

        let decoded = client
            .decode_domain_prompt(Some(record))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            decoded.prompt_config.as_ref().unwrap().developer_prompt_addon.as_deref(),
            Some("You are helpful.")
        );
        assert!(decoded.encrypted_payload.is_none());
    }
}
