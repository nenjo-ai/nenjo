//! Shared tool contracts for Nenjo agents, model providers, and runtimes.
//!
//! This crate owns the common tool API surface used across the Nenjo workspace.
//! It is deliberately independent from the rest of the workspace so model
//! integrations, SDK code, and worker runtimes can agree on tool schemas and
//! execution results without depending on each other.
//!
//! The main entry points are:
//!
//! - [`Tool`], the async trait implemented by concrete tool runtimes.
//! - [`ToolSpec`], the JSON-schema-backed metadata sent to model providers.
//! - [`ToolCategory`], the side-effect classification used for guidance and
//!   filtering.
//! - [`ToolCall`], [`ToolResult`], and [`ToolResultMessage`], the request and
//!   result payloads that flow through tool execution.
//! - [`ToolAutonomy`] and [`ToolSecurity`], the SDK-level policy inputs used
//!   when constructing tools.
//!
//! # Example
//!
//! ```rust
//! use async_trait::async_trait;
//! use serde_json::json;
//! use nenjo_tool_api::{Tool, ToolCategory, ToolResult};
//!
//! struct EchoTool;
//!
//! #[async_trait]
//! impl Tool for EchoTool {
//!     fn name(&self) -> &str {
//!         "echo"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Echoes a message back to the caller."
//!     }
//!
//!     fn parameters_schema(&self) -> serde_json::Value {
//!         json!({
//!             "type": "object",
//!             "properties": {
//!                 "message": { "type": "string" }
//!             },
//!             "required": ["message"]
//!         })
//!     }
//!
//!     fn category(&self) -> ToolCategory {
//!         ToolCategory::Read
//!     }
//!
//!     async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
//!         Ok(ToolResult::success(
//!             args["message"].as_str().unwrap_or_default(),
//!         ))
//!     }
//! }
//! ```

use async_trait::async_trait;
pub use nenjo_content::{
    ArtifactId, ArtifactInput, ArtifactInputSource, ArtifactInstruction, ArtifactRef, ArtifactSize,
    MediaType, Sha256Digest,
};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::path::PathBuf;

pub mod async_ops;

pub use async_ops::{
    AsyncControl, AsyncControls, AsyncOperationKind, AsyncOperationSignalKind,
    AsyncOperationStartReceipt, AsyncOperationStatus, INSPECT_TOOL_NAME, InspectOperationsArgs,
    SEND_INPUT_TOOL_NAME, STOP_TOOL_NAME, SendOperationInputArgs, StopOperationsArgs,
    WAIT_TOOL_NAME, WaitOperationsArgs, deserialize_u64_from_json_number,
    deserialize_usize_from_json_number, inspect_operations_parameters_schema,
    send_operation_input_parameters_schema, stop_operations_parameters_schema,
    wait_operations_parameters_schema,
};

/// Classifies a tool's side-effect profile for filtering and model guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// Pure read/search with no persistent side effects.
    Read,
    /// Mutates files, state, or external systems.
    #[default]
    Write,
    /// Both read and write sub-operations.
    ReadWrite,
}

impl ToolCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "READ",
            Self::Write => "WRITE",
            Self::ReadWrite => "READ/WRITE",
        }
    }

    pub fn guidance(self) -> &'static str {
        match self {
            Self::Read => "Inspects or verifies state without persistent side effects.",
            Self::Write => {
                "Mutates persistent state. Use sparingly and avoid repeated calls in one turn."
            }
            Self::ReadWrite => {
                "Can read and mutate state. Use carefully and avoid repeated calls in one turn."
            }
        }
    }

    pub fn is_write_like(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// Full specification of a tool for LLM registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub category: ToolCategory,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Display for ToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "name={} arguments={}", self.name, self.arguments)
    }
}

/// A tool result to feed back to the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub output: ToolOutput,
}

impl ToolResultMessage {
    pub fn text(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            output: ToolOutput::text(content),
        }
    }

    pub fn new(tool_call_id: impl Into<String>, output: ToolOutput) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            output,
        }
    }

    pub fn with_artifact(mut self, artifact: ArtifactRef) -> Self {
        self.output.push_artifact(artifact);
        self
    }
}

