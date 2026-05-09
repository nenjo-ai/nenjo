//! HTTP client for the platform manifest API.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, StatusCode, Url, header, multipart};
use uuid::Uuid;

use crate::manifest_mcp::{
    AbilityCreateDocument, AbilityDocument, AbilityPromptMutationResult, AbilityUpdateDocument,
    AgentCreateDocument, AgentDocument, AgentUpdateDocument, ContextBlockCreateDocument,
    ContextBlockDocument, ContextBlockUpdateDocument, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilDocument, CouncilMemberUpdateDocument,
    CouncilUpdateDocument, DomainCreateDocument, DomainDocument, DomainPromptDocument,
    DomainPromptMutationResult, DomainUpdateDocument, ModelCreateDocument, ModelDocument,
    ModelUpdateDocument, ProjectCreateDocument, ProjectDocument, ProjectDocumentContentDocument,
    ProjectDocumentCreateDocument, ProjectDocumentSummary, ProjectUpdateDocument,
    RoutineCreateDocument, RoutineDocument, RoutineGraphInput, RoutineUpdateDocument,
};
use crate::types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
use nenjo::manifest::{
    CouncilDelegationStrategy, RoutineEdgeCondition, RoutineEdgeManifest, RoutineMetadata,
    RoutineStepManifest, RoutineStepType, RoutineTrigger,
};

/// Thin HTTP client for Nenjo platform manifest endpoints.
#[derive(Debug, Clone)]
pub struct PlatformManifestClient {
    http: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, serde::Deserialize)]
struct RoutineResponseRow {
    id: Uuid,
    name: String,
    description: Option<String>,
    trigger: RoutineTrigger,
    #[serde(default)]
    metadata: RoutineMetadata,
}

#[derive(Debug, serde::Deserialize)]
struct RoutineResponseDetail {
    #[serde(flatten)]
    routine: RoutineResponseRow,
    #[serde(default)]
    steps: Vec<RoutineResponseStep>,
    #[serde(default)]
    edges: Vec<RoutineResponseEdge>,
}

#[derive(Debug, serde::Deserialize)]
struct RoutineResponseStep {
    id: Uuid,
    routine_id: Uuid,
    name: String,
    step_type: String,
    council_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    #[serde(default)]
    config: serde_json::Value,
    order_index: i32,
}

#[derive(Debug, serde::Deserialize)]
struct RoutineResponseEdge {
    id: Uuid,
    routine_id: Uuid,
    source_step_id: Uuid,
    target_step_id: Uuid,
    condition: String,
}

#[derive(Debug, serde::Serialize)]
struct RoutineCreateApiBody<'a> {
    name: &'a str,
    description: Option<&'a str>,
    trigger: Option<RoutineTrigger>,
    metadata: Option<&'a RoutineMetadata>,
}

#[derive(Debug, serde::Serialize)]
struct RoutineUpdateApiBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<Option<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger: Option<RoutineTrigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a RoutineMetadata>,
}

#[derive(Debug, serde::Serialize)]
struct SaveRoutineGraphStepBody {
    client_ref: String,
    id: Option<Uuid>,
    name: String,
    step_type: String,
    council_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    lambda_id: Option<Uuid>,
    config: serde_json::Value,
    position_x: f64,
    position_y: f64,
    order_index: i32,
}

#[derive(Debug, serde::Serialize)]
struct SaveRoutineGraphEdgeBody {
    source_ref: String,
    target_ref: String,
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
    trigger: Option<RoutineTrigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a RoutineMetadata>,
    entry_step_refs: Vec<String>,
    steps: Vec<SaveRoutineGraphStepBody>,
    edges: Vec<SaveRoutineGraphEdgeBody>,
}

#[derive(Debug, serde::Deserialize)]
struct PromptMutationEnvelope<T> {
    #[serde(default)]
    prompt_config: Option<T>,
}

#[derive(Debug, serde::Deserialize)]
struct ContentMutationEnvelope<T> {
    #[serde(default)]
    template: Option<T>,
}

#[derive(Debug, serde::Deserialize)]
struct AuthMeResponse {
    org_id: Option<String>,
}

