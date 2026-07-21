//! Shared contracts for model-visible async operation tools.
//!
//! Runtime crates own operation scheduling, cancellation, polling, and event
//! delivery. This module only defines stable tool names, argument DTOs, JSON
//! schemas, and operation lifecycle enums.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

pub const INSPECT_TOOL_NAME: &str = "inspect";
pub const SEND_INPUT_TOOL_NAME: &str = "send_input";
pub const STOP_TOOL_NAME: &str = "stop";
pub const WAIT_TOOL_NAME: &str = "wait";

/// A model-facing action supported by an async operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncControl {
    Inspect,
    SendInput,
    Stop,
    Wait,
}

/// Compact set of controls declared when an async operation starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AsyncControls(u8);

impl AsyncControls {
    pub const NONE: Self = Self(0);

    pub const fn new(control: AsyncControl) -> Self {
        Self(control.mask())
    }

    pub const fn with(self, control: AsyncControl) -> Self {
        Self(self.0 | control.mask())
    }

    pub const fn contains(self, control: AsyncControl) -> bool {
        self.0 & control.mask() != 0
    }

    pub fn iter(self) -> impl Iterator<Item = AsyncControl> {
        [
            AsyncControl::Inspect,
            AsyncControl::SendInput,
            AsyncControl::Stop,
            AsyncControl::Wait,
        ]
        .into_iter()
        .filter(move |control| self.contains(*control))
    }
}

impl AsyncControl {
    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::Inspect => INSPECT_TOOL_NAME,
            Self::SendInput => SEND_INPUT_TOOL_NAME,
            Self::Stop => STOP_TOOL_NAME,
            Self::Wait => WAIT_TOOL_NAME,
        }
    }

    const fn mask(self) -> u8 {
        match self {
            Self::Inspect => 1 << 0,
            Self::SendInput => 1 << 1,
            Self::Stop => 1 << 2,
            Self::Wait => 1 << 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationKind {
    Ability,
    Delegation,
    SubAgent,
    Shell,
    Media,
    TaskExecution,
}

impl AsyncOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ability => "ability",
            Self::Delegation => "delegation",
            Self::SubAgent => "sub_agent",
            Self::Shell => "shell",
            Self::Media => "media",
            Self::TaskExecution => "task_execution",
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

/// Canonical model-facing receipt returned after an async operation starts.
#[derive(Debug, Clone, Serialize)]
pub struct AsyncOperationStartReceipt {
    operation_id: String,
    kind: AsyncOperationKind,
    status: AsyncOperationStatus,
    control_tools: Vec<AsyncControlGuide>,
}

#[derive(Debug, Clone, Serialize)]
struct AsyncControlGuide {
    control: AsyncControl,
    tool: &'static str,
    instruction: &'static str,
    suggested_arguments: serde_json::Value,
}

impl AsyncOperationStartReceipt {
    pub fn new(
        operation_id: impl Into<String>,
        kind: AsyncOperationKind,
        controls: AsyncControls,
    ) -> Self {
        let operation_id = operation_id.into();
        let control_tools = controls
            .iter()
            .map(|control| AsyncControlGuide::new(control, &operation_id, kind))
            .collect();
        Self {
            operation_id,
            kind,
            status: AsyncOperationStatus::Running,
            control_tools,
        }
    }
}

impl AsyncControlGuide {
    fn new(control: AsyncControl, operation_id: &str, kind: AsyncOperationKind) -> Self {
        let (instruction, suggested_arguments) = match control {
            AsyncControl::Inspect => (
                "Use inspect to check this operation's current state or final output. Set include_transcript=true when recent activity is needed. A nested tool error while status is running is recoverable; use wait rather than stopping and restarting the operation.",
                json!({"operations": [operation_id]}),
            ),
            AsyncControl::SendInput => (
                "Use send_input only when this operation asks the parent agent for input.",
                json!({
                    "operations": [operation_id],
                    "message": "<response to the operation>"
                }),
            ),
            AsyncControl::Stop => (
                "Use stop to cancel this operation when it should no longer continue. Do not stop solely because a nested tool failed while the operation remains running.",
                json!({"operations": [operation_id]}),
            ),
            AsyncControl::Wait => (
                "Use wait while this operation is running; repeat until it completes, fails, stops, or asks for input. A recoverable_tool_error means the operation is still running and the default next action is wait.",
                json!({"seconds": 10, "kind": kind}),
            ),
        };
        Self {
            control,
            tool: control.tool_name(),
            instruction,
            suggested_arguments,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncOperationSignalKind {
    Started,
    Progress,
    RecoverableToolError,
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
            Self::RecoverableToolError => "recoverable_tool_error",
            Self::NeedsInput => "needs_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct InspectOperationsArgs {
    #[serde(default, deserialize_with = "deserialize_operation_ids")]
    pub operations: Vec<String>,
    #[serde(default)]
    pub kind: Option<AsyncOperationKind>,
    #[serde(default, deserialize_with = "deserialize_bool_or_string")]
    pub include_transcript: bool,
    #[serde(
        default = "default_inspect_limit",
        deserialize_with = "deserialize_usize_from_json_number"
    )]
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StopOperationsArgs {
    #[serde(default, deserialize_with = "deserialize_operation_ids")]
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
    #[serde(default, deserialize_with = "deserialize_operation_ids")]
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
        "enum": ["ability", "delegation", "sub_agent", "shell", "media", "task_execution"]
    })
}

fn default_inspect_limit() -> usize {
    30
}

fn deserialize_operation_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let array = match value {
        serde_json::Value::Array(values) => serde_json::Value::Array(values),
        serde_json::Value::String(encoded) => {
            let decoded = serde_json::from_str::<serde_json::Value>(&encoded).map_err(|_| {
                serde::de::Error::custom(
                    "expected an array of operation ids or a JSON-encoded array",
                )
            })?;
            if !decoded.is_array() {
                return Err(serde::de::Error::custom(
                    "expected an array of operation ids or a JSON-encoded array",
                ));
            }
            decoded
        }
        _ => {
            return Err(serde::de::Error::custom(
                "expected an array of operation ids or a JSON-encoded array",
            ));
        }
    };

    serde_json::from_value(array).map_err(serde::de::Error::custom)
}

fn deserialize_bool_or_string<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(value) => Ok(value),
        serde_json::Value::String(value) if value == "true" => Ok(true),
        serde_json::Value::String(value) if value == "false" => Ok(false),
        _ => Err(serde::de::Error::custom(
            "expected a boolean or the string \"true\" or \"false\"",
        )),
    }
}

