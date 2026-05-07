//! # nenjo-tools
//!
//! Tool trait, types, and built-in tool implementations for the Nenjo agent platform.
//!
//! This crate provides:
//! - The [`Tool`] trait for defining agent capabilities
//! - Supporting types: [`SecurityPolicy`], [`RuntimeAdapter`], [`AgentMemory`]
//! - Built-in tool implementations (shell, file I/O, git, search, web, memory, etc.)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Supporting modules ───────────────────────────────────────────

pub mod memory;
pub mod runtime;
pub mod security;

// ── Tool implementations ─────────────────────────────────────────

pub mod browser;
pub mod browser_open;
pub mod content_search;
pub mod file_delete;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod git_operations;
pub mod glob_search;
pub mod http_request;
pub mod memory_forget;
pub mod memory_recall;
pub mod memory_store;
pub mod screenshot;
pub mod shell;
pub mod web_fetch;
pub mod web_search_tool;

// ── Re-exports ───────────────────────────────────────────────────

pub use browser::{BrowserTool, ComputerUseConfig};
pub use browser_open::BrowserOpenTool;
pub use content_search::ContentSearchTool;
pub use file_delete::FileDeleteTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use git_operations::GitOperationsTool;
pub use glob_search::GlobSearchTool;
pub use http_request::HttpRequestTool;
pub use memory_forget::MemoryForgetTool;
pub use memory_recall::MemoryRecallTool;
pub use memory_store::MemoryStoreTool;
pub use screenshot::ScreenshotTool;
pub use shell::ShellTool;
pub use web_fetch::WebFetchTool;
pub use web_search_tool::WebSearchTool;

// ── Core types ───────────────────────────────────────────────────

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Classifies a tool's side-effect profile for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// Pure read/search — no side effects.
    Read,
    /// Mutates files, state, or external systems.
    #[default]
    Write,
    /// Both read and write sub-operations (e.g. shell, git).
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

/// Core tool trait — implement for any agent capability.
///
/// Tools are invoked by the agent turn loop when the LLM emits a function
/// call matching [`Tool::name`]. The JSON arguments from the LLM are passed
/// to [`Tool::execute`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in LLM function calling).
    fn name(&self) -> &str;

    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Tool category for profile-based filtering.
    ///
    /// Defaults to [`ToolCategory::Write`] (safe default — tools must opt-in
    /// to [`ToolCategory::Read`]).
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    /// Whether calling this tool should immediately end the turn loop.
    ///
    /// When `true`, the turn loop will stop after executing this tool without
    /// pushing the tool result back into the conversation. This is useful for
    /// tools like `pass_verdict` where the structured arguments are the signal
    /// and no further LLM interaction is needed.
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
}