fn routine_document_from_detail(detail: RoutineResponseDetail) -> RoutineDocument {
    RoutineDocument {
        summary: crate::manifest_mcp::RoutineSummary {
            id: detail.routine.id,
            name: detail.routine.name,
            description: detail.routine.description,
            trigger: detail.routine.trigger,
        },
        metadata: detail.routine.metadata,
        steps: detail
            .steps
            .into_iter()
            .map(|step| RoutineStepManifest {
                id: step.id,
                routine_id: step.routine_id,
                name: step.name,
                step_type: match step.step_type.as_str() {
                    "council" => RoutineStepType::Council,
                    "cron" => RoutineStepType::Cron,
                    "gate" => RoutineStepType::Gate,
                    "terminal" => RoutineStepType::Terminal,
                    "terminal_fail" => RoutineStepType::TerminalFail,
                    _ => RoutineStepType::Agent,
                },
                council_id: step.council_id,
                agent_id: step.agent_id,
                config: step.config,
                order_index: step.order_index,
            })
            .collect(),
        edges: detail
            .edges
            .into_iter()
            .map(|edge| RoutineEdgeManifest {
                id: edge.id,
                routine_id: edge.routine_id,
                source_step_id: edge.source_step_id,
                target_step_id: edge.target_step_id,
                condition: RoutineEdgeCondition::from_str_value(&edge.condition),
            })
            .collect(),
    }
}

