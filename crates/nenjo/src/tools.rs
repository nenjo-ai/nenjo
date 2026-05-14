//! Tool trait and security types re-exported from `nenjo-tool-api`.

pub use tool_api::{
    Tool, ToolAutonomy, ToolCall, ToolCategory, ToolResult, ToolResultMessage, ToolSecurity,
    ToolSpec, sanitize_tool_name, sanitize_tool_name_lenient,
};