/// One ordered part of a tool's durable output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ToolOutputPart {
    Text(String),
    Artifact(ArtifactRef),
}

/// Ordered, serializable tool output without decrypted bytes or host paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolOutput(Vec<ToolOutputPart>);

impl ToolOutput {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self(vec![ToolOutputPart::Text(value.into())])
    }

    pub fn from_parts(parts: Vec<ToolOutputPart>) -> Self {
        Self(parts)
    }

    pub fn parts(&self) -> &[ToolOutputPart] {
        &self.0
    }

    pub fn parts_mut(&mut self) -> &mut [ToolOutputPart] {
        &mut self.0
    }

    pub fn push_text(&mut self, value: impl Into<String>) {
        self.0.push(ToolOutputPart::Text(value.into()));
    }

    pub fn push_artifact(&mut self, artifact: ArtifactRef) {
        self.0.push(ToolOutputPart::Artifact(artifact));
    }

    /// Retain artifact parts selected for an ephemeral model request.
    /// Text parts are always preserved.
    pub fn retain_artifacts(&mut self, mut retain: impl FnMut(&ArtifactRef) -> bool) {
        self.0.retain(|part| match part {
            ToolOutputPart::Text(_) => true,
            ToolOutputPart::Artifact(artifact) => retain(artifact),
        });
    }

    pub fn has_artifacts(&self) -> bool {
        self.0
            .iter()
            .any(|part| matches!(part, ToolOutputPart::Artifact(_)))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
            || self.0.iter().all(|part| match part {
                ToolOutputPart::Text(text) => text.is_empty(),
                ToolOutputPart::Artifact(_) => false,
            })
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn contains(&self, pattern: &str) -> bool {
        self.text_content().contains(pattern)
    }

    /// Concatenate textual parts for logs, previews, and text-only UI surfaces.
    pub fn text_content(&self) -> String {
        self.0
            .iter()
            .filter_map(|part| match part {
                ToolOutputPart::Text(text) => Some(text.as_str()),
                ToolOutputPart::Artifact(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Borrow the sole text part without allocating.
    pub fn as_text(&self) -> Option<&str> {
        match self.0.as_slice() {
            [ToolOutputPart::Text(text)] => Some(text),
            [] | [ToolOutputPart::Artifact(_)] | [_, _, ..] => None,
        }
    }

    pub fn len(&self) -> usize {
        self.text_content().len()
    }
}

impl From<String> for ToolOutput {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for ToolOutput {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl Display for ToolOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text_content())
    }
}

impl PartialEq<&str> for ToolOutput {
    fn eq(&self, other: &&str) -> bool {
        self.text_content() == *other
    }
}

impl PartialEq<ToolOutput> for &str {
    fn eq(&self, other: &ToolOutput) -> bool {
        other == self
    }
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: ToolOutput,
    pub error: Option<String>,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: ToolOutput::text(output),
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: ToolOutput::empty(),
            error: Some(error.into()),
        }
    }

    pub fn with_artifact(mut self, artifact: ArtifactRef) -> Self {
        self.output.push_artifact(artifact);
        self
    }

    pub fn with_artifacts(mut self, artifacts: Vec<ArtifactRef>) -> Self {
        for artifact in artifacts {
            self.output.push_artifact(artifact);
        }
        self
    }
}

/// Runtime ownership surface for a tool.
///
/// This is intentionally separate from read/write category. It describes who
/// owns tool availability and scoping so runtimes can rebuild or inherit tools
/// correctly across abilities, domains, and sub-agents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOrigin {
    /// Host/runtime tools such as shell, files, git, web, and memory.
    #[default]
    Host,
    /// External MCP server tools governed by an agent or ability MCP assignment.
    Mcp,
    /// Platform resource tools whose availability is governed by platform scopes.
    Platform,
    /// Harness orchestration tools such as ability or sub-agent control.
    Harness,
}

