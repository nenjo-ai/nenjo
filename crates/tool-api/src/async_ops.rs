//! Shared contracts for model-visible async operation tools.
//!
//! Runtime crates own operation scheduling, cancellation, polling, and event
//! delivery. This module only defines stable tool names, argument DTOs, JSON
//! schemas, and operation lifecycle enums.

use serde::{Deserialize, Serialize};
use serde_json::json;

pub const WAIT_OPERATIONS_TOOL_NAME: &str = "wait_operations";
pub const INSPECT_OPERATIONS_TOOL_NAME: &str = "inspect_operations";
pub const STOP_OPERATIONS_TOOL_NAME: &str = "stop_operations";
pub const SEND_OPERATION_INPUT_TOOL_NAME: &str = "send_operation_input";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationKind {
    Ability,
    SubAgent,
    Shell,
    Media,
}

impl AsyncOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ability => "ability",
            Self::SubAgent => "sub_agent",
            Self::Shell => "shell",
            Self::Media => "media",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Stopped,
}

impl AsyncOperationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn can_receive_input(self) -> bool {
        matches!(self, Self::Running | Self::WaitingForInput)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationSignalKind {
    Started,
    Progress,
    NeedsInput,
    Completed,
    Failed,
    Stopped,
}

impl AsyncOperationSignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Progress => "progress",
            Self::NeedsInput => "needs_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct InspectOperationsArgs {
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub kind: Option<AsyncOperationKind>,
    #[serde(default)]
    pub include_transcript: bool,
    #[serde(default = "default_inspect_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopOperationsArgs {
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub kind: Option<AsyncOperationKind>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitOperationsArgs {
    #[serde(default = "default_wait_seconds")]
    pub seconds: u64,
    #[serde(default)]
    pub kind: Option<AsyncOperationKind>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendOperationInputArgs {
    #[serde(default)]
    pub operations: Vec<String>,
    pub message: String,
}

pub fn inspect_operations_parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "operations": {"type": "array", "items": {"type": "string"}},
            "kind": operation_kind_schema(),
            "include_transcript": {"type": "boolean"},
            "limit": {"type": "number", "minimum": 1, "maximum": 50}
        },
        "additionalProperties": false
    })
}

pub fn stop_operations_parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "operations": {"type": "array", "items": {"type": "string"}},
            "kind": operation_kind_schema(),
            "reason": {"type": "string"}
        },
        "additionalProperties": false
    })
}

pub fn wait_operations_parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "seconds": {"type": "number", "minimum": 1, "maximum": 30},
            "kind": operation_kind_schema(),
            "reason": {"type": "string"}
        },
        "additionalProperties": false
    })
}

pub fn send_operation_input_parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "operations": {"type": "array", "items": {"type": "string"}},
            "message": {"type": "string"}
        },
        "required": ["operations", "message"],
        "additionalProperties": false
    })
}

pub fn operation_kind_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "enum": ["ability", "sub_agent", "shell", "media"]
    })
}

fn default_inspect_limit() -> usize {
    30
}

fn default_wait_seconds() -> u64 {
    10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_operation_kind_uses_wire_names() {
        assert_eq!(AsyncOperationKind::Ability.as_str(), "ability");
        assert_eq!(AsyncOperationKind::SubAgent.as_str(), "sub_agent");
        assert_eq!(AsyncOperationKind::Shell.as_str(), "shell");
        assert_eq!(AsyncOperationKind::Media.as_str(), "media");
    }

    #[test]
    fn wait_args_deserialize_with_defaults() {
        let args: WaitOperationsArgs = serde_json::from_value(json!({})).unwrap();

        assert_eq!(args.seconds, 10);
        assert_eq!(args.kind, None);
    }
}
