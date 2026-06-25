//! Shared contracts for model-visible async operation tools.
//!
//! Runtime crates own operation scheduling, cancellation, polling, and event
//! delivery. This module only defines stable tool names, argument DTOs, JSON
//! schemas, and operation lifecycle enums.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

pub const WAIT_OPERATIONS_TOOL_NAME: &str = "wait_operations";
pub const INSPECT_OPERATIONS_TOOL_NAME: &str = "inspect_operations";
pub const STOP_OPERATIONS_TOOL_NAME: &str = "stop_operations";
pub const SEND_OPERATION_INPUT_TOOL_NAME: &str = "send_operation_input";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationKind {
    Ability,
    Delegation,
    SubAgent,
    Shell,
    Media,
}

impl AsyncOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ability => "ability",
            Self::Delegation => "delegation",
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
    #[serde(
        default = "default_inspect_limit",
        deserialize_with = "deserialize_usize_from_json_number"
    )]
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
    #[serde(
        default = "default_wait_seconds",
        deserialize_with = "deserialize_u64_from_json_number"
    )]
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
            "limit": {"type": "integer", "minimum": 1, "maximum": 50}
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
            "seconds": {"type": "integer", "minimum": 1, "maximum": 30},
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
        "enum": ["ability", "delegation", "sub_agent", "shell", "media"]
    })
}

fn default_inspect_limit() -> usize {
    30
}

pub fn deserialize_usize_from_json_number<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => {
            if let Some(raw) = number.as_u64() {
                usize::try_from(raw).map_err(serde::de::Error::custom)
            } else if let Some(raw) = number.as_f64() {
                if raw.is_finite() && raw.fract() == 0.0 && raw >= 0.0 {
                    usize::try_from(raw as u64).map_err(serde::de::Error::custom)
                } else {
                    Err(serde::de::Error::custom(
                        "expected a non-negative whole number",
                    ))
                }
            } else {
                Err(serde::de::Error::custom(
                    "expected a non-negative whole number",
                ))
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected a non-negative whole number, got {other}"
        ))),
    }
}

pub fn deserialize_u64_from_json_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => {
            if let Some(raw) = number.as_u64() {
                Ok(raw)
            } else if let Some(raw) = number.as_f64() {
                if raw.is_finite() && raw.fract() == 0.0 && raw >= 0.0 {
                    Ok(raw as u64)
                } else {
                    Err(serde::de::Error::custom(
                        "expected a non-negative whole number",
                    ))
                }
            } else {
                Err(serde::de::Error::custom(
                    "expected a non-negative whole number",
                ))
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected a non-negative whole number, got {other}"
        ))),
    }
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
        assert_eq!(AsyncOperationKind::Delegation.as_str(), "delegation");
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

    #[test]
    fn wait_args_accept_whole_float_seconds_from_model_args() {
        let args: WaitOperationsArgs = serde_json::from_value(json!({
            "kind": "ability",
            "seconds": 30.0
        }))
        .unwrap();

        assert_eq!(args.seconds, 30);
        assert_eq!(args.kind, Some(AsyncOperationKind::Ability));
    }

    #[test]
    fn wait_args_reject_fractional_seconds() {
        let err = serde_json::from_value::<WaitOperationsArgs>(json!({
            "seconds": 5.5
        }))
        .unwrap_err();

        assert!(err.to_string().contains("whole number"));
    }

    #[test]
    fn inspect_args_accept_whole_float_limit_from_model_args() {
        let args: InspectOperationsArgs = serde_json::from_value(json!({
            "operations": ["ability_build_agent_2"],
            "include_transcript": true,
            "limit": 5.0
        }))
        .unwrap();

        assert_eq!(args.limit, 5);
    }

    #[test]
    fn inspect_args_reject_fractional_limit() {
        let err = serde_json::from_value::<InspectOperationsArgs>(json!({
            "limit": 5.5
        }))
        .unwrap_err();

        assert!(err.to_string().contains("whole number"));
    }
}