/// Core tool trait for agent capabilities.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name used in LLM function calling.
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Tool category for profile-based filtering.
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    /// Runtime ownership surface for this tool.
    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Host
    }

    /// Whether calling this tool should immediately end the turn loop.
    fn is_terminal(&self) -> bool {
        false
    }

    /// Whether this tool should be advertised to the model for the next request.
    ///
    /// Tools remain registered for execution when hidden. Runtime control tools
    /// can use this hook to appear only after their corresponding capability has
    /// been activated.
    async fn is_available_to_model(&self) -> bool {
        true
    }

    /// Build the full spec for LLM registration.
    fn spec(&self) -> ToolSpec {
        let category = self.category();
        ToolSpec {
            name: self.name().to_string(),
            description: format!(
                "[{}] {} {}",
                category.label(),
                category.guidance(),
                self.description()
            ),
            parameters: self.parameters_schema(),
            category,
        }
    }
}

/// High-level autonomy requested while constructing runtime tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolAutonomy {
    ReadOnly,
    #[default]
    Supervised,
    Full,
}

/// SDK-level tool construction policy.
///
/// Concrete runtimes can translate this into their own enforcement policy.
#[derive(Debug, Clone)]
pub struct ToolSecurity {
    pub autonomy: ToolAutonomy,
    pub workspace_dir: PathBuf,
    pub forwarded_env_names: Vec<String>,
}

impl Default for ToolSecurity {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        Self {
            autonomy: ToolAutonomy::Supervised,
            workspace_dir: home.join(".nenjo").join("workspace"),
            forwarded_env_names: Vec::new(),
        }
    }
}

impl ToolSecurity {
    pub fn with_workspace_dir(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            ..Default::default()
        }
    }
}

/// Sanitize a tool function name to match the strict OpenAI pattern
/// `^[a-zA-Z0-9_-]+$`.
///
/// Used by OpenAI, DeepSeek, and other strict providers. Replaces dots, slashes,
/// and any other disallowed characters with `_`.
pub fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Light sanitization for lenient providers (Ollama) while preserving dots used
/// in MCP namespaced tool names.
pub fn sanitize_tool_name_lenient(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }

        fn description(&self) -> &str {
            "A test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
                    .into(),
                error: None,
            })
        }
    }

    #[test]
    fn spec_uses_tool_metadata() {
        let spec = DummyTool.spec();
        assert_eq!(spec.name, "dummy");
        assert_eq!(spec.category, ToolCategory::Write);
    }

    #[tokio::test]
    async fn execute_returns_output() {
        let result = DummyTool
            .execute(serde_json::json!({"value": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output.text_content(), "hello");
    }

    #[test]
    fn tool_result_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new().into(),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn tool_result_artifacts_are_immutable_refs_without_input_provenance() {
        let artifact = ArtifactRef::new(
            nenjo_content::ArtifactId::parse(uuid::Uuid::new_v4()).unwrap(),
            nenjo_content::Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            nenjo_content::MediaType::parse("image/png").unwrap(),
            nenjo_content::ArtifactSize::new(12),
        );
        let encoded =
            serde_json::to_value(ToolResult::success("image ready").with_artifact(artifact))
                .unwrap();

        let artifact = &encoded["output"][1]["value"];
        assert_eq!(artifact["media_type"], "image/png");
        assert!(artifact.get("source").is_none());
        assert!(artifact.get("bytes").is_none());
        assert!(artifact.get("path").is_none());
    }

    #[test]
    fn sanitize_tool_name_replaces_dots_and_slashes() {
        assert_eq!(
            sanitize_tool_name("app.nenjo.platform/tasks"),
            "app_nenjo_platform_tasks"
        );
    }

    #[test]
    fn sanitize_tool_name_preserves_valid_chars() {
        assert_eq!(sanitize_tool_name("my-tool_v2"), "my-tool_v2");
    }

    #[test]
    fn sanitize_tool_name_lenient_preserves_dots() {
        assert_eq!(
            sanitize_tool_name_lenient("app.nenjo.platform/tasks"),
            "app.nenjo.platform_tasks"
        );
    }
}
