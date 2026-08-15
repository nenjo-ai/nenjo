//! Request and response types for the Nenjo backend API.

use chrono::{DateTime, Utc};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

pub use crate::manifest_contract::{
    AbilityPromptRecord, AgentPromptRecord, AgentRecord, ContextBlockContentRecord,
    ContextBlockRecord, CouncilRecord, DomainPromptRecord, DomainRecord,
    KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord, KnowledgePackRecord,
    ParsedKnowledgeDocument, RoutineRecord,
};

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

pub type DocumentSyncContent = KnowledgeDocSyncContent;

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

/// Worker-authorized committed review response used to resume a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHumanResolutionResponse {
    pub review_id: Uuid,
    pub execution_id: Uuid,
    pub version: i64,
    pub checkpoint_id: Uuid,
    pub checkpoint_payload_id: Uuid,
    pub decision: serde_json::Value,
    pub resolved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPayloadKind {
    ReviewInputs,
    Checkpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct PutExecutionPayloadRequest {
    pub execution_id: Uuid,
    pub kind: ExecutionPayloadKind,
    pub encrypted: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionPayloadResponse {
    pub id: Uuid,
    pub execution_id: Uuid,
    pub kind: String,
    pub encrypted: serde_json::Value,
    pub size_bytes: i64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewInputsReference {
    pub blob_id: Uuid,
    pub schemas: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PutReviewRequest {
    pub execution_id: Uuid,
    pub task_id: Uuid,
    pub step: String,
    pub round: u32,
    pub title: String,
    pub inputs: ReviewInputsReference,
    pub form: Option<serde_json::Value>,
    pub checkpoint_id: Uuid,
    pub artifact_ids: Vec<Uuid>,
    pub wait_for_review: bool,
}

/// Encrypted replacement checkpoint for requests that remain pending after a
/// parallel human decision advances the graph.
#[derive(Debug, Clone, Serialize)]
pub struct PutExecutionCheckpointRequest {
    pub execution_id: Uuid,
    pub contract: String,
    pub graph_revision: String,
    pub payload_id: Uuid,
    pub review_ids: Vec<Uuid>,
}
