//! Tool re-exports and factory for the worker.
//!
//! Re-exports the SDK `Tool` trait, owns the worker runtime tool
//! implementations, and provides a `WorkerToolFactory` that builds per-agent
//! tool sets.

// Re-export core tool types.
pub use nenjo::{Tool, Tool as ToolTrait, ToolCategory, ToolResult, ToolSpec};

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
pub mod memory;
pub mod memory_forget;
pub mod memory_recall;
pub mod memory_store;
pub mod native_runtime;
pub(crate) mod platform_payload;
pub(crate) mod platform_services;
pub mod runtime;
pub mod screenshot;
pub mod security;
pub mod shell;
pub mod web_fetch;
pub mod web_search_tool;

// Re-export built-in tool implementations.
pub use browser::BrowserTool;
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
pub use native_runtime::NativeRuntime;
pub use runtime::RuntimeAdapter;
pub use screenshot::ScreenshotTool;
pub use security::{AutonomyLevel, SecurityPolicy};
pub use shell::ShellTool;
pub use web_fetch::WebFetchTool;
pub use web_search_tool::WebSearchTool;

// Re-export per-ability tool type from nenjo SDK.
pub use nenjo::agents::abilities::AssignedAbilityTool;

mod factory;
#[cfg(test)]
mod factory_tests;
pub use factory::WorkerToolFactory;