fn routine_graph_body<'a>(
    name: Option<&'a str>,
    description: Option<Option<&'a str>>,
    trigger: Option<RoutineTrigger>,
    metadata: Option<&'a RoutineMetadata>,
    graph: &'a RoutineGraphInput,
) -> SaveRoutineGraphBody<'a> {
    SaveRoutineGraphBody {
        name,
        description,
        trigger,
        metadata,
        entry_step_refs: graph.entry_step_ids.clone(),
        steps: graph
            .steps
            .iter()
            .map(|step| SaveRoutineGraphStepBody {
                client_ref: step.step_id.clone(),
                id: Uuid::parse_str(&step.step_id).ok(),
                name: step.name.clone(),
                step_type: step.step_type.to_string(),
                council_id: step.council_id,
                agent_id: step.agent_id,
                lambda_id: step
                    .config
                    .get("lambda_id")
                    .and_then(|v| v.as_str().and_then(|value| Uuid::parse_str(value).ok())),
                config: step.config.clone(),
                position_x: step
                    .config
                    .get("position_x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                position_y: step
                    .config
                    .get("position_y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                order_index: step.order_index,
            })
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|edge| SaveRoutineGraphEdgeBody {
                source_ref: edge.source_step_id.clone(),
                target_ref: edge.target_step_id.clone(),
                condition: Some(
                    match edge.condition {
                        RoutineEdgeCondition::Always => "always",
                        RoutineEdgeCondition::OnPass => "on_pass",
                        RoutineEdgeCondition::OnFail => "on_fail",
                    }
                    .to_string(),
                ),
                metadata: None,
            })
            .collect(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseRow {
    id: Uuid,
    name: String,
    leader_agent_id: Uuid,
    delegation_strategy: CouncilDelegationStrategy,
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseAgentSummary {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct CouncilResponseMemberDetail {
    id: Uuid,
    agent_id: Uuid,
    priority: i32,
    agent: CouncilResponseAgentSummary,
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
            id: detail.council.id,
            name: detail.council.name,
            delegation_strategy: detail.council.delegation_strategy,
            leader_agent_id: detail.council.leader_agent_id,
        },
        members: detail
            .members
            .into_iter()
            .map(|member| crate::manifest_mcp::CouncilMemberDocument {
                agent_id: member.agent_id,
                agent_name: member.agent.name,
                priority: member.priority,
            })
            .collect(),
    }
}

fn council_member_id_by_agent(detail: &CouncilResponseDetail, agent_id: Uuid) -> Option<Uuid> {
    detail
        .members
        .iter()
        .find(|member| member.agent_id == agent_id)
        .map(|member| member.id)
}

#[derive(Debug, Clone, serde::Deserialize)]
/// Response body for a project document content read.
pub struct ProjectDocumentContentResponse {
    /// Plaintext document content when the platform can return it directly.
    #[serde(default)]
    pub content: Option<String>,
    /// Stored document filename.
    pub filename: String,
    /// MIME content type for the stored document.
    pub content_type: String,
    /// Stored content size in bytes.
    pub size_bytes: i64,
    /// Encrypted payload when content is protected outside the platform response body.
    #[serde(default)]
    pub encrypted_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// Metadata for one project document stored by the platform.
pub struct ProjectDocumentMetadata {
    /// Document ID.
    pub id: Uuid,
    /// Owning project ID.
    pub project_id: Uuid,
    /// Stored document filename.
    pub filename: String,
    /// Optional repository-relative or project-relative path.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional document kind classifier.
    #[serde(default)]
    pub kind: Option<String>,
    /// Authority that owns or produced the document.
    pub authority: String,
    /// Optional short summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// Optional processing or publication status.
    #[serde(default)]
    pub status: Option<String>,
    /// Tags associated with the document.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Alternate names that should resolve to this document.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Search keywords associated with the document.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// MIME content type for the stored document.
    pub content_type: String,
    /// Stored content size in bytes.
    pub size_bytes: i64,
    /// Platform creation timestamp.
    pub created_at: String,
    /// Platform update timestamp.
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// Directed relationship between two project documents.
pub struct ProjectDocumentEdge {
    /// Edge ID.
    pub id: Uuid,
    /// Owning project ID.
    pub project_id: Uuid,
    /// Source document ID.
    pub source_document_id: Uuid,
    /// Target document ID.
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

#[derive(Debug, Clone, Default, serde::Serialize)]
/// Query parameters for listing project tasks.
pub struct ProjectTaskListQuery {
    /// Project whose tasks should be listed.
    pub project_id: Uuid,
    /// Optional task status filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Optional task priority filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// Optional task type filter.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// Optional comma-separated tag filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<String>,
    /// Optional routine ID filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<Uuid>,
    /// Optional agent assignment filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_agent_id: Option<Uuid>,
    /// Optional maximum number of tasks to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Optional result offset for pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
/// Query parameters for listing project execution runs.
pub struct ProjectExecutionListQuery {
    /// Project whose execution runs should be listed.
    pub project_id: Uuid,
    /// Optional agent ID filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    /// Optional routine ID filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<Uuid>,
    /// Optional execution status filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Optional maximum number of runs to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Optional result offset for pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
/// Request body for creating a project execution run.
pub struct CreateExecutionRequest {
    /// Project that should own the execution run.
    pub project_id: Uuid,
    /// Execution-specific configuration payload.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Optional number of models to use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_count: Option<i32>,
    /// Optional parallelism setting for the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_count: Option<i32>,
    /// Optional initial status to assign to the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_status: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExecutionCommandRequest<'a> {
    command: &'a str,
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
            .send()
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
            .send()
            .await
            .context("failed to fetch agents")?;

        match response.status() {
            StatusCode::OK => response.json().await.context("failed to decode agents"),
            status => bail!("agents request failed with status {status}"),
        }
    }

    /// Fetch one agent document by ID.
    pub async fn fetch_agent_document(&self, id: Uuid) -> Result<Option<AgentDocument>> {
        let response = self
            .http
            .get(format!("{}/api/v1/agents/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch agent {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .map(Some)
                .context("failed to decode agent"),
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("agent request failed with status {status}"),
        }
    }

    /// Create an agent document.
    pub async fn create_agent_document(
        &self,
        agent: &AgentCreateDocument,
    ) -> Result<AgentDocument> {
        let body = serde_json::to_value(agent).context("failed to encode agent create payload")?;
        let response = self
            .http
            .post(format!("{}/api/v1/agents", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create agent")?;

        match response.status() {
            StatusCode::CREATED => response
                .json::<AgentDocument>()
                .await
                .context("failed to decode created agent"),
            status => bail!("agent create failed with status {status}"),
        }
    }

    /// Apply a partial metadata update to an agent document.
    pub async fn update_agent_document(
        &self,
        id: Uuid,
        agent: &AgentUpdateDocument,
    ) -> Result<AgentDocument> {
        let body = serde_json::to_value(agent).context("failed to encode agent update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/agents/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update agent {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated agent"),
            status => bail!("agent update failed with status {status}"),
        }
    }

    /// Update an agent's prompt document and return the canonical prompt config when provided.
    pub async fn update_agent_prompt_document(
        &self,
        id: Uuid,
        prompt_config: &serde_json::Value,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>> {
        let mut body = serde_json::json!({
            "prompt_config": prompt_config,
        });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }

        let response = self
            .http
            .patch(format!("{}/api/v1/agents/{id}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update agent prompt {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let payload = response
                    .json::<serde_json::Value>()
                    .await
                    .context("failed to decode updated agent prompt")?;
                Ok(payload.get("prompt_config").cloned())
            }
            status => bail!("agent prompt update failed with status {status}"),
        }
    }

    /// Delete an agent document by ID.
    pub async fn delete_agent_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/agents/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete agent {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("agent delete failed with status {status}"),
        }
    }

    /// Create an ability document, optionally sending encrypted prompt payload content.
    pub async fn create_ability_document(
        &self,
        ability: &AbilityCreateDocument,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<AbilityDocument> {
        let mut body =
            serde_json::to_value(ability).context("failed to encode ability create payload")?;
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .post(format!("{}/api/v1/abilities", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create ability")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created ability"),
            status => bail!("ability create failed with status {status}"),
        }
    }

    /// Fetch one ability document by ID.
    pub async fn fetch_ability_document(&self, id: Uuid) -> Result<Option<AbilityDocument>> {
        let response = self
            .http
            .get(format!("{}/api/v1/abilities/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch ability {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .map(Some)
                .context("failed to decode ability"),
            StatusCode::NOT_FOUND => Ok(None),
            status => bail!("ability request failed with status {status}"),
        }
    }

    /// Apply a partial metadata update to an ability document.
    pub async fn update_ability_document(
        &self,
        id: Uuid,
        ability: &AbilityUpdateDocument,
    ) -> Result<AbilityDocument> {
        let body =
            serde_json::to_value(ability).context("failed to encode ability update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/abilities/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update ability {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated ability"),
            status => bail!("ability update failed with status {status}"),
        }
    }

    /// Update an ability prompt document and return the canonical prompt config when provided.
    pub async fn update_ability_prompt_document(
        &self,
        id: Uuid,
        prompt_config: &nenjo::manifest::AbilityPromptConfig,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<AbilityPromptMutationResult> {
        let mut body = serde_json::json!({ "prompt_config": prompt_config });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .patch(format!("{}/api/v1/abilities/{id}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update ability prompt {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let body: PromptMutationEnvelope<nenjo::manifest::AbilityPromptConfig> = response
                    .json()
                    .await
                    .context("failed to decode updated ability prompt")?;
                Ok(AbilityPromptMutationResult {
                    prompt_config: body.prompt_config.unwrap_or_else(|| prompt_config.clone()),
                })
            }
            status => bail!("ability prompt update failed with status {status}"),
        }
    }

    /// Delete an ability document by ID.
    pub async fn delete_ability_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/abilities/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete ability {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("ability delete failed with status {status}"),
        }
    }

    /// Create a domain document, optionally sending encrypted prompt payload content.
    pub async fn create_domain_document(
        &self,
        domain: &DomainCreateDocument,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<DomainDocument> {
        let mut body =
            serde_json::to_value(domain).context("failed to encode domain create payload")?;
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .post(format!("{}/api/v1/domains", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create domain")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created domain"),
            status => bail!("domain create failed with status {status}"),
        }
    }

    /// Apply a partial metadata update to a domain document.
    pub async fn update_domain_document(
        &self,
        id: Uuid,
        domain: &DomainUpdateDocument,
    ) -> Result<DomainDocument> {
        let body = serde_json::to_value(domain).context("failed to encode domain update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/domains/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update domain {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated domain"),
            status => bail!("domain update failed with status {status}"),
        }
    }

    /// Fetch a domain manifest document including prompt configuration.
    pub async fn get_domain_manifest_document(&self, id: Uuid) -> Result<DomainPromptDocument> {
        let response = self
            .http
            .get(format!("{}/api/v1/domains/{id}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch domain prompt {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode domain prompt"),
            status => bail!("domain prompt get failed with status {status}"),
        }
    }

    /// Update a domain manifest prompt document.
    pub async fn update_domain_manifest_document(
        &self,
        id: Uuid,
        prompt_config: nenjo::manifest::DomainPromptConfig,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<DomainPromptMutationResult> {
        let mut body = serde_json::json!({
            "prompt_config": prompt_config
        });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .patch(format!("{}/api/v1/domains/{id}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update domain prompt {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let body: PromptMutationEnvelope<nenjo::manifest::DomainPromptConfig> = response
                    .json()
                    .await
                    .context("failed to decode updated domain prompt")?;
                Ok(DomainPromptMutationResult {
                    prompt_config: body.prompt_config.unwrap_or(prompt_config),
                })
            }
            status => bail!("domain prompt update failed with status {status}"),
        }
    }

    /// Delete a domain document by ID.
    pub async fn delete_domain_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/domains/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete domain {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("domain delete failed with status {status}"),
        }
    }

    /// Create a project document.
    pub async fn create_project_document(
        &self,
        project: &ProjectCreateDocument,
    ) -> Result<ProjectDocument> {
        let body =
            serde_json::to_value(project).context("failed to encode project create payload")?;
        let response = self
            .http
            .post(format!("{}/api/v1/projects", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
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

    /// Apply a partial metadata update to a project document.
    pub async fn update_project_document(
        &self,
        id: Uuid,
        project: &ProjectUpdateDocument,
    ) -> Result<ProjectDocument> {
        let mut body = serde_json::Map::new();
        if let Some(name) = &project.name {
            body.insert("name".into(), serde_json::json!(name));
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
            .patch(format!("{}/api/v1/projects/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .with_context(|| format!("failed to update project {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated project"),
            status => bail!("project update failed with status {status}"),
        }
    }

    /// Delete a project document by ID.
    pub async fn delete_project_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/projects/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete project {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("project delete failed with status {status}"),
        }
    }

    /// List project document summaries for a project.
    pub async fn list_project_documents(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectDocumentSummary>> {
        let documents = self.list_project_document_metadata(project_id).await?;
        Ok(documents
            .into_iter()
            .map(|document| ProjectDocumentSummary {
                id: document.id,
                project_id: document.project_id,
                filename: document.filename,
                content_type: document.content_type,
                size_bytes: document.size_bytes,
                updated_at: document.updated_at,
            })
            .collect())
    }

    /// List project document metadata records for a project.
    pub async fn list_project_document_metadata(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectDocumentMetadata>> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/projects/{project_id}/documents",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to list documents for project {project_id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode project document metadata"),
            status => bail!("project document list failed with status {status}"),
        }
    }

    /// Fetch one project document summary.
    pub async fn get_project_document(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<Option<ProjectDocumentSummary>> {
        let document = self
            .get_project_document_metadata(project_id, document_id)
            .await?;
        Ok(document.map(|document| ProjectDocumentSummary {
            id: document.id,
            project_id: document.project_id,
            filename: document.filename,
            content_type: document.content_type,
            size_bytes: document.size_bytes,
            updated_at: document.updated_at,
        }))
    }

    /// Fetch one project document metadata record.
    pub async fn get_project_document_metadata(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<Option<ProjectDocumentMetadata>> {
        let documents = self.list_project_document_metadata(project_id).await?;
        Ok(documents
            .into_iter()
            .find(|document| document.id == document_id))
    }

    /// Fetch raw project document content from the platform.
    pub async fn fetch_project_document_content(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<ProjectDocumentContentResponse> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/projects/{project_id}/documents/{document_id}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| {
                format!("failed to fetch content for project document {document_id}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode project document content"),
            status => bail!("project document content fetch failed with status {status}"),
        }
    }

    /// Fetch project document content in the manifest MCP document shape.
    pub async fn get_project_document_content(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<ProjectDocumentContentDocument> {
        let metadata = self
            .get_project_document_metadata(project_id, document_id)
            .await?
            .ok_or_else(|| anyhow!("project document not found: {document_id}"))?;
        let payload = self
            .fetch_project_document_content(project_id, document_id)
            .await?;
        let content = payload
            .content
            .ok_or_else(|| anyhow!("project document content response did not include content"))?;
        Ok(ProjectDocumentContentDocument {
            document: ProjectDocumentSummary {
                id: metadata.id,
                project_id: metadata.project_id,
                filename: payload.filename,
                content_type: payload.content_type,
                size_bytes: payload.size_bytes,
                updated_at: metadata.updated_at,
            },
            description: content,
        })
    }

    /// List graph edges connected to a project document.
    pub async fn list_project_document_edges(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<Vec<ProjectDocumentEdge>> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/projects/{project_id}/documents/{document_id}/edges",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to list edges for project document {document_id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode project document edges"),
            status => bail!("project document edge list failed with status {status}"),
        }
    }

    /// Create a project file document, optionally sending encrypted content payload.
    pub async fn create_project_file_document(
        &self,
        document: &ProjectDocumentCreateDocument,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<ProjectDocumentSummary> {
        let content_type = document
            .content_type
            .clone()
            .unwrap_or_else(|| "text/plain".to_string());
        let file_part = multipart::Part::bytes(document.description.clone().into_bytes())
            .file_name(document.filename.clone())
            .mime_str(&content_type)
            .context("failed to encode project document mime type")?;
        let mut form = multipart::Form::new().part("file", file_part);
        if let Some(encrypted_payload) = encrypted_payload {
            form = form.text(
                "encrypted_payload",
                serde_json::to_string(&encrypted_payload)
                    .context("failed to encode encrypted project document payload")?,
            );
        }

        let response = self
            .http
            .post(format!(
                "{}/api/v1/projects/{}/documents",
                self.base_url, document.project_id
            ))
            .header("X-API-Key", &self.api_key)
            .multipart(form)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to create project document {} for project {}",
                    document.filename, document.project_id
                )
            })?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created project document"),
            status => bail!("project document create failed with status {status}"),
        }
    }

    /// Update the content for an existing project document.
    pub async fn update_project_document_content(
        &self,
        project_id: Uuid,
        document_id: Uuid,
        description: &str,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<ProjectDocumentSummary> {
        let mut body = serde_json::json!({ "content": description });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }

        let response = self
            .http
            .put(format!(
                "{}/api/v1/projects/{project_id}/documents/{document_id}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| {
                format!("failed to update content for project document {document_id}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated project document"),
            status => bail!("project document content update failed with status {status}"),
        }
    }

    /// Delete a project file document by project and document ID.
    pub async fn delete_project_file_document(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<()> {
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/projects/{project_id}/documents/{document_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete project document {document_id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("project document delete failed with status {status}"),
        }
    }

    /// List project tasks using the platform task API.
    pub async fn list_project_tasks(
        &self,
        query: &ProjectTaskListQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!("{}/api/v1/tasks", self.base_url))
            .context("failed to build task list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("project_id", &query.project_id.to_string());
            if let Some(status) = query.status.as_ref() {
                pairs.append_pair("status", status);
            }
            if let Some(priority) = query.priority.as_ref() {
                pairs.append_pair("priority", priority);
            }
            if let Some(task_type) = query.task_type.as_ref() {
                pairs.append_pair("type", task_type);
            }
            if let Some(tags) = query.tags.as_ref() {
                pairs.append_pair("tags", tags);
            }
            if let Some(routine_id) = query.routine_id {
                pairs.append_pair("routine_id", &routine_id.to_string());
            }
            if let Some(assigned_agent_id) = query.assigned_agent_id {
                pairs.append_pair("assigned_agent_id", &assigned_agent_id.to_string());
            }
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
            .send()
            .await
            .with_context(|| format!("failed to list tasks for project {}", query.project_id))?;

        match response.status() {
            StatusCode::OK => response
                .json::<serde_json::Value>()
                .await
                .context("failed to decode task list"),
            status => bail!("project task list failed with status {status}"),
        }
    }

    /// Fetch the current organization ID associated with the API key.
    pub async fn current_org_id(&self) -> Result<Uuid> {
        let response = self
            .http
            .get(format!("{}/api/v1/auth/me", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
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

    /// Fetch one project task by ID.
    pub async fn get_project_task(&self, task_id: Uuid) -> Result<serde_json::Value> {
        let response = self
            .http
            .get(format!("{}/api/v1/tasks/{task_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch task {task_id}"))?;

        match response.status() {
            StatusCode::OK => response.json().await.context("failed to decode task"),
            status => bail!("project task fetch failed with status {status}"),
        }
    }

    /// Create a project task using a raw platform task payload.
    pub async fn create_project_task(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!("{}/api/v1/tasks", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .context("failed to create project task")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created project task"),
            status => bail!("project task create failed with status {status}"),
        }
    }

    /// Create multiple project tasks using a raw platform bulk payload.
    pub async fn bulk_create_project_tasks(
        &self,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!("{}/api/v1/tasks/bulk", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .context("failed to bulk create project tasks")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode bulk-created project tasks"),
            status => bail!("project task bulk create failed with status {status}"),
        }
    }

    /// Update a project task using a raw platform task payload.
    pub async fn update_project_task(
        &self,
        task_id: Uuid,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .patch(format!("{}/api/v1/tasks/{task_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to update task {task_id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated project task"),
            status => bail!("project task update failed with status {status}"),
        }
    }

    /// Delete a project task by ID.
    pub async fn delete_project_task(&self, task_id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/tasks/{task_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete task {task_id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("project task delete failed with status {status}"),
        }
    }

    /// List project execution runs using the platform execution API.
    pub async fn list_project_execution_runs(
        &self,
        query: &ProjectExecutionListQuery,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(&format!("{}/api/v1/executions", self.base_url))
            .context("failed to build execution list URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("project_id", &query.project_id.to_string());
            if let Some(agent_id) = query.agent_id {
                pairs.append_pair("agent_id", &agent_id.to_string());
            }
            if let Some(routine_id) = query.routine_id {
                pairs.append_pair("routine_id", &routine_id.to_string());
            }
            if let Some(status) = query.status.as_ref() {
                pairs.append_pair("status", status);
            }
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
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to list execution runs for project {}",
                    query.project_id
                )
            })?;

        match response.status() {
            StatusCode::OK => response
                .json::<serde_json::Value>()
                .await
                .context("failed to decode execution run list"),
            status => bail!("project execution run list failed with status {status}"),
        }
    }

    /// Fetch one project execution run by ID.
    pub async fn get_project_execution_run(
        &self,
        execution_run_id: Uuid,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .get(format!(
                "{}/api/v1/executions/{execution_run_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch execution run {execution_run_id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode execution run"),
            status => bail!("project execution run fetch failed with status {status}"),
        }
    }

    /// Create a project execution run.
    pub async fn create_execution_run(
        &self,
        request: &CreateExecutionRequest,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!("{}/api/v1/executions", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .send()
            .await
            .context("failed to create execution run")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created execution run"),
            status => bail!("project execution start failed with status {status}"),
        }
    }

    /// Send a command to an existing project execution run.
    pub async fn command_project_execution_run(
        &self,
        execution_run_id: Uuid,
        command: &str,
    ) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/executions/{execution_run_id}/command",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&ExecutionCommandRequest { command })
            .send()
            .await
            .with_context(|| {
                format!("failed to send '{command}' command to execution run {execution_run_id}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode commanded execution run"),
            status => bail!("project execution command failed with status {status}"),
        }
    }

    /// Create a routine document and optional routine graph.
    pub async fn create_routine_document(
        &self,
        routine: &RoutineCreateDocument,
    ) -> Result<RoutineDocument> {
        let body = RoutineCreateApiBody {
            name: &routine.name,
            description: routine.description.as_deref(),
            trigger: routine.trigger,
            metadata: routine.metadata.as_ref(),
        };
        let response = self
            .http
            .post(format!("{}/api/v1/routines", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create routine")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let detail: RoutineResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode created routine")?;
                let created = routine_document_from_detail(detail);
                if let Some(graph) = routine.graph.as_ref() {
                    self.save_routine_graph_document(
                        created.summary.id,
                        &routine_graph_body(
                            Some(&routine.name),
                            Some(routine.description.as_deref()),
                            routine.trigger,
                            routine.metadata.as_ref(),
                            graph,
                        ),
                    )
                    .await
                } else {
                    Ok(created)
                }
            }
            status => bail!("routine create failed with status {status}"),
        }
    }

    /// Update routine metadata and optional routine graph.
    pub async fn update_routine_document(
        &self,
        id: Uuid,
        routine: &RoutineUpdateDocument,
    ) -> Result<RoutineDocument> {
        if let Some(graph) = routine.graph.as_ref() {
            return self
                .save_routine_graph_document(
                    id,
                    &routine_graph_body(
                        routine.name.as_deref(),
                        routine.description.as_ref().map(|value| value.as_deref()),
                        routine.trigger,
                        routine.metadata.as_ref(),
                        graph,
                    ),
                )
                .await;
        }

        let body = RoutineUpdateApiBody {
            name: routine.name.as_deref(),
            description: routine.description.as_ref().map(|value| value.as_deref()),
            trigger: routine.trigger,
            metadata: routine.metadata.as_ref(),
        };
        let response = self
            .http
            .patch(format!("{}/api/v1/routines/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update routine {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let detail: RoutineResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode updated routine")?;
                Ok(routine_document_from_detail(detail))
            }
            status => bail!("routine update failed with status {status}"),
        }
    }

    async fn save_routine_graph_document(
        &self,
        id: Uuid,
        body: &SaveRoutineGraphBody<'_>,
    ) -> Result<RoutineDocument> {
        let response = self
            .http
            .patch(format!("{}/api/v1/routines/{id}/graph", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to save routine graph for {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let detail: RoutineResponseDetail = response
                    .json()
                    .await
                    .context("failed to decode updated routine graph")?;
                Ok(routine_document_from_detail(detail))
            }
            status => bail!("routine graph save failed with status {status}"),
        }
    }

    /// Delete a routine document by ID.
    pub async fn delete_routine_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/routines/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete routine {id}"))?;

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
        let body = serde_json::json!({
            "name": model.name,
            "description": model.description,
            "model": model.model,
            "model_provider": model.model_provider.clone().unwrap_or_else(|| "openai".into()),
            "temperature": model.temperature.unwrap_or(0.7),
            "base_url": model.base_url,
        });
        let response = self
            .http
            .post(format!("{}/api/v1/models", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create model")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created model"),
            status => bail!("model create failed with status {status}"),
        }
    }

    /// Apply a partial update to a model document.
    pub async fn update_model_document(
        &self,
        id: Uuid,
        model: &ModelUpdateDocument,
    ) -> Result<ModelDocument> {
        let body = serde_json::to_value(model).context("failed to encode model update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/models/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update model {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated model"),
            status => bail!("model update failed with status {status}"),
        }
    }

    /// Delete a model document by ID.
    pub async fn delete_model_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/models/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete model {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("model delete failed with status {status}"),
        }
    }

    async fn fetch_council_document(&self, id: Uuid) -> Result<CouncilDocument> {
        Ok(council_document_from_detail(
            self.fetch_council_detail(id).await?,
        ))
    }

    async fn fetch_council_detail(&self, id: Uuid) -> Result<CouncilResponseDetail> {
        let response = self
            .http
            .get(format!("{}/api/v1/councils/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch council {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode council detail"),
            status => bail!("council detail request failed with status {status}"),
        }
    }

    /// Create a council document.
    pub async fn create_council_document(
        &self,
        council: &CouncilCreateDocument,
    ) -> Result<CouncilDocument> {
        let body =
            serde_json::to_value(council).context("failed to encode council create payload")?;
        let response = self
            .http
            .post(format!("{}/api/v1/councils", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
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
        id: Uuid,
        council: &CouncilUpdateDocument,
    ) -> Result<CouncilDocument> {
        let body =
            serde_json::to_value(council).context("failed to encode council update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/councils/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update council {id}"))?;

        match response.status() {
            StatusCode::OK => self.fetch_council_document(id).await,
            status => bail!("council update failed with status {status}"),
        }
    }

    /// Delete a council document by ID.
    pub async fn delete_council_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/councils/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete council {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("council delete failed with status {status}"),
        }
    }

    /// Add an agent member to a council.
    pub async fn add_council_member_document(
        &self,
        council_id: Uuid,
        member: &CouncilCreateMemberDocument,
    ) -> Result<CouncilDocument> {
        let body = serde_json::to_value(member)
            .context("failed to encode council member create payload")?;
        let response = self
            .http
            .post(format!(
                "{}/api/v1/councils/{council_id}/members",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to add member to council {council_id}"))?;

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

    /// Update a council member by council and agent ID.
    pub async fn update_council_member_document(
        &self,
        council_id: Uuid,
        agent_id: Uuid,
        member: &CouncilMemberUpdateDocument,
    ) -> Result<CouncilDocument> {
        let detail = self.fetch_council_detail(council_id).await?;
        let member_id = council_member_id_by_agent(&detail, agent_id).ok_or_else(|| {
            anyhow!("council member not found for council {council_id} and agent {agent_id}")
        })?;
        let body = serde_json::to_value(member)
            .context("failed to encode council member update payload")?;
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/councils/{council_id}/members/{member_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update member in council {council_id}"))?;

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
        council_id: Uuid,
        agent_id: Uuid,
    ) -> Result<CouncilDocument> {
        let detail = self.fetch_council_detail(council_id).await?;
        let member_id = council_member_id_by_agent(&detail, agent_id).ok_or_else(|| {
            anyhow!("council member not found for council {council_id} and agent {agent_id}")
        })?;
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/councils/{council_id}/members/{member_id}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to remove member from council {council_id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => self.fetch_council_document(council_id).await,
            status => bail!("council remove member failed with status {status}"),
        }
    }

    /// Create a context block document.
    pub async fn create_context_block_document(
        &self,
        context_block: &ContextBlockCreateDocument,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<ContextBlockDocument> {
        let mut body = serde_json::to_value(context_block)
            .context("failed to encode context block create payload")?;
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .post(format!("{}/api/v1/context-blocks", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to create context block")?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created context block"),
            status => bail!("context block create failed with status {status}"),
        }
    }

    /// Apply a partial metadata update to a context block document.
    pub async fn update_context_block_document(
        &self,
        id: Uuid,
        context_block: &ContextBlockUpdateDocument,
    ) -> Result<ContextBlockDocument> {
        let body = serde_json::to_value(context_block)
            .context("failed to encode context block update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/context-blocks/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update context block {id}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated context block"),
            status => bail!("context block update failed with status {status}"),
        }
    }

    /// Update a context block template document.
    pub async fn update_context_block_content_document(
        &self,
        id: Uuid,
        template: &str,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<crate::manifest_mcp::ContextBlockContentMutationResult> {
        let mut body = serde_json::json!({ "template": template });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/context-blocks/{id}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update context block content {id}"))?;

        match response.status() {
            StatusCode::OK => {
                let body: ContentMutationEnvelope<String> = response
                    .json()
                    .await
                    .context("failed to decode updated context block content")?;
                Ok(crate::manifest_mcp::ContextBlockContentMutationResult {
                    template: body.template.unwrap_or_else(|| template.to_string()),
                })
            }
            status => bail!("context block content update failed with status {status}"),
        }
    }

    /// Delete a context block document by ID.
    pub async fn delete_context_block_document(&self, id: Uuid) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/context-blocks/{id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete context block {id}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("context block delete failed with status {status}"),
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
            .send()
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
}
