//! Request and response types for the Nenjo backend API.

use chrono::{DateTime, Utc};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

pub use crate::manifest_contract::{
    AgentPromptRecord, AgentRecord, ContextBlockContentRecord, ContextBlockRecord, CouncilRecord,
    DomainPromptRecord, DomainRecord, KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord,
    ParsedKnowledgeDocument, RoutineRecord,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveCronRoutineState {
    pub routine: nenjo::Slug,
    pub project: Option<nenjo::Slug>,
    pub schedule: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveAgentHeartbeatState {
    pub agent: nenjo::Slug,
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