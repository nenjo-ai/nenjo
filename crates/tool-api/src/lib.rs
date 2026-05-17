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
//!         Ok(ToolResult {
//!             success: true,
//!             output: args["message"].as_str().unwrap_or_default().to_string(),
//!             error: None,
//!         })
//!     }
//! }
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::path::PathBuf;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub content: String,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
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

    /// Whether calling this tool should immediately end the turn loop.
    fn is_terminal(&self) -> bool {
        false
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
                output: args["value"].as_str().unwrap_or_default().to_string(),
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
        assert_eq!(result.output, "hello");
    }

    #[test]
    fn tool_result_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error.as_deref(), Some("boom"));
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
