//! HTTP client for the platform manifest API.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, StatusCode, Url, header, multipart};
use std::time::Duration;
use uuid::Uuid;

use crate::manifest_contract::{
    AbilityPromptRecord, AgentRecord, ContextBlockContentRecord, DomainPromptRecord, RoutineRecord,
};
use crate::manifest_mcp::{
    AbilityConfigureDocument, AbilityDocument, AgentConfigureDocument, AgentDocument,
    CommandConfigureDocument, ContextBlockConfigureDocument, ContextBlockDocument, CouncilDocument,
    CouncilMemberUpdateDocument, CouncilUpdateDocument, DomainConfigureDocument, DomainDocument,
    KnowledgeDocCreateDocument, KnowledgeDocSummary, KnowledgeDocUpdateDocument,
    KnowledgePackCreateDocument, KnowledgePackDocument, KnowledgePackUpdateDocument,
    ModelCreateDocument, ModelDocument, ModelUpdateDocument, ProjectCreateDocument,
    ProjectDocument, ProjectUpdateDocument, RoutineConfigureDocument, RoutineConfigureMetadata,
    RoutineGraphInput, RoutineStepConfigInput,
};
use crate::types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
use nenjo::Slug;
use nenjo::manifest::{
    CommandManifest, CouncilDelegationStrategy, RoutineEdgeCondition, RoutineMetadata,
};

#[derive(Debug, serde::Deserialize)]
struct CommandConfigureResponse {
    id: Uuid,
    #[serde(flatten)]
    manifest: CommandManifest,
}

/// Thin HTTP client for Nenjo platform manifest endpoints.
#[derive(Debug, Clone)]
pub struct PlatformManifestClient {
    http: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, serde::Serialize)]
struct RoutineConfigureApiBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    routine: Option<&'a Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a RoutineConfigureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_metadata: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_payload: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<ConfigureRoutineGraphApiBody>,
}

#[derive(Debug, serde::Serialize)]
struct ConfigureRoutineGraphApiBody {
    entry_steps: Vec<Slug>,
    steps: Vec<SaveRoutineGraphStepBody>,
    edges: Vec<SaveRoutineGraphEdgeBody>,
}

#[derive(Debug, serde::Serialize)]
struct SaveRoutineGraphStepBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Uuid>,
    slug: Slug,
    name: String,
    step_type: String,
    council: Option<Slug>,
    agent: Option<Slug>,
    config: RoutineStepConfigInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_y: Option<f64>,
    order_index: i32,
}

#[derive(Debug, serde::Serialize)]
struct SaveRoutineGraphEdgeBody {
    source_step: Slug,
    target_step: Slug,
    condition: Option<String>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, serde::Serialize)]
struct SaveRoutineGraphBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<Option<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a RoutineMetadata>,
    entry_steps: Vec<Slug>,
    steps: Vec<SaveRoutineGraphStepBody>,
    edges: Vec<SaveRoutineGraphEdgeBody>,
}

#[derive(Debug, serde::Deserialize)]
struct AuthMeResponse {
    org_id: Option<String>,
}

const PLATFORM_CLIENT_429_RETRY_DELAY: Duration = Duration::from_millis(250);
const PLATFORM_CLIENT_429_MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

trait PlatformRetryRequestBuilderExt {
    /// Send a platform API request with the one transport-level retry policy.
    ///
    /// The platform client retries cloneable requests once when the backend
    /// returns `429 Too Many Requests`. Keep this as the only retry point for
    /// platform manifests so endpoints do not grow competing backoff behavior.
    async fn send_with_platform_retry(self) -> reqwest::Result<reqwest::Response>;
}

impl PlatformRetryRequestBuilderExt for reqwest::RequestBuilder {
    async fn send_with_platform_retry(self) -> reqwest::Result<reqwest::Response> {
        let retry_request = self.try_clone();
        let response = self.send().await?;
        if response.status() != StatusCode::TOO_MANY_REQUESTS {
            return Ok(response);
        }

        let Some(retry_request) = retry_request else {
            return Ok(response);
        };
        let delay = retry_after_delay(response.headers());
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        retry_request.send().await
    }
}

fn retry_after_delay(headers: &header::HeaderMap) -> Duration {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(PLATFORM_CLIENT_429_RETRY_DELAY)
        .min(PLATFORM_CLIENT_429_MAX_RETRY_DELAY)
}

fn routine_graph_body<'a>(
    name: Option<&'a str>,
    description: Option<Option<&'a str>>,
    metadata: Option<&'a RoutineMetadata>,
    graph: &'a RoutineGraphInput,
) -> SaveRoutineGraphBody<'a> {
    SaveRoutineGraphBody {
        name,
        description,
        metadata,
        entry_steps: graph.entry_steps.clone(),
        steps: graph
            .steps
            .iter()
            .map(|step| SaveRoutineGraphStepBody {
                id: step.id,
                slug: step.slug.clone(),
                name: step.name.clone(),
                step_type: step.step_type.to_string(),
                council: step.council.clone(),
                agent: step.agent.clone(),
                config: step.config.clone(),
                encrypted_payload: step.encrypted_payload.clone(),
                position_x: step.position_x,
                position_y: step.position_y,
                order_index: step.order_index,
            })
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|edge| SaveRoutineGraphEdgeBody {
                source_step: edge.source_step.clone(),
                target_step: edge.target_step.clone(),
                condition: Some(
                    match edge.condition {
                        RoutineEdgeCondition::Always => "always",
                        RoutineEdgeCondition::OnPass => "on_pass",
                        RoutineEdgeCondition::OnFail => "on_fail",
                    }
                    .to_string(),
                ),
                metadata: Some(edge.metadata.clone()),
            })
            .collect(),
    }
}

fn routine_configure_graph_body(graph: &RoutineGraphInput) -> ConfigureRoutineGraphApiBody {
    let body = routine_graph_body(None, None, None, graph);
    ConfigureRoutineGraphApiBody {
        entry_steps: body.entry_steps,
        steps: body.steps,
        edges: body.edges,
    }
}

