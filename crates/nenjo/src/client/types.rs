//! Request and response types for the Nenjo backend API.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::{CouncilManifest, CouncilMemberManifest};

/// Metadata for a project document, used during doc sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSyncMeta {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Council detail response (from GET /councils/{id}) → conversion to Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CouncilDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub delegation_strategy: String,
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