pub fn deserialize_usize_from_json_number<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let number = deserialize_non_negative_whole_number(value)?;
    usize::try_from(number).map_err(serde::de::Error::custom)
}

pub fn deserialize_u64_from_json_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    deserialize_non_negative_whole_number(value)
}

fn deserialize_non_negative_whole_number<E>(value: serde_json::Value) -> Result<u64, E>
where
    E: serde::de::Error,
{
    match value {
        serde_json::Value::Number(number) => {
            if let Some(raw) = number.as_u64() {
                Ok(raw)
            } else if let Some(raw) = number.as_f64() {
                // `u64::MAX as f64` rounds up to 2^64. A strict comparison is
                // therefore required or the first out-of-range float silently
                // saturates to `u64::MAX` during the cast.
                if raw.is_finite() && raw.fract() == 0.0 && raw >= 0.0 && raw < u64::MAX as f64 {
                    Ok(raw as u64)
                } else {
                    Err(E::custom("expected a non-negative whole number"))
                }
            } else {
                Err(E::custom("expected a non-negative whole number"))
            }
        }
        serde_json::Value::String(raw)
            if !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            raw.parse::<u64>().map_err(E::custom)
        }
        other => Err(E::custom(format!(
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
    fn async_controls_track_explicit_capabilities() {
        let controls = AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait);

        assert!(controls.contains(AsyncControl::Inspect));
        assert!(controls.contains(AsyncControl::Wait));
        assert!(!controls.contains(AsyncControl::SendInput));
        assert!(!controls.contains(AsyncControl::Stop));
    }

    #[test]
    fn recoverable_tool_error_has_a_stable_wire_name() {
        assert_eq!(
            AsyncOperationSignalKind::RecoverableToolError.as_str(),
            "recoverable_tool_error"
        );
    }

    #[test]
    fn start_receipt_guides_recovery_without_restarting() {
        let receipt = AsyncOperationStartReceipt::new(
            "ability_build_routine_1",
            AsyncOperationKind::Ability,
            AsyncControls::new(AsyncControl::Inspect)
                .with(AsyncControl::Stop)
                .with(AsyncControl::Wait),
        );
        let value = serde_json::to_value(receipt).unwrap();
        let instructions = value["control_tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|control| control["instruction"].as_str())
            .collect::<Vec<_>>()
            .join(" ");

        assert!(instructions.contains("recoverable"));
        assert!(instructions.contains("Do not stop solely"));
        assert!(instructions.contains("default next action is wait"));
    }

    #[test]
    fn start_receipt_instructs_only_supported_controls() {
        let receipt = AsyncOperationStartReceipt::new(
            "media_generate_video_1",
            AsyncOperationKind::Media,
            AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait),
        );
        let value = serde_json::to_value(receipt).unwrap();

        assert_eq!(value["operation_id"], "media_generate_video_1");
        assert_eq!(value["kind"], "media");
        assert_eq!(value["status"], "running");
        assert_eq!(value["control_tools"].as_array().unwrap().len(), 2);
        assert_eq!(value["control_tools"][0]["tool"], "inspect");
        assert_eq!(
            value["control_tools"][0]["suggested_arguments"]["operations"][0],
            "media_generate_video_1"
        );
        assert_eq!(value["control_tools"][1]["tool"], "wait");
        assert_eq!(
            value["control_tools"][1]["suggested_arguments"]["kind"],
            "media"
        );
    }

    #[test]
    fn async_operation_kind_uses_wire_names() {
        assert_eq!(AsyncOperationKind::Ability.as_str(), "ability");
        assert_eq!(AsyncOperationKind::Delegation.as_str(), "delegation");
        assert_eq!(AsyncOperationKind::SubAgent.as_str(), "sub_agent");
        assert_eq!(AsyncOperationKind::Shell.as_str(), "shell");
        assert_eq!(AsyncOperationKind::Media.as_str(), "media");
        assert_eq!(AsyncOperationKind::TaskExecution.as_str(), "task_execution");
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
    fn wait_args_accept_digit_string_seconds_from_model_args() {
        let args: WaitOperationsArgs = serde_json::from_value(json!({
            "kind": "ability",
            "seconds": "10"
        }))
        .unwrap();

        assert_eq!(args.seconds, 10);
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
    fn numeric_coercion_accepts_u64_max_without_rounding() {
        #[derive(Deserialize)]
        struct Args {
            #[serde(deserialize_with = "deserialize_u64_from_json_number")]
            value: u64,
        }

        let integer: Args = serde_json::from_str(r#"{"value":18446744073709551615}"#).unwrap();
        let string: Args = serde_json::from_value(json!({
            "value": "18446744073709551615"
        }))
        .unwrap();

        assert_eq!(integer.value, u64::MAX);
        assert_eq!(string.value, u64::MAX);
    }

    #[test]
    fn numeric_coercion_rejects_values_at_or_above_two_to_the_64th() {
        #[derive(Debug, Deserialize)]
        struct Args {
            #[serde(deserialize_with = "deserialize_u64_from_json_number")]
            #[allow(dead_code)]
            value: u64,
        }

        let numeric =
            serde_json::from_str::<Args>(r#"{"value":1.8446744073709552e19}"#).unwrap_err();
        let string = serde_json::from_value::<Args>(json!({
            "value": "18446744073709551616"
        }))
        .unwrap_err();

        assert!(numeric.to_string().contains("whole number"));
        assert!(string.to_string().contains("number too large"));
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
    fn inspect_args_accept_json_encoded_model_values() {
        let args: InspectOperationsArgs = serde_json::from_value(json!({
            "operations": "[\"ability_build_routine_1\"]",
            "include_transcript": "true",
            "limit": "5"
        }))
        .unwrap();

        assert_eq!(args.operations, ["ability_build_routine_1"]);
        assert!(args.include_transcript);
        assert_eq!(args.limit, 5);
    }

    #[test]
    fn operation_controls_share_json_encoded_operation_id_compatibility() {
        let stop: StopOperationsArgs = serde_json::from_value(json!({
            "operations": "[\"ability_build_routine_1\"]"
        }))
        .unwrap();
        let send: SendOperationInputArgs = serde_json::from_value(json!({
            "operations": "[\"ability_build_routine_1\"]",
            "message": "continue"
        }))
        .unwrap();

        assert_eq!(stop.operations, ["ability_build_routine_1"]);
        assert_eq!(send.operations, ["ability_build_routine_1"]);
    }

    #[test]
    fn inspect_args_reject_ambiguous_string_values() {
        let operations_err = serde_json::from_value::<InspectOperationsArgs>(json!({
            "operations": "ability_build_routine_1"
        }))
        .unwrap_err();
        let bool_err = serde_json::from_value::<InspectOperationsArgs>(json!({
            "include_transcript": "yes"
        }))
        .unwrap_err();

        assert!(operations_err.to_string().contains("JSON-encoded array"));
        assert!(bool_err.to_string().contains("expected a boolean"));
    }

    #[test]
    fn inspect_args_reject_fractional_limit() {
        let err = serde_json::from_value::<InspectOperationsArgs>(json!({
            "limit": 5.5
        }))
        .unwrap_err();

        assert!(err.to_string().contains("whole number"));
    }

    #[test]
    fn wait_args_reject_non_digit_string_seconds() {
        for seconds in ["5.5", "-1", "ten"] {
            let err = serde_json::from_value::<WaitOperationsArgs>(json!({
                "seconds": seconds
            }))
            .unwrap_err();

            assert!(err.to_string().contains("whole number"));
        }
    }
}
