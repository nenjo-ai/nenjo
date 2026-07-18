use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TaskScheduleDefinition;

/// The single agent or routine target selected for task execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "slug", rename_all = "snake_case")]
pub enum TaskExecutionTarget {
    Agent(String),
    Routine(String),
}

/// Encrypted content payload exchanged between the platform and trusted endpoints.
///
/// The ciphertext is bound to the object identity via AEAD associated data
/// derived from `account_id`, `object_id`, and `object_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub account_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_scope: Option<String>,
    pub object_id: Uuid,
    pub object_type: String,
    pub algorithm: String,
    pub key_version: u32,
    pub nonce: String,
    pub ciphertext: String,
}

/// Content-bearing task fields carried in `task.execute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecuteContent {
    pub title: String,
    /// Decrypted task instructions available after secure-envelope decoding.
    /// Command producers must place this value in [`TaskEncryptedContent`], not
    /// in the plaintext `task.execute.payload` object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// One task schedule distributed to every worker as control-plane state.
///
/// All workers cache assignments, but only a worker with the exclusive `cron`
/// capability materializes occurrences from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScheduleAssignment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub authorized_by_user_id: Uuid,
    pub definition: TaskScheduleDefinition,
    #[serde(default)]
    pub occurrence_count: u32,
    pub next_run_at: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub target: TaskExecutionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<TaskExecuteContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
    #[serde(default)]
    pub runnable: bool,
    pub updated_at: String,
}

/// Normalized encrypted task content stored outside the plaintext task row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskEncryptedContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{TaskEncryptedContent, TaskExecuteContent};

    #[test]
    fn task_execute_content_round_trips() {
        let content: TaskExecuteContent = serde_json::from_value(serde_json::json!({
            "title": "Current",
            "instructions": "current instructions",
            "labels": ["bug"]
        }))
        .unwrap();

        assert_eq!(
            content.instructions.as_deref(),
            Some("current instructions")
        );
        assert_eq!(content.labels, ["bug"]);
        let serialized = serde_json::to_value(content).unwrap();
        assert_eq!(serialized["instructions"], "current instructions");
        assert_eq!(serialized["labels"], serde_json::json!(["bug"]));
    }

    #[test]
    fn encrypted_task_content_round_trips() {
        let content: TaskEncryptedContent = serde_json::from_value(serde_json::json!({
            "instructions": "new"
        }))
        .unwrap();

        assert_eq!(content.instructions.as_deref(), Some("new"));
        assert_eq!(
            serde_json::to_value(content).unwrap(),
            serde_json::json!({
                "instructions": "new"
            })
        );
    }
}
