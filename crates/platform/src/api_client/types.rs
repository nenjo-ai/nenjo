//! Request and response types for the Nenjo backend API.

use chrono::{DateTime, Utc};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use nenjo::Slug;
use nenjo::manifest::{
    AgentHeartbeatManifest, AgentManifest, CouncilDelegationStrategy, CouncilManifest,
    CouncilMemberManifest, DomainManifest, DomainPromptConfig, PromptConfig, RoutineEdgeCondition,
    RoutineEdgeManifest, RoutineManifest, RoutineMetadata, RoutineStepManifest, RoutineStepType,
    RoutineTrigger,
};

/// Metadata for a workspace knowledge pack, used during knowledge sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgePackSyncMeta {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_knowledge_pack_source_type")]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub updated_at: String,
}

fn default_knowledge_pack_source_type() -> String {
    "uploaded".to_string()
}

/// Metadata for a workspace knowledge document, used during knowledge sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSyncMeta {
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    pub pack_id: Option<Uuid>,
    #[serde(default)]
    pub pack_slug: String,
    pub slug: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSyncContent {
    #[serde(default)]
    pub content: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSyncEdge {
    pub id: Uuid,
    #[serde(default)]
    pub pack_id: Option<Uuid>,
    pub source_doc: Slug,
    #[serde(default)]
    pub source_item_id: Option<Uuid>,
    pub target_doc: Slug,
    #[serde(default)]
    pub target_item_id: Option<Uuid>,
    pub edge_type: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub type DocumentSyncMeta = KnowledgeDocSyncMeta;
pub type DocumentSyncContent = KnowledgeDocSyncContent;
pub type DocumentSyncEdge = KnowledgeDocSyncEdge;

// ---------------------------------------------------------------------------
// Project detail response (from GET /projects/{id}) -> conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
}

impl From<ProjectDetailResponse> for nenjo::manifest::ProjectManifest {
    fn from(project: ProjectDetailResponse) -> Self {
        Self {
            id: project.id,
            name: project.name,
            slug: Slug::derive(project.slug),
            description: project.description,
            settings: project.settings,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent detail response (from GET /agents/{id}) → conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AgentDetailResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub description: Option<String>,
    pub color: String,
    #[serde(default)]
    pub model: Option<Slug>,
    #[serde(default)]
    pub prompt_locked: bool,
    #[serde(default)]
    pub domains: Vec<Slug>,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatManifest>,
}

impl From<AgentDetailResponse> for AgentManifest {
    fn from(d: AgentDetailResponse) -> Self {
        Self {
            id: d.id,
            name: d.name,
            slug: d.slug.map(Slug::derive),
            description: d.description,
            prompt_config: PromptConfig::default(),
            color: Some(d.color),
            model: d.model,
            domains: d.domains,
            platform_scopes: d.platform_scopes,
            mcp_servers: d.mcp_servers,
            abilities: d.abilities,
            prompt_locked: d.prompt_locked,
            heartbeat: d.heartbeat,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentPromptConfigResponse {
    #[serde(default)]
    pub prompt_config: Option<PromptConfig>,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
}

// ---------------------------------------------------------------------------
// Council detail response (from GET /councils/{id}) → conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilDetailResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub delegation_strategy: CouncilDelegationStrategy,
    pub leader_agent: Slug,
    pub members: Vec<CouncilMemberDetailResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilMemberDetailResponse {
    pub agent: Slug,
    pub priority: i32,
    #[serde(default)]
    pub agent_detail: Option<CouncilAgentSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilAgentSummary {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
}

impl From<CouncilDetailResponse> for CouncilManifest {
    fn from(d: CouncilDetailResponse) -> Self {
        Self {
            id: d.id,
            name: d.name,
            delegation_strategy: d.delegation_strategy,
            leader_agent: d.leader_agent,
            members: d
                .members
                .into_iter()
                .map(|m| CouncilMemberManifest {
                    agent: m.agent,
                    priority: m.priority,
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Domain manifest response (from GET /domains/{id}/manifest) → conversion
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DomainManifestResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub display_name: String,
    pub description: Option<String>,
    pub command: String,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub prompt_config: DomainPromptConfig,
}

impl From<DomainManifestResponse> for DomainManifest {
    fn from(d: DomainManifestResponse) -> Self {
        Self {
            id: d.id,
            name: d.name,
            path: d.path,
            display_name: d.display_name,
            description: d.description,
            command: d.command,
            platform_scopes: d.platform_scopes,
            abilities: d.abilities,
            mcp_servers: d.mcp_servers,
            prompt_config: d.prompt_config,
        }
    }
}

// ---------------------------------------------------------------------------
// Context block responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ContextBlockSummaryResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextBlockContentResponse {
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
}

// ---------------------------------------------------------------------------
// Routine detail response (from GET /routines/{id}) → conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RoutineDetailResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub description: Option<String>,
    pub trigger: RoutineTrigger,
    #[serde(default)]
    pub metadata: RoutineMetadata,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
    #[serde(default)]
    pub steps: Vec<RoutineStepDetailResponse>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeDetailResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutineStepDetailResponse {
    pub id: Uuid,
    #[serde(default)]
    pub slug: Option<String>,
    pub name: String,
    pub step_type: RoutineStepType,
    #[serde(default)]
    pub council: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
    pub order_index: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutineEdgeDetailResponse {
    pub id: Uuid,
    pub source_step: String,
    pub target_step: String,
    pub condition: RoutineEdgeCondition,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl From<RoutineDetailResponse> for RoutineManifest {
    fn from(d: RoutineDetailResponse) -> Self {
        let routine_slug = d
            .slug
            .map(Slug::derive)
            .unwrap_or_else(|| Slug::derive(&d.name));
        Self {
            id: d.id,
            name: d.name,
            description: d.description,
            trigger: d.trigger,
            metadata: d.metadata,
            steps: d
                .steps
                .into_iter()
                .map(|step| RoutineStepManifest {
                    id: step.id,
                    slug: step
                        .slug
                        .map(Slug::derive)
                        .unwrap_or_else(|| Slug::derive(&step.name)),
                    routine: routine_slug.clone(),
                    name: step.name,
                    step_type: step.step_type,
                    council: step.council.map(Slug::derive),
                    agent: step.agent.map(Slug::derive),
                    config: step.config,
                    order_index: step.order_index,
                })
                .collect(),
            edges: d
                .edges
                .into_iter()
                .map(|edge| RoutineEdgeManifest {
                    id: edge.id,
                    routine: routine_slug.clone(),
                    source_step: Slug::derive(edge.source_step),
                    target_step: Slug::derive(edge.target_step),
                    condition: edge.condition,
                    metadata: edge.metadata,
                })
                .collect(),
        }
    }
}

/// Standard error envelope returned by the API.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveCronRoutineState {
    pub routine: Slug,
    pub project: Option<Slug>,
    pub schedule: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveAgentHeartbeatState {
    pub agent: Slug,
    pub interval: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEnrollmentRequest {
    pub api_key_id: Uuid,
    pub requested_at: DateTime<Utc>,
    pub crypto_version: u32,
    pub enc_public_key: String,
    pub sign_public_key: String,
    pub verification_code: String,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCertificate {
    pub account_id: Uuid,
    pub api_key_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub enc_public_key: String,
    pub sign_public_key: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedAccountContentKey {
    pub key_version: u32,
    pub algorithm: String,
    pub ephemeral_public_key: String,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedOrgContentKey {
    pub key_version: u32,
    pub algorithm: String,
    #[serde(default)]
    pub ephemeral_public_key: Option<String>,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEnrollmentState {
    Pending,
    Active,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEnrollmentStatusResponse {
    pub api_key_id: Uuid,
    #[serde(default)]
    pub metadata: Option<Value>,
    pub state: WorkerEnrollmentState,
    #[serde(default)]
    pub certificate: Option<WorkerCertificate>,
    #[serde(default)]
    pub user_wrapped_acks: HashMap<Uuid, WrappedAccountContentKey>,
    #[serde(default)]
    pub wrapped_ock: Option<WrappedOrgContentKey>,
}