fn response_error_preview(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty response body>".to_string();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
    {
        return message.to_string();
    }
    trimmed.chars().take(1000).collect()
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseRow {
    slug: Slug,
    name: String,
    leader_agent: Slug,
    delegation_strategy: CouncilDelegationStrategy,
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseAgentSummary {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseMemberDetail {
    agent: Slug,
    priority: i32,
    #[serde(default)]
    agent_detail: Option<CouncilResponseAgentSummary>,
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseDetail {
    #[serde(flatten)]
    council: CouncilResponseRow,
    #[serde(default)]
    members: Vec<CouncilResponseMemberDetail>,
}

fn council_document_from_detail(detail: CouncilResponseDetail) -> CouncilDocument {
    CouncilDocument {
        summary: crate::manifest_mcp::CouncilSummary {
            slug: detail.council.slug,
            name: detail.council.name,
            delegation_strategy: detail.council.delegation_strategy,
            leader_agent: detail.council.leader_agent,
        },
        members: detail
            .members
            .into_iter()
            .map(|member| crate::manifest_mcp::CouncilMemberDocument {
                agent_name: member
                    .agent_detail
                    .map(|agent| agent.name)
                    .unwrap_or_else(|| member.agent.to_string()),
                agent: member.agent,
                priority: member.priority,
            })
            .collect(),
    }
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CouncilCreateApiBody {
    pub slug: Slug,
    pub name: String,
    pub description: Option<String>,
    pub leader_agent: Slug,
    pub delegation_strategy: Option<CouncilDelegationStrategy>,
    pub config: Option<serde_json::Value>,
    pub members: Vec<CouncilCreateMemberApiBody>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CouncilCreateMemberApiBody {
    pub agent: Slug,
    pub priority: i32,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
/// Response body for a library knowledge document content read.
pub struct KnowledgeDocContentResponse {
    /// Plaintext document content when the platform can return it directly.
    #[serde(default)]
    pub content: Option<String>,
    /// Stored document filename.
    pub filename: String,
    /// MIME content type for the stored document.
    pub content_type: String,
    /// Stored document content size in bytes.
    pub size_bytes: i64,
    /// Encrypted payload when content is protected outside the platform response body.
    #[serde(default)]
    pub encrypted_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// Metadata for one library knowledge document stored by the platform.
pub struct ProjectDocumentMetadata {
    /// Library knowledge document ID.
    pub id: Uuid,
    /// Owning library pack ID.
    pub project_id: Uuid,
    /// Stable document slug within the pack.
    pub slug: Slug,
    /// Stored document filename.
    pub filename: String,
    /// Optional library-relative path.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional library document kind classifier.
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional short summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// Tags associated with the library document.
    #[serde(default)]
    pub tags: Vec<String>,
    /// MIME content type for the stored document.
    pub content_type: String,
    /// Platform creation timestamp.
    pub created_at: String,
    /// Platform update timestamp.
    pub updated_at: String,
}

pub use crate::manifest_contract::KnowledgeDocumentEdgeRecord as KnowledgeDocEdgeResponse;
pub use crate::manifest_contract::KnowledgeDocumentRecord as KnowledgeDocMetadataResponse;
use crate::manifest_contract::{KnowledgeDocumentRecord, KnowledgePackRecord};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// Directed relationship between two library knowledge documents.
pub struct ProjectDocumentEdge {
    /// Edge ID.
    pub id: Uuid,
    /// Owning library pack ID.
    pub project_id: Uuid,
    /// Source library document ID.
    pub source_document_id: Uuid,
    /// Target library document ID.
    pub target_document_id: Uuid,
    /// Platform edge type classifier.
    pub edge_type: String,
    /// Optional human-readable note for the relationship.
    #[serde(default)]
    pub note: Option<String>,
    /// Platform creation timestamp.
    pub created_at: String,
    /// Platform update timestamp.
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeDocEdgeReplaceItem<'a> {
    pub target_doc: &'a str,
    pub edge_type: &'a str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<&'a str>,
}

/// Cursor-based filters for the organization task catalog.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct TaskListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// Lifecycle subset supported by the execution history API.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionActivityQuery {
    Active,
}

/// Resource ownership subset supported by the execution history API.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionKindQuery {
    Task,
}

/// Filters for task execution history.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct ExecutionListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_slug: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<Slug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<ExecutionActivityQuery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ExecutionKindQuery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

/// Incremental trace filters used by execution watchers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionTraceQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    pub limit: i64,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
/// Query parameters for listing notification sessions.
pub struct NotificationSessionListQuery {
    /// Optional maximum number of sessions to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Optional result offset for pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
/// Query parameters for listing persisted notifications.
pub struct NotificationListQuery {
    /// Optional notification session id filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// Optional maximum number of messages to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Optional pagination cursor timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NotificationMessagePage {
    pub messages: Vec<NotificationMessageRecord>,
    pub has_more: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NotificationMessageRecord {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub user_id: Uuid,
    pub username: String,
    pub sender: String,
    pub content: String,
    pub session_id: Uuid,
    pub created_at: String,
    pub updated_at: String,
    pub metadata: Option<serde_json::Value>,
    pub encrypted_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
/// Query parameters for searching notification recipients.
pub struct NotificationRecipientSearchQuery {
    /// Optional handle or display-name query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Optional maximum number of recipients to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

impl PlatformManifestClient {
    /// Build a client with a freshly constructed `reqwest` client.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build reqwest client")?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        })
    }

    /// Build a client with an externally provided `reqwest` client.
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

    /// Fetch the bootstrap manifest snapshot used to seed a worker's local cache.
    pub async fn fetch_bootstrap(&self) -> Result<BootstrapManifestResponse> {
        let response = self
            .http
            .get(format!("{}/api/v1/manifest", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to fetch manifest bootstrap")?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode manifest bootstrap"),
            status => bail!("manifest bootstrap request failed with status {status}"),
        }
    }

    /// Fetch the full list of agents visible to the API key.
    pub async fn fetch_agents(&self) -> Result<Vec<AgentDocument>> {
        let response = self
            .http
            .get(format!("{}/api/v1/agents", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to fetch agents")?;

        match response.status() {
            StatusCode::OK => response.json().await.context("failed to decode agents"),
            status => bail!("agents request failed with status {status}"),
        }
    }

    /// Fetch one agent document by slug.
    pub async fn fetch_agent_document(&self, agent: &Slug) -> Result<Option<AgentDocument>> {
        let response = self
            .http
            .get(format!("{}/api/v1/agents/{agent}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch agent {agent}"))?;

        match response.status() {
            StatusCode::OK => {
                let record: AgentRecord =
                    response.json().await.context("failed to decode agent")?;
                Ok(Some(record.to_document()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("agent request failed with status {status}"),
        }
    }

    /// Configure an agent in one backend-owned sequence and return the canonical record.
    pub async fn configure_agent_record(
        &self,
        agent: &AgentConfigureDocument,
    ) -> Result<AgentRecord> {
        if agent.prompt_config.is_some() && agent.encrypted_payload.is_none() {
            bail!("agent configure requires encrypted_payload for prompt_config");
        }
        let mut body =
            serde_json::to_value(agent).context("failed to encode agent configure payload")?;
        if agent.encrypted_payload.is_some()
            && let Some(object) = body.as_object_mut()
        {
            object.remove("prompt_config");
        }
        let response = self
            .http
            .post(format!("{}/api/v1/agents/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure agent")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json::<AgentRecord>()
                .await
                .context("failed to decode configured agent"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "agent configure failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Fetch one ability document by slug.
    pub async fn fetch_ability_document(&self, ability: &Slug) -> Result<Option<AbilityDocument>> {
        let selector = ability.as_str();
        let response = self
            .http
            .get(format!("{}/api/v1/abilities/{selector}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch ability {ability}"))?;

        match response.status() {
            StatusCode::OK => {
                let record: AbilityPromptRecord =
                    response.json().await.context("failed to decode ability")?;
                Ok(Some(record.to_document()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("ability request failed with status {status}"),
        }
    }

    /// Configure an ability in one backend-owned sequence and return the canonical document.
    pub async fn configure_ability_document(
        &self,
        ability: &AbilityConfigureDocument,
    ) -> Result<AbilityDocument> {
        if ability.prompt_config.is_some() && ability.encrypted_payload.is_none() {
            bail!("ability configure requires encrypted_payload for prompt_config");
        }
        let mut body =
            serde_json::to_value(ability).context("failed to encode ability configure payload")?;
        if ability.encrypted_payload.is_some()
            && let Some(object) = body.as_object_mut()
        {
            object.remove("prompt_config");
        }
        let response = self
            .http
            .post(format!("{}/api/v1/abilities/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure ability")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let record: AbilityPromptRecord = response
                    .json()
                    .await
                    .context("failed to decode configured ability")?;
                Ok(record.to_document())
            }
            status => bail!("ability configure failed with status {status}"),
        }
    }

    /// Configure a slash command in one backend-owned sequence and return the canonical manifest.
    pub async fn configure_command_document(
        &self,
        command: &CommandConfigureDocument,
    ) -> Result<(Uuid, CommandManifest)> {
        if command.content.is_some() && command.encrypted_payload.is_none() {
            bail!("command configure requires encrypted_payload for content");
        }

        let mut body = serde_json::Map::new();
        if let Some(id) = command.id {
            body.insert("id".to_string(), serde_json::json!(id));
        }
        if let Some(command_ref) = command.command_ref.as_ref() {
            if command_ref.starts_with('/') {
                body.insert("command".to_string(), serde_json::json!(command_ref));
            } else {
                body.insert("name".to_string(), serde_json::json!(command_ref));
            }
        }
        if let Some(metadata) = command.metadata.as_ref() {
            if let Some(name) = metadata.name.as_ref() {
                body.insert("name".to_string(), serde_json::json!(name));
            }
            if let Some(path) = metadata.path.as_ref() {
                body.insert("path".to_string(), serde_json::json!(path));
            }
            if let Some(slash_command) = metadata.command.as_ref() {
                body.insert("command".to_string(), serde_json::json!(slash_command));
            }
            if let Some(description) = metadata.description.as_ref() {
                body.insert("description".to_string(), serde_json::json!(description));
            }
        }
        if let Some(encrypted_payload) = command.encrypted_payload.as_ref() {
            body.insert("encrypted_payload".to_string(), encrypted_payload.clone());
        }

        let response = self
            .http
            .post(format!("{}/api/v1/chat-commands/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure command")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let record = response
                    .json::<CommandConfigureResponse>()
                    .await
                    .context("failed to decode configured command")?;
                Ok((record.id, record.manifest))
            }
            status => bail!("command configure failed with status {status}"),
        }
    }

    /// Configure a domain in one backend-owned sequence and return the canonical record.
    pub async fn configure_domain_record(
        &self,
        domain: &DomainConfigureDocument,
    ) -> Result<DomainPromptRecord> {
        if domain.prompt_config.is_some() && domain.encrypted_payload.is_none() {
            bail!("domain configure requires encrypted_payload for prompt_config");
        }
        let mut body =
            serde_json::to_value(domain).context("failed to encode domain configure payload")?;
        if domain.encrypted_payload.is_some()
            && let Some(object) = body.as_object_mut()
        {
            object.remove("prompt_config");
        }
        let response = self
            .http
            .post(format!("{}/api/v1/domains/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure domain")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json::<DomainPromptRecord>()
                .await
                .context("failed to decode configured domain"),
            status => bail!("domain configure failed with status {status}"),
        }
    }

    /// Configure a domain in one backend-owned sequence and return the canonical document.
    pub async fn configure_domain_document(
        &self,
        domain: &DomainConfigureDocument,
    ) -> Result<DomainDocument> {
        Ok(self.configure_domain_record(domain).await?.to_document())
    }

    /// Fetch one domain record by slug.
    pub async fn fetch_domain_record(&self, domain: &Slug) -> Result<Option<DomainPromptRecord>> {
        let selector = domain.as_str();
        let response = self
            .http
            .get(format!("{}/api/v1/domains/{selector}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch domain {domain}"))?;

        match response.status() {
            StatusCode::OK => response
                .json::<DomainPromptRecord>()
                .await
                .map(Some)
                .context("failed to decode domain"),
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("domain request failed with status {status}"),
        }
    }

    /// Fetch one domain document by slug.
    pub async fn fetch_domain_document(&self, domain: &Slug) -> Result<Option<DomainDocument>> {
        Ok(self
            .fetch_domain_record(domain)
            .await?
            .map(|record| record.to_document()))
    }

    /// Create a project manifest resource.
    pub async fn create_project_document(
        &self,
        project: &ProjectCreateDocument,
        id: Option<Uuid>,
    ) -> Result<ProjectDocument> {
        let mut body =
            serde_json::to_value(project).context("failed to encode project create payload")?;
        if let Some(id) = id {
            body["id"] = serde_json::to_value(id)?;
        }
        let response = self
            .http
            .post(format!("{}/api/v1/projects", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to create project")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created project"),
            status => bail!("project create failed with status {status}"),
        }
    }

    /// Apply a partial metadata update to a project manifest resource.
    pub async fn update_project_document(
        &self,
        project_ref: &Slug,
        project: &ProjectUpdateDocument,
    ) -> Result<ProjectDocument> {
        let mut body = serde_json::Map::new();
        if let Some(name) = &project.name {
            body.insert("name".into(), serde_json::json!(name));
        }
        if let Some(slug) = &project.slug {
            body.insert("slug".into(), serde_json::json!(slug));
        }
        if let Some(description) = &project.description {
            body.insert("description".into(), serde_json::json!(description));
        }
        if let Some(repo_url) = &project.repo_url {
            let settings = match repo_url {
                Some(url) => serde_json::json!({ "repo_url": url }),
                None => serde_json::json!({}),
            };
            body.insert("settings".into(), settings);
        }
        let response = self
            .http
            .patch(format!("{}/api/v1/projects/{project_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::Value::Object(body))
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to update project {project_ref}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated project"),
            status => bail!("project update failed with status {status}"),
        }
    }

    /// Delete a project manifest resource by slug.
    pub async fn delete_project_document(&self, project: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/projects/{project}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete project {project}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("project delete failed with status {status}"),
        }
    }

    /// Create a library knowledge document, optionally sending encrypted content payload.
    pub async fn create_knowledge_doc(
        &self,
        pack: &Slug,
        doc_id: Uuid,
        item: &KnowledgeDocCreateDocument,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<KnowledgeDocSummary> {
        if encrypted_payload.is_none() {
            bail!("library knowledge document create requires encrypted_payload for content");
        }
        let content_type = item
            .content_type
            .clone()
            .unwrap_or_else(|| "text/plain".to_string());
        let mut form = multipart::Form::new();
        let placeholder = multipart::Part::bytes(vec![0_u8])
            .file_name(item.filename.clone())
            .mime_str(&content_type)
            .context("failed to build encrypted library knowledge document placeholder file")?;
        form = form.part("file", placeholder);
        form = form.text("item_id", doc_id.to_string());
        form = form.text("filename", item.filename.clone());
        form = form.text("content_type", content_type);
        if let Some(path) = item.path.as_deref() {
            form = form.text("path", path.to_string());
        }
        if let Some(title) = item.title.as_deref() {
            form = form.text("title", title.to_string());
        }
        if let Some(kind) = item.kind.as_deref() {
            form = form.text("kind", kind.to_string());
        }
        if let Some(summary) = item.summary.as_deref() {
            form = form.text("summary", summary.to_string());
        }
        if !item.tags.is_empty() {
            form = form.text(
                "tags",
                serde_json::to_string(&item.tags)
                    .context("failed to encode library knowledge document tags")?,
            );
        }
        if let Some(encrypted_payload) = encrypted_payload {
            form = form.text(
                "encrypted_payload",
                serde_json::to_string(&encrypted_payload)
                    .context("failed to encode encrypted library knowledge document payload")?,
            );
        }

        let response = self
            .http
            .post(format!("{}/api/v1/knowledge/{pack}/items", self.base_url))
            .header("X-API-Key", &self.api_key)
            .multipart(form)
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!(
                    "failed to create library knowledge document {} in pack {}",
                    item.filename, pack
                )
            })?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let document: KnowledgeDocMetadataResponse = response
                    .json()
                    .await
                    .context("failed to decode created library knowledge document")?;
                Ok(knowledge_doc_summary(pack, document))
            }
            StatusCode::CONFLICT => {
                let body = response.text().await.unwrap_or_default();
                if body.trim().is_empty() {
                    bail!(
                        "library knowledge document create conflicted in pack {pack}; search_knowledge for the filename/title to find the existing document.slug, then call update_knowledge_doc with slug instead of create_knowledge_doc"
                    )
                } else {
                    bail!(
                        "library knowledge document create conflicted in pack {pack}: {}; search_knowledge for the filename/title to find the existing document.slug, then call update_knowledge_doc with slug instead of create_knowledge_doc",
                        body.trim()
                    )
                }
            }
            _ => Err(self
                .response_error(response, "library knowledge document create")
                .await),
        }
    }

    /// Update metadata for an existing library knowledge document.
    pub async fn update_knowledge_doc_metadata(
        &self,
        pack: &Slug,
        doc: &Slug,
        item: &KnowledgeDocUpdateDocument,
    ) -> Result<KnowledgeDocSummary> {
        let mut body = serde_json::to_value(item)
            .context("failed to encode library knowledge document metadata update")?;
        if let Some(object) = body.as_object_mut() {
            object.remove("content");
            object.remove("related");
        }

        let response = self
            .http
            .patch(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to update metadata for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK => {
                let document: KnowledgeDocMetadataResponse = response
                    .json()
                    .await
                    .context("failed to decode updated library knowledge document metadata")?;
                Ok(knowledge_doc_summary(pack, document))
            }
            _ => Err(self
                .response_error(response, "library knowledge document metadata update")
                .await),
        }
    }

    /// Update the content for an existing library knowledge document.
    pub async fn update_knowledge_doc_content(
        &self,
        pack: &Slug,
        doc: &Slug,
        _content: &str,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<KnowledgeDocSummary> {
        let encrypted_payload = encrypted_payload.ok_or_else(|| {
            anyhow::anyhow!("library knowledge document content update requires encrypted_payload")
        })?;
        let body = serde_json::json!({ "encrypted_payload": encrypted_payload });

        let response = self
            .http
            .put(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to update content for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK => {
                let document: KnowledgeDocMetadataResponse = response
                    .json()
                    .await
                    .context("failed to decode updated library knowledge document")?;
                Ok(knowledge_doc_summary(pack, document))
            }
            _ => Err(self
                .response_error(response, "library knowledge document content update")
                .await),
        }
    }

    /// List graph edges connected to a library knowledge document.
    pub async fn list_knowledge_doc_edges(
        &self,
        pack: &Slug,
        doc: &Slug,
    ) -> Result<Vec<KnowledgeDocEdgeResponse>> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/edges",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to list edges for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode library knowledge document edges"),
            _ => Err(self
                .response_error(response, "library knowledge document edge list")
                .await),
        }
    }

    /// Create an outbound graph edge for a library knowledge document.
    pub async fn create_knowledge_doc_edge(
        &self,
        pack: &Slug,
        doc: &Slug,
        target_doc: &Slug,
        edge_type: &str,
        note: Option<&str>,
    ) -> Result<KnowledgeDocEdgeResponse> {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/edges",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({
                "target_doc": target_doc,
                "edge_type": edge_type,
                "note": note,
            }))
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to create edge for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created library knowledge document edge"),
            _ => Err(self
                .response_error(response, "library knowledge document edge create")
                .await),
        }
    }

    /// Replace all outbound graph edges for a library knowledge document.
    pub async fn replace_knowledge_doc_edges(
        &self,
        pack: &Slug,
        doc: &Slug,
        related: &[KnowledgeDocEdgeReplaceItem<'_>],
    ) -> Result<Vec<KnowledgeDocEdgeResponse>> {
        let response = self
            .http
            .put(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/edges",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({ "related": related }))
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to replace edges for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode replaced library knowledge document edges"),
            _ => Err(self
                .response_error(response, "library knowledge document edge replace")
                .await),
        }
    }

    /// Delete a graph edge connected to a library knowledge document.
    pub async fn delete_knowledge_doc_edge(
        &self,
        pack: &Slug,
        doc: &Slug,
        edge_id: Uuid,
    ) -> Result<()> {
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/edges/{edge_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| {
                format!("failed to delete edge {edge_id} for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            _ => Err(self
                .response_error(response, "library knowledge document edge delete")
                .await),
        }
    }

    /// Delete a library knowledge document by pack and item ID.
    pub async fn delete_knowledge_doc(&self, pack: &Slug, doc: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete library knowledge document {doc}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            _ => Err(self
                .response_error(response, "library knowledge document delete")
                .await),
        }
    }

    /// Fetch the current organization ID associated with the API key.
    pub async fn current_org_id(&self) -> Result<Uuid> {
        let response = self
            .http
            .get(format!("{}/api/v1/auth/me", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to fetch authenticated org context")?;

        match response.status() {
            StatusCode::OK => {
                let body: AuthMeResponse = response
                    .json()
                    .await
                    .context("failed to decode authenticated org context")?;
                let raw_org_id = body
                    .org_id
                    .filter(|value| !value.trim().is_empty())
                    .context("authenticated org context did not include org_id")?;
                Uuid::parse_str(&raw_org_id).context("authenticated org_id was not a valid UUID")
            }
            status => bail!("fetch authenticated org context failed with status {status}"),
        }
    }

    /// List notification sessions visible to the authenticated account.
    pub async fn list_notification_sessions(
        &self,
        query: &NotificationSessionListQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!("{}/api/v1/notifications/sessions", self.base_url))
            .context("failed to build notification session list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(offset) = query.offset {
                pairs.append_pair("offset", &offset.to_string());
            }
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list notification sessions")?;

        match response.status() {
            StatusCode::OK => response
                .json::<serde_json::Value>()
                .await
                .context("failed to decode notification sessions"),
            status => bail!("notification session list failed with status {status}"),
        }
    }

    /// List persisted notification messages visible to the authenticated account.
    pub async fn list_notifications(
        &self,
        query: &NotificationListQuery,
    ) -> Result<NotificationMessagePage> {
        let mut url = Url::parse(&format!("{}/api/v1/notifications", self.base_url))
            .context("failed to build notification list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(session_id) = query.session_id {
                pairs.append_pair("session_id", &session_id.to_string());
            }
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(before) = query.before.as_ref() {
                pairs.append_pair("before", before);
            }
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list notifications")?;

        match response.status() {
            StatusCode::OK => response
                .json::<NotificationMessagePage>()
                .await
                .context("failed to decode notification list results"),
            status => bail!("notification list failed with status {status}"),
        }
    }

    /// Search notification recipients visible to the authenticated account.
    pub async fn search_notification_recipients(
        &self,
        query: &NotificationRecipientSearchQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!(
            "{}/api/v1/notifications/recipients",
            self.base_url
        ))
        .context("failed to build notification recipient search URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(query) = query.query.as_ref() {
                pairs.append_pair("q", query);
            }
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to search notification recipients")?;

        match response.status() {
            StatusCode::OK => response
                .json::<serde_json::Value>()
                .await
                .context("failed to decode notification recipients"),
            status => bail!("notification recipient search failed with status {status}"),
        }
    }

    /// List tasks using the canonical organization task API.
    pub async fn list_tasks(&self, query: &TaskListQuery) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!("{}/api/v1/tasks", self.base_url))
            .context("failed to build task list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(value) = query.project.as_ref() {
                pairs.append_pair("project", value.as_str());
            }
            if let Some(value) = query.status.as_ref() {
                pairs.append_pair("status", value);
            }
            if let Some(value) = query.label.as_ref() {
                pairs.append_pair("label", value);
            }
            if let Some(value) = query.agent.as_ref() {
                pairs.append_pair("agent", value.as_str());
            }
            if let Some(value) = query.routine.as_ref() {
                pairs.append_pair("routine", value.as_str());
            }
            if let Some(value) = query.cursor_updated_at.as_ref() {
                pairs.append_pair("cursor_updated_at", value);
            }
            if let Some(value) = query.cursor_id {
                pairs.append_pair("cursor_id", &value.to_string());
            }
            if let Some(value) = query.limit {
                pairs.append_pair("limit", &value.to_string());
            }
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list tasks")?;
        match response.status() {
            StatusCode::OK => response.json().await.context("failed to decode task list"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "task list failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// List the organization task-label catalog.
    pub async fn list_task_labels(&self) -> Result<serde_json::Value> {
        let response = self
            .http
            .get(format!("{}/api/v1/labels", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list task labels")?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode task labels"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "task label list failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Fetch one organization task by UUID or slug.
    pub async fn get_task(&self, task_ref: &str) -> Result<Option<serde_json::Value>> {
        let response = self
            .http
            .get(format!("{}/api/v1/tasks/{task_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch task {task_ref}"))?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .map(Some)
                .context("failed to decode task"),
            StatusCode::NOT_FOUND => Ok(None),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "task fetch failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Create or update one task through the backend-owned slug resolution flow.
    pub async fn configure_task(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!("{}/api/v1/tasks/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send_with_platform_retry()
            .await
            .context("failed to configure task")?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode configured task"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "task configure failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Delete one organization task by its stable ID.
    pub async fn delete_task(&self, task_id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/tasks/{task_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete task {task_id}"))?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("task delete failed with status {status}"),
        }
    }

    /// Dispatch one manual task execution and return its run immediately.
    pub async fn dispatch_task(
        &self,
        task_id: Uuid,
        idempotency_key: Uuid,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!("{}/api/v1/tasks/{task_id}/execute", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({ "idempotency_key": idempotency_key }))
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to dispatch task {task_id}"))?;
        match response.status() {
            StatusCode::ACCEPTED => response
                .json()
                .await
                .context("failed to decode dispatched task execution"),
            status => bail!("task dispatch failed with status {status}"),
        }
    }

    /// List task execution runs across the organization.
    pub async fn list_execution_runs(
        &self,
        query: &ExecutionListQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!("{}/api/v1/executions", self.base_url))
            .context("failed to build execution list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(value) = query.task_slug.as_ref() {
                pairs.append_pair("task_slug", value.as_str());
            }
            if let Some(value) = query.project.as_ref() {
                pairs.append_pair("project", value.as_str());
            }
            if let Some(value) = query.agent.as_ref() {
                pairs.append_pair("agent", value.as_str());
            }
            if let Some(value) = query.routine.as_ref() {
                pairs.append_pair("routine", value.as_str());
            }
            if let Some(value) = query.status.as_ref() {
                pairs.append_pair("status", value);
            }
            if let Some(ExecutionActivityQuery::Active) = query.activity {
                pairs.append_pair("activity", "active");
            }
            if let Some(ExecutionKindQuery::Task) = query.kind {
                pairs.append_pair("kind", "task");
            }
            if let Some(value) = query.limit {
                pairs.append_pair("limit", &value.to_string());
            }
            if let Some(value) = query.offset {
                pairs.append_pair("offset", &value.to_string());
            }
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list task execution runs")?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode task execution runs"),
            status => bail!("task execution list failed with status {status}"),
        }
    }

    /// Fetch one task-backed execution run.
    pub async fn get_execution_run(
        &self,
        execution_run_id: Uuid,
    ) -> Result<Option<serde_json::Value>> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/executions/{execution_run_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch execution run {execution_run_id}"))?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .map(Some)
                .context("failed to decode execution run"),
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("execution run fetch failed with status {status}"),
        }
    }

    /// Fetch the routine task/step projection for an execution run.
    pub async fn get_execution_tasks(&self, execution_run_id: Uuid) -> Result<serde_json::Value> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/executions/{execution_run_id}/tasks",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch execution tasks for {execution_run_id}"))?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode execution tasks"),
            status => bail!("execution task activity failed with status {status}"),
        }
    }

    /// Fetch a bounded incremental page of persisted execution traces.
    pub async fn get_execution_trace_events(
        &self,
        execution_run_id: Uuid,
        query: &ExecutionTraceQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!(
            "{}/api/v1/executions/{execution_run_id}/trace-events",
            self.base_url
        ))
        .context("failed to build execution trace URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(after) = query.after {
                pairs.append_pair("after", &after.to_string());
            }
            pairs.append_pair("limit", &query.limit.to_string());
        }
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch execution trace for {execution_run_id}"))?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode execution trace"),
            status => bail!("execution trace failed with status {status}"),
        }
    }

    /// List attachment metadata produced by a task's executions.
    pub async fn list_task_attachments(&self, task_id: Uuid) -> Result<serde_json::Value> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/tasks/{task_id}/attachments",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to list attachments for task {task_id}"))?;
        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode task attachments"),
            status => bail!("task attachment list failed with status {status}"),
        }
    }

    /// Request cancellation of one queued or running task execution.
    pub async fn cancel_execution_run(&self, execution_run_id: Uuid) -> Result<serde_json::Value> {
        self.task_execution_command(execution_run_id, "cancel", None)
            .await
    }

    /// Retry one terminal task execution and return the new run.
    pub async fn retry_execution_run(
        &self,
        execution_run_id: Uuid,
        idempotency_key: Uuid,
    ) -> Result<serde_json::Value> {
        self.task_execution_command(execution_run_id, "retry", Some(idempotency_key))
            .await
    }

    async fn task_execution_command(
        &self,
        execution_run_id: Uuid,
        command: &str,
        idempotency_key: Option<Uuid>,
    ) -> Result<serde_json::Value> {
        let request = self
            .http
            .post(format!(
                "{}/api/v1/executions/{execution_run_id}/{command}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key);
        let request = match idempotency_key {
            Some(key) => request.json(&serde_json::json!({ "idempotency_key": key })),
            None => request,
        };
        let response = request
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to {command} task execution {execution_run_id}"))?;
        match response.status() {
            StatusCode::ACCEPTED => response
                .json()
                .await
                .with_context(|| format!("failed to decode {command} execution response")),
            status => bail!("task execution {command} failed with status {status}"),
        }
    }

    /// Configure a routine in one backend-owned sequence and return the canonical record.
    pub(crate) async fn configure_routine_record(
        &self,
        routine: &RoutineConfigureDocument,
    ) -> Result<RoutineRecord> {
        let body = RoutineConfigureApiBody {
            id: routine.id,
            routine: routine.routine.as_ref(),
            metadata: routine.metadata.as_ref(),
            runtime_metadata: routine.runtime_metadata.as_ref(),
            encrypted_payload: routine.encrypted_payload.as_ref(),
            graph: routine.graph.as_ref().map(routine_configure_graph_body),
        };
        let response = self
            .http
            .post(format!("{}/api/v1/routines/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure routine")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json::<RoutineRecord>()
                .await
                .context("failed to decode configured routine"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "routine configure failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Get a routine record by slug or UUID, returning `None` only when the platform
    /// confirms it does not exist.
    pub(crate) async fn get_routine_record_optional(
        &self,
        routine_ref: &Slug,
    ) -> Result<Option<RoutineRecord>> {
        let response = self
            .http
            .get(format!("{}/api/v1/routines/{routine_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to get routine")?;

        match response.status() {
            StatusCode::OK => response
                .json::<RoutineRecord>()
                .await
                .map(Some)
                .context("failed to decode routine"),
            StatusCode::NOT_FOUND => Ok(None),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "routine get failed with status {status}: {}",
                    response_error_preview(&body)
                )
            }
        }
    }

    /// Delete a routine document by slug.
    pub async fn delete_routine_document(&self, routine_ref: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/routines/{routine_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete routine {routine_ref}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("routine delete failed with status {status}"),
        }
    }

    /// Create a model document.
    pub async fn create_model_document(
        &self,
        model: &ModelCreateDocument,
    ) -> Result<ModelDocument> {
        let response = self
            .http
            .post(format!("{}/api/v1/models", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(model)
            .send_with_platform_retry()
            .await
            .context("failed to create model")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created model"),
            status => {
                let body = response.text().await.unwrap_or_default();
                bail!("model create failed with status {status}: {body}")
            }
        }
    }

    /// Apply a partial update to a model document.
    pub async fn update_model_document(
        &self,
        model_ref: &Slug,
        model: &ModelUpdateDocument,
    ) -> Result<ModelDocument> {
        let body = serde_json::to_value(model).context("failed to encode model update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/models/{model_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to update model {model_ref}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated model"),
            status => bail!("model update failed with status {status}"),
        }
    }

    /// Delete a model document by slug.
    pub async fn delete_model_document(&self, model: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/models/{model}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete model {model}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("model delete failed with status {status}"),
        }
    }

    async fn fetch_council_document(&self, council_ref: &Slug) -> Result<CouncilDocument> {
        Ok(council_document_from_detail(
            self.fetch_council_detail(council_ref).await?,
        ))
    }

    async fn fetch_council_detail(&self, council_ref: &Slug) -> Result<CouncilResponseDetail> {
        let response = self
            .http
            .get(format!("{}/api/v1/councils/{council_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch council {council_ref}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode council detail"),
            status => bail!("council detail request failed with status {status}"),
        }
    }

    /// Create a council document.
    pub(crate) async fn create_council_document(
        &self,
        council: &CouncilCreateApiBody,
    ) -> Result<CouncilDocument> {
        let body =
            serde_json::to_value(council).context("failed to encode council create payload")?;
        let response = self
            .http
            .post(format!("{}/api/v1/councils", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to create council")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let detail: CouncilResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode created council")?;
                Ok(council_document_from_detail(detail))
            }
            status => bail!("council create failed with status {status}"),
        }
    }

    /// Apply a partial update to a council document.
    pub async fn update_council_document(
        &self,
        council_ref: &Slug,
        council: &CouncilUpdateDocument,
    ) -> Result<CouncilDocument> {
        let body =
            serde_json::to_value(council).context("failed to encode council update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/councils/{council_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to update council {council_ref}"))?;

        match response.status() {
            StatusCode::OK => {
                let detail: CouncilResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode updated council")?;
                Ok(council_document_from_detail(detail))
            }
            status => bail!("council update failed with status {status}"),
        }
    }

    /// Delete a council document by slug.
    pub async fn delete_council_document(&self, council_ref: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/councils/{council_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to delete council {council_ref}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("council delete failed with status {status}"),
        }
    }

    /// Add an agent member to a council.
    pub(crate) async fn add_council_member_document(
        &self,
        council_ref: &Slug,
        member: &CouncilCreateMemberApiBody,
    ) -> Result<CouncilDocument> {
        let body = serde_json::to_value(member)
            .context("failed to encode council member create payload")?;
        let response = self
            .http
            .post(format!(
                "{}/api/v1/councils/{council_ref}/members",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to add member to council {council_ref}"))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let detail: CouncilResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode updated council after add member")?;
                Ok(council_document_from_detail(detail))
            }
            status => bail!("council add member failed with status {status}"),
        }
    }

    /// Update a council member by council slug and agent slug.
    pub async fn update_council_member_document(
        &self,
        council_ref: &Slug,
        agent: &Slug,
        member: &CouncilMemberUpdateDocument,
    ) -> Result<CouncilDocument> {
        let body = serde_json::to_value(member)
            .context("failed to encode council member update payload")?;
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/councils/{council_ref}/members/{agent}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to update member in council {council_ref}"))?;

        match response.status() {
            StatusCode::OK => {
                let detail: CouncilResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode updated council after update member")?;
                Ok(council_document_from_detail(detail))
            }
            status => bail!("council update member failed with status {status}"),
        }
    }

    /// Remove an agent member from a council.
    pub async fn remove_council_member_document(
        &self,
        council_ref: &Slug,
        agent: &Slug,
    ) -> Result<CouncilDocument> {
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/councils/{council_ref}/members/{agent}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to remove member from council {council_ref}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => self.fetch_council_document(council_ref).await,
            status => bail!("council remove member failed with status {status}"),
        }
    }

    /// Configure a context block in one backend-owned sequence and return the canonical document.
    pub async fn configure_context_block_document(
        &self,
        context_block: &ContextBlockConfigureDocument,
    ) -> Result<ContextBlockDocument> {
        if context_block.template.is_some() && context_block.encrypted_payload.is_none() {
            bail!("context block configure requires encrypted_payload for template");
        }
        let mut body = serde_json::to_value(context_block)
            .context("failed to encode context block configure payload")?;
        if context_block.encrypted_payload.is_some()
            && let Some(object) = body.as_object_mut()
        {
            object.remove("template");
        }
        let response = self
            .http
            .post(format!("{}/api/v1/context-blocks/configure", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send_with_platform_retry()
            .await
            .context("failed to configure context block")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let record: ContextBlockContentRecord = response
                    .json()
                    .await
                    .context("failed to decode configured context block")?;
                Ok(record.to_document())
            }
            status => bail!("context block configure failed with status {status}"),
        }
    }

    /// Fetch one generic manifest resource by platform resource type and ID.
    pub async fn fetch_resource(
        &self,
        path: &str,
        id: Uuid,
    ) -> Result<Option<PlatformManifestItem>> {
        let response = self
            .http
            .get(format!(
                "{}/{}/{}",
                self.base_url,
                path.trim_start_matches('/'),
                id
            ))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to fetch manifest resource {path}{id}"))?;

        match response.status() {
            StatusCode::OK => {
                let payload = response
                    .json::<serde_json::Value>()
                    .await
                    .context("failed to decode manifest resource")?;
                Ok(Some(PlatformManifestItem { id, payload }))
            }
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("manifest resource request failed with status {status}"),
        }
    }

    /// Upsert one generic manifest resource through the platform write API.
    pub async fn upsert_resource(
        &self,
        request: &PlatformManifestWriteRequest,
    ) -> Result<PlatformManifestItem> {
        bail!(
            "platform manifest writes are not implemented yet for {} {}",
            request.resource_type,
            request.resource_id
        )
    }

    /// Delete one generic manifest resource through the platform write API.
    pub async fn delete_resource(&self, _resource_type: &str, _resource_id: Uuid) -> Result<()> {
        bail!("platform manifest deletes are not implemented yet")
    }

    /// Build the API-key authorization header value used by REST-backed tools.
    pub fn auth_header(&self) -> Result<header::HeaderValue> {
        header::HeaderValue::from_str(&self.api_key).context("invalid api key header")
    }

    async fn response_error(
        &self,
        response: reqwest::Response,
        operation: impl AsRef<str>,
    ) -> anyhow::Error {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if body.trim().is_empty() {
            anyhow!("{} failed with status {status}", operation.as_ref())
        } else {
            anyhow!(
                "{} failed with status {status}: {}",
                operation.as_ref(),
                body.trim()
            )
        }
    }

    pub async fn list_knowledge_packs(&self) -> Result<Vec<KnowledgePackRecord>> {
        let response = self
            .http
            .get(format!("{}/api/v1/knowledge", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .context("failed to list knowledge packs")?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode knowledge packs"),
            status => bail!("knowledge pack list failed with status {status}"),
        }
    }

    /// Create a user-managed Library knowledge pack.
    pub async fn create_knowledge_pack(
        &self,
        pack: &KnowledgePackCreateDocument,
    ) -> Result<KnowledgePackDocument> {
        let response = self
            .http
            .post(format!("{}/api/v1/knowledge", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(pack)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to create knowledge pack {}", pack.name))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .map(knowledge_pack_document)
                .context("failed to decode created knowledge pack"),
            status => bail!("knowledge pack create failed with status {status}"),
        }
    }

    /// Update a user-managed Library knowledge pack.
    pub async fn update_knowledge_pack(
        &self,
        pack: &Slug,
        update: &KnowledgePackUpdateDocument,
    ) -> Result<KnowledgePackDocument> {
        let response = self
            .http
            .patch(format!("{}/api/v1/knowledge/{pack}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(update)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to update knowledge pack {pack}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .map(knowledge_pack_document)
                .context("failed to decode updated knowledge pack"),
            status => bail!("knowledge pack update failed with status {status}"),
        }
    }

    pub async fn resolve_knowledge_pack_slug(&self, pack: &Slug) -> Result<Uuid> {
        self.list_knowledge_packs()
            .await?
            .into_iter()
            .find(|candidate| Slug::derive(&candidate.slug) == *pack)
            .map(|candidate| candidate.id)
            .ok_or_else(|| anyhow!("knowledge pack not found: {pack}"))
    }

    pub async fn list_knowledge_doc_metadata(
        &self,
        pack: &Slug,
    ) -> Result<Vec<KnowledgeDocMetadataResponse>> {
        let response = self
            .http
            .get(format!("{}/api/v1/knowledge/{pack}/items", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_with_platform_retry()
            .await
            .with_context(|| format!("failed to list knowledge documents for pack {pack}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode knowledge document metadata"),
            status => bail!("knowledge document list failed with status {status}"),
        }
    }

    pub async fn resolve_knowledge_doc_slug(&self, pack: &Slug, doc: &Slug) -> Result<Uuid> {
        self.list_knowledge_doc_metadata(pack)
            .await?
            .into_iter()
            .find(|candidate| Slug::derive(&candidate.slug) == *doc)
            .map(|candidate| candidate.id)
            .ok_or_else(|| anyhow!("knowledge document not found in pack {pack}: {doc}"))
    }

    /// Resolve a document reference from slug, selector, filename, or search metadata path.
    pub async fn resolve_knowledge_doc_reference(
        &self,
        pack: &Slug,
        reference: &str,
    ) -> Result<Slug> {
        let reference = reference.trim();
        if reference.is_empty() {
            bail!("knowledge document reference cannot be empty");
        }

        if let Ok(slug) = Slug::parse(reference) {
            return Ok(slug);
        }

        let docs = self.list_knowledge_doc_metadata(pack).await?;
        if let Some(doc) = docs.iter().find(|candidate| candidate.slug == reference) {
            return Ok(Slug::derive(&doc.slug));
        }

        let derived = Slug::derive(reference);
        if let Some(doc) = docs
            .iter()
            .find(|candidate| Slug::derive(&candidate.slug) == derived)
        {
            return Ok(Slug::derive(&doc.slug));
        }

        let path_candidates = knowledge_doc_reference_paths(reference);
        if let Some(doc) = docs.iter().find(|candidate| {
            let relative = candidate.library_doc_relative_path();
            path_candidates.iter().any(|path| path == &relative)
                || candidate.filename == reference
                || candidate.library_selector(pack.as_str()) == reference
        }) {
            return Ok(Slug::derive(&doc.slug));
        }

        bail!("knowledge document not found in pack {pack}: {reference}")
    }
}

fn knowledge_doc_reference_paths(reference: &str) -> Vec<String> {
    let mut paths = vec![reference.trim_matches('/').to_string()];
    if reference.contains('.')
        && !reference.contains('/')
        && let Some((stem, extension)) = reference.rsplit_once('.')
        && (extension == "md" || extension == "markdown")
    {
        paths.push(format!("{}.md", stem.replace('.', "/")));
        paths.push(stem.replace('.', "/"));
    }
    paths
}

fn knowledge_doc_summary(pack: &Slug, document: KnowledgeDocumentRecord) -> KnowledgeDocSummary {
    let updated_at = document.updated_at_rfc3339();
    KnowledgeDocSummary {
        pack: pack.clone(),
        slug: Slug::derive(&document.slug),
        filename: document.filename,
        path: document.path,
        title: document.title,
        kind: document.kind,
        summary: document.summary,
        tags: document.tags,
        content_type: document.content_type,
        updated_at,
    }
}

fn knowledge_pack_document(pack: KnowledgePackRecord) -> KnowledgePackDocument {
    KnowledgePackDocument {
        slug: Slug::derive(&pack.slug),
        name: pack.name,
        description: pack.description,
        source_type: pack.source_type,
        read_only: pack.read_only,
        selector: pack.selector,
        version: pack.version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest_mcp::{RoutineEdgeInput, RoutineStepInput};
    use nenjo::manifest::RoutineStepType;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn response_error_preview_extracts_the_platform_message() {
        assert_eq!(
            response_error_preview(
                r#"{"error":{"code":"validation_error","message":"routine not found: missing-pipeline"}}"#,
            ),
            "routine not found: missing-pipeline"
        );
    }

    #[test]
    fn routine_graph_body_uses_step_slugs_for_platform_refs() {
        let graph = RoutineGraphInput {
            entry_steps: vec![Slug::derive("implement_pr_changes")],
            steps: vec![
                RoutineStepInput {
                    id: None,
                    slug: Slug::derive("implement_pr_changes"),
                    name: "Implement PR changes".to_string(),
                    step_type: RoutineStepType::Agent,
                    council: None,
                    agent: Some(Slug::derive("coder")),
                    config: RoutineStepConfigInput::default(),
                    encrypted_payload: None,
                    position_x: None,
                    position_y: None,
                    order_index: 0,
                },
                RoutineStepInput {
                    id: None,
                    slug: Slug::derive("evaluate_result"),
                    name: "Evaluate result".to_string(),
                    step_type: RoutineStepType::Gate,
                    council: None,
                    agent: Some(Slug::derive("security")),
                    config: RoutineStepConfigInput::default(),
                    encrypted_payload: None,
                    position_x: None,
                    position_y: None,
                    order_index: 1,
                },
            ],
            edges: vec![RoutineEdgeInput {
                source_step: Slug::derive("implement_pr_changes"),
                target_step: Slug::derive("evaluate_result"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            }],
        };

        let body = routine_graph_body(None, None, None, &graph);

        assert_eq!(body.entry_steps, vec![Slug::derive("implement_pr_changes")]);
        assert_eq!(
            body.edges[0].source_step,
            Slug::derive("implement_pr_changes")
        );
        assert_eq!(body.edges[0].target_step, Slug::derive("evaluate_result"));
        assert_eq!(body.steps[0].position_x, None);
        assert_eq!(body.steps[0].position_y, None);
    }

    #[test]
    fn routine_graph_body_preserves_explicit_step_positions() {
        let graph = RoutineGraphInput {
            entry_steps: vec![Slug::derive("implement_pr_changes")],
            steps: vec![RoutineStepInput {
                id: None,
                slug: Slug::derive("implement_pr_changes"),
                name: "Implement PR changes".to_string(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: RoutineStepConfigInput::default(),
                encrypted_payload: None,
                position_x: Some(42.0),
                position_y: Some(99.0),
                order_index: 0,
            }],
            edges: Vec::new(),
        };

        let body = routine_graph_body(None, None, None, &graph);

        assert_eq!(body.steps[0].position_x, Some(42.0));
        assert_eq!(body.steps[0].position_y, Some(99.0));
    }

    #[tokio::test]
    async fn platform_client_retries_cloneable_request_once_after_429() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("read test server address");

        let server = thread::spawn(move || {
            let mut request_lines = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let mut buffer = [0_u8; 4096];
                let bytes = stream.read(&mut buffer).expect("read test request");
                let request = String::from_utf8_lossy(&buffer[..bytes]);
                request_lines.push(request.lines().next().unwrap_or_default().to_string());

                let response = if attempt == 0 {
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    let body = r#"[{"id":"00000000-0000-0000-0000-000000000001","slug":"humanizer","name":"Humanizer","description":null,"source_type":"uploaded","read_only":false,"metadata":{},"selector":"lib:humanizer","version":null,"created_at":"2026-06-11T00:00:00Z","updated_at":"2026-06-11T00:00:00Z"}]"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                stream
                    .write_all(response.as_bytes())
                    .expect("write test response");
            }
            request_lines
        });

        let client = PlatformManifestClient::new(format!("http://{addr}"), "test-key")
            .expect("build platform client");
        let packs = client
            .list_knowledge_packs()
            .await
            .expect("list knowledge packs after retry");

        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].slug, "humanizer");

        let request_lines = server.join().expect("join test server");
        assert_eq!(
            request_lines,
            vec![
                "GET /api/v1/knowledge HTTP/1.1".to_string(),
                "GET /api/v1/knowledge HTTP/1.1".to_string()
            ]
        );
    }
}
