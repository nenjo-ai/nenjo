use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Encrypted content payload exchanged between the platform and trusted endpoints.
///
/// The ciphertext is bound to the object identity via AEAD associated data
/// derived from `account_id`, `object_id`, and `object_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub account_id: Uuid,
    pub object_id: Uuid,
    pub object_type: String,
    pub algorithm: String,
    pub key_version: u32,
    pub nonce: String,
    pub ciphertext: String,
}

/// Content-bearing task execution fields carried in `task.execute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecuteContent {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
}

/// Encrypted task content fields stored outside the plaintext task row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskEncryptedContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
}
