//! HTTP client for the platform manifest API.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, StatusCode, Url, header, multipart};
use uuid::Uuid;

use crate::manifest_mcp::{
    AbilityCreateDocument, AbilityDocument, AbilityPromptMutationResult, AbilityUpdateDocument,
    AgentCreateDocument, AgentDocument, AgentUpdateDocument, ContextBlockCreateDocument,
    ContextBlockDocument, ContextBlockUpdateDocument, CouncilDocument, CouncilMemberUpdateDocument,
    CouncilUpdateDocument, DomainCreateDocument, DomainDocument, DomainPromptDocument,
    DomainPromptMutationResult, DomainUpdateDocument, KnowledgeDocCreateDocument,
    KnowledgeDocSummary, KnowledgeDocUpdateDocument, ModelCreateDocument, ModelDocument,
    ModelUpdateDocument, ProjectCreateDocument, ProjectDocument, ProjectUpdateDocument,
    ResourceRef, RoutineCreateDocument, RoutineDocument, RoutineGraphInput, RoutineUpdateDocument,
};
use crate::types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
use nenjo::Slug;
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
    name: String,
    step_type: String,
    #[serde(default)]
    council: Option<Slug>,
    #[serde(default)]
    agent: Option<Slug>,
    #[serde(default)]
    config: serde_json::Value,
    order_index: i32,
}

