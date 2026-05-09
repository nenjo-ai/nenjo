//! Request and response types for the Nenjo backend API.

use chrono::{DateTime, Utc};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::manifest::{
    AgentHeartbeatManifest, AgentManifest, CouncilDelegationStrategy, CouncilManifest,
    CouncilMemberManifest, DomainManifest, DomainPromptConfig, PromptConfig, RoutineEdgeCondition,
    RoutineEdgeManifest, RoutineManifest, RoutineMetadata, RoutineStepManifest, RoutineStepType,
    RoutineTrigger,
};

/// Metadata for a project document, used during doc sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSyncMeta {
    pub id: Uuid,
    pub filename: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub authority: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub content_type: String,
    pub size_bytes: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSyncContent {
    #[serde(default)]
    pub content: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSyncEdge {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_document_id: Uuid,
    pub target_document_id: Uuid,
    pub edge_type: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Agent detail response (from GET /agents/{id}) → conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AgentDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub color: String,
    pub model_id: Option<Uuid>,
    #[serde(default)]
    pub prompt_locked: bool,
    #[serde(default)]
    pub domain_ids: Vec<Uuid>,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_server_ids: Vec<Uuid>,
    #[serde(default)]
    pub ability_ids: Vec<Uuid>,
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatManifest>,
}

impl From<AgentDetailResponse> for AgentManifest {
    fn from(d: AgentDetailResponse) -> Self {
        Self {
            id: d.id,
            name: d.name,
            description: d.description,
            prompt_config: PromptConfig::default(),
            color: Some(d.color),
            model_id: d.model_id,
            domain_ids: d.domain_ids,
            platform_scopes: d.platform_scopes,
            mcp_server_ids: d.mcp_server_ids,
            ability_ids: d.ability_ids,
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
    pub delegation_strategy: CouncilDelegationStrategy,
    pub leader_agent_id: Uuid,
    pub members: Vec<CouncilMemberDetailResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilMemberDetailResponse {
    pub agent_id: Uuid,
    pub priority: i32,
    pub agent: CouncilAgentSummary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilAgentSummary {
    pub name: String,
}

impl From<CouncilDetailResponse> for CouncilManifest {
    fn from(d: CouncilDetailResponse) -> Self {
        Self {
            id: d.id,
            name: d.name,
            delegation_strategy: d.delegation_strategy,
            leader_agent_id: d.leader_agent_id,
            members: d
                .members
                .into_iter()
                .map(|m| CouncilMemberManifest {
                    agent_id: m.agent_id,
                    agent_name: m.agent.name,
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
    pub ability_ids: Vec<Uuid>,
    #[serde(default)]
    pub mcp_server_ids: Vec<Uuid>,
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
            ability_ids: d.ability_ids,
            mcp_server_ids: d.mcp_server_ids,
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
    pub description: Option<String>,
    pub trigger: RoutineTrigger,
    #[serde(default)]
    pub metadata: RoutineMetadata,
    #[serde(default)]
    pub steps: Vec<RoutineStepDetailResponse>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeDetailResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutineStepDetailResponse {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub name: String,
    pub step_type: RoutineStepType,
    pub council_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub config: serde_json::Value,
    pub order_index: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutineEdgeDetailResponse {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub source_step_id: Uuid,
    pub target_step_id: Uuid,
    pub condition: RoutineEdgeCondition,
}

impl From<RoutineDetailResponse> for RoutineManifest {
    fn from(d: RoutineDetailResponse) -> Self {
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
                    routine_id: step.routine_id,
                    name: step.name,
                    step_type: step.step_type,
                    council_id: step.council_id,
                    agent_id: step.agent_id,
                    config: step.config,
                    order_index: step.order_index,
                })
                .collect(),
            edges: d
                .edges
                .into_iter()
                .map(|edge| RoutineEdgeManifest {
                    id: edge.id,
                    routine_id: edge.routine_id,
                    source_step_id: edge.source_step_id,
                    target_step_id: edge.target_step_id,
                    condition: edge.condition,
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
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub schedule: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveAgentHeartbeatState {
    pub id: Uuid,
    pub agent_id: Uuid,
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