#[derive(Debug, serde::Deserialize)]
struct RoutineResponseEdge {
    id: Uuid,
    source_step: Slug,
    target_step: Slug,
    condition: String,
    #[serde(default)]
    metadata: serde_json::Value,
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
    council: Option<Slug>,
    agent: Option<Slug>,
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

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PlatformKnowledgePackMetadata {
    pub id: Uuid,
    pub slug: Slug,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

fn routine_document_from_detail(detail: RoutineResponseDetail) -> RoutineDocument {
    let routine = Slug::derive(&detail.routine.name);
    let step_slugs = detail
        .steps
        .iter()
        .map(|step| (step.id, Slug::derive(&step.name)))
        .collect::<std::collections::HashMap<_, _>>();
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
                slug: step_slugs
                    .get(&step.id)
                    .cloned()
                    .unwrap_or_else(|| Slug::derive(&step.name)),
                routine: routine.clone(),
                name: step.name,
                step_type: match step.step_type.as_str() {
                    "council" => RoutineStepType::Council,
                    "cron" => RoutineStepType::Cron,
                    "gate" => RoutineStepType::Gate,
                    "terminal" => RoutineStepType::Terminal,
                    "terminal_fail" => RoutineStepType::TerminalFail,
                    _ => RoutineStepType::Agent,
                },
                council: step.council,
                agent: step.agent,
                config: step.config,
                order_index: step.order_index,
            })
            .collect(),
        edges: detail
            .edges
            .into_iter()
            .map(|edge| RoutineEdgeManifest {
                id: edge.id,
                routine: routine.clone(),
                source_step: edge.source_step,
                target_step: edge.target_step,
                condition: RoutineEdgeCondition::from_str_value(&edge.condition),
                metadata: edge.metadata,
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
                council: step.council.clone(),
                agent: step.agent.clone(),
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
                source_ref: edge.source_step.clone(),
                target_ref: edge.target_step.clone(),
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
            id: detail.council.id,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeDocMetadataResponse {
    pub id: Uuid,
    pub pack_id: Uuid,
    pub slug: Slug,
    pub filename: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub content_type: String,
    pub updated_at: String,
}

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// Directed relationship between two library knowledge documents.
pub struct KnowledgeDocEdgeResponse {
    pub id: Uuid,
    pub org_id: Uuid,
    pub source_item_id: Uuid,
    pub source_doc: Slug,
    pub target_item_id: Uuid,
    pub target_doc: Slug,
    pub edge_type: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
/// Query parameters for listing project tasks.
pub struct ProjectTaskListQuery {
    /// Project whose tasks should be listed.
    pub project: Slug,
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
    /// Optional routine slug filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<Slug>,
    /// Optional agent assignment filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_agent: Option<Slug>,
    /// Optional maximum number of tasks to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Optional result offset for pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
/// Query parameters for listing project execution runs.
pub struct ProjectExecutionListQuery {
    /// Project whose execution runs should be listed.
    pub project: Slug,
    /// Optional agent slug filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Slug>,
    /// Optional routine slug filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<Slug>,
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
    pub project: Slug,
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

    /// Fetch one agent document by slug.
    pub async fn fetch_agent_document(&self, agent: &Slug) -> Result<Option<AgentDocument>> {
        let response = self
            .http
            .get(format!("{}/api/v1/agents/{agent}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch agent {agent}"))?;

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
        agent_ref: &Slug,
        agent: &AgentUpdateDocument,
    ) -> Result<AgentDocument> {
        let body = serde_json::to_value(agent).context("failed to encode agent update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/agents/{agent_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update agent {agent_ref}"))?;

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
        agent: &Slug,
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
            .patch(format!("{}/api/v1/agents/{agent}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update agent prompt {agent}"))?;

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

    /// Delete an agent document by slug.
    pub async fn delete_agent_document(&self, agent: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/agents/{agent}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete agent {agent}"))?;

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
    pub async fn fetch_ability_document(
        &self,
        ability: &ResourceRef,
    ) -> Result<Option<AbilityDocument>> {
        let selector = ability.as_path_segment();
        let response = self
            .http
            .get(format!("{}/api/v1/abilities/{selector}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch ability {ability}"))?;

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
        ability_ref: &ResourceRef,
        ability: &AbilityUpdateDocument,
    ) -> Result<AbilityDocument> {
        let selector = ability_ref.as_path_segment();
        let body =
            serde_json::to_value(ability).context("failed to encode ability update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/abilities/{selector}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update ability {ability_ref}"))?;

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
        ability: &ResourceRef,
        prompt_config: &nenjo::manifest::AbilityPromptConfig,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<AbilityPromptMutationResult> {
        let selector = ability.as_path_segment();
        let mut body = serde_json::json!({ "prompt_config": prompt_config });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/abilities/{selector}/prompt",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update ability prompt {ability}"))?;

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
    pub async fn delete_ability_document(&self, ability: &ResourceRef) -> Result<()> {
        let selector = ability.as_path_segment();
        let response = self
            .http
            .delete(format!("{}/api/v1/abilities/{selector}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete ability {ability}"))?;

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
        domain: &Slug,
        update: &DomainUpdateDocument,
    ) -> Result<DomainDocument> {
        let body = serde_json::to_value(update).context("failed to encode domain update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/domains/{domain}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update domain {domain}"))?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode updated domain"),
            status => bail!("domain update failed with status {status}"),
        }
    }

    /// Fetch a domain manifest document including prompt configuration.
    pub async fn get_domain_manifest_document(
        &self,
        domain: &Slug,
    ) -> Result<DomainPromptDocument> {
        let response = self
            .http
            .get(format!("{}/api/v1/domains/{domain}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to fetch domain prompt {domain}"))?;

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
        domain: &Slug,
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
            .patch(format!("{}/api/v1/domains/{domain}/prompt", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update domain prompt {domain}"))?;

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
    pub async fn delete_domain_document(&self, domain: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/domains/{domain}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete domain {domain}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("domain delete failed with status {status}"),
        }
    }

    /// Create a project manifest resource.
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
            .send()
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
            .send()
            .await
            .with_context(|| format!("failed to delete project {project}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("project delete failed with status {status}"),
        }
    }

    /// List library knowledge document metadata records for a project.
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
                .context("failed to decode library knowledge document metadata"),
            status => bail!("library knowledge document list failed with status {status}"),
        }
    }

    /// Fetch one library knowledge document metadata record.
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

    /// Fetch raw library knowledge document content from the platform.
    pub async fn fetch_project_document_content(
        &self,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<KnowledgeDocContentResponse> {
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
                format!("failed to fetch content for library knowledge document {document_id}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode library knowledge document content"),
            status => bail!("library knowledge document content fetch failed with status {status}"),
        }
    }

    /// List graph edges connected to a library knowledge document.
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
            .with_context(|| {
                format!("failed to list edges for library knowledge document {document_id}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode library knowledge document edges"),
            status => bail!("library knowledge document edge list failed with status {status}"),
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
        let content_type = item
            .content_type
            .clone()
            .unwrap_or_else(|| "text/plain".to_string());
        let file_part = multipart::Part::bytes(item.content.clone().into_bytes())
            .file_name(item.filename.clone())
            .mime_str(&content_type)
            .context("failed to encode library knowledge document mime type")?;
        let mut form = multipart::Form::new().part("file", file_part);
        form = form.text("item_id", doc_id.to_string());
        if let Some(doc) = item.doc.as_ref() {
            form = form.text("slug", doc.to_string());
        }
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
            .send()
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
            status => bail!("library knowledge document create failed with status {status}"),
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
            .send()
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
            status => {
                bail!("library knowledge document metadata update failed with status {status}")
            }
        }
    }

    /// Update the content for an existing library knowledge document.
    pub async fn update_knowledge_doc_content(
        &self,
        pack: &Slug,
        doc: &Slug,
        content: &str,
        encrypted_payload: Option<serde_json::Value>,
    ) -> Result<KnowledgeDocSummary> {
        let mut body = serde_json::json!({ "content": content });
        if let Some(encrypted_payload) = encrypted_payload {
            body["encrypted_payload"] = encrypted_payload;
        }

        let response = self
            .http
            .put(format!(
                "{}/api/v1/knowledge/{pack}/items/{doc}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
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
            status => {
                bail!("library knowledge document content update failed with status {status}")
            }
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
            .send()
            .await
            .with_context(|| {
                format!("failed to list edges for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK => response
                .json()
                .await
                .context("failed to decode library knowledge document edges"),
            status => bail!("library knowledge document edge list failed with status {status}"),
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
            .send()
            .await
            .with_context(|| {
                format!("failed to create edge for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => response
                .json()
                .await
                .context("failed to decode created library knowledge document edge"),
            status => bail!("library knowledge document edge create failed with status {status}"),
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
            .send()
            .await
            .with_context(|| {
                format!("failed to delete edge {edge_id} for library knowledge document {doc}")
            })?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("library knowledge document edge delete failed with status {status}"),
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
            .send()
            .await
            .with_context(|| format!("failed to delete library knowledge document {doc}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => Ok(()),
            status => bail!("library knowledge document delete failed with status {status}"),
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
            pairs.append_pair("project", query.project.as_str());
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
            if let Some(routine) = query.routine.as_ref() {
                pairs.append_pair("routine", routine.as_str());
            }
            if let Some(assigned_agent) = query.assigned_agent.as_ref() {
                pairs.append_pair("assigned_agent", assigned_agent.as_str());
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
            .with_context(|| format!("failed to list tasks for project {}", query.project))?;

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
            pairs.append_pair("project", query.project.as_str());
            if let Some(agent) = query.agent.as_ref() {
                pairs.append_pair("agent", agent.as_str());
            }
            if let Some(routine) = query.routine.as_ref() {
                pairs.append_pair("routine", routine.as_str());
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
                    query.project
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
    pub(crate) async fn create_routine_document(
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
                    let routine_ref = Slug::derive(&created.summary.name);
                    self.save_routine_graph_document(
                        &routine_ref,
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
    pub(crate) async fn update_routine_document(
        &self,
        routine_ref: &Slug,
        routine: &RoutineUpdateDocument,
    ) -> Result<RoutineDocument> {
        if let Some(graph) = routine.graph.as_ref() {
            return self
                .save_routine_graph_document(
                    routine_ref,
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
            .patch(format!("{}/api/v1/routines/{routine_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update routine {routine_ref}"))?;

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
        routine_ref: &Slug,
        body: &SaveRoutineGraphBody<'_>,
    ) -> Result<RoutineDocument> {
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/routines/{routine_ref}/graph",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to save routine graph for {routine_ref}"))?;

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

    /// Delete a routine document by slug.
    pub async fn delete_routine_document(&self, routine_ref: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/api/v1/routines/{routine_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
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
        model_ref: &Slug,
        model: &ModelUpdateDocument,
    ) -> Result<ModelDocument> {
        let body = serde_json::to_value(model).context("failed to encode model update patch")?;
        let response = self
            .http
            .patch(format!("{}/api/v1/models/{model_ref}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
            .await
            .with_context(|| format!("failed to remove member from council {council_ref}"))?;

        match response.status() {
            StatusCode::NO_CONTENT => self.fetch_council_document(council_ref).await,
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
        context_block_ref: &Slug,
        context_block: &ContextBlockUpdateDocument,
    ) -> Result<ContextBlockDocument> {
        let body = serde_json::to_value(context_block)
            .context("failed to encode context block update patch")?;
        let response = self
            .http
            .patch(format!(
                "{}/api/v1/context-blocks/{context_block_ref}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update context block {context_block_ref}"))?;

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
        context_block: &Slug,
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
                "{}/api/v1/context-blocks/{context_block}/content",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to update context block content {context_block}"))?;

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

    /// Delete a context block document by slug.
    pub async fn delete_context_block_document(&self, context_block: &Slug) -> Result<()> {
        let response = self
            .http
            .delete(format!(
                "{}/api/v1/context-blocks/{context_block}",
                self.base_url
            ))
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .with_context(|| format!("failed to delete context block {context_block}"))?;

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

    pub async fn list_knowledge_packs(&self) -> Result<Vec<PlatformKnowledgePackMetadata>> {
        let response = self
            .http
            .get(format!("{}/api/v1/knowledge", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send()
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

    pub async fn resolve_knowledge_pack_slug(&self, pack: &Slug) -> Result<Uuid> {
        self.list_knowledge_packs()
            .await?
            .into_iter()
            .find(|candidate| candidate.slug == *pack)
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
            .send()
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
            .find(|candidate| candidate.slug == *doc)
            .map(|candidate| candidate.id)
            .ok_or_else(|| anyhow!("knowledge document not found in pack {pack}: {doc}"))
    }
}

fn knowledge_doc_summary(
    pack: &Slug,
    document: KnowledgeDocMetadataResponse,
) -> KnowledgeDocSummary {
    KnowledgeDocSummary {
        pack: pack.clone(),
        doc: document.slug,
        filename: document.filename,
        path: document.path,
        title: document.title,
        kind: document.kind,
        summary: document.summary,
        tags: document.tags,
        content_type: document.content_type,
        updated_at: document.updated_at,
    }
}
