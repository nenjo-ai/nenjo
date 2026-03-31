//! MCP integration — generic MCP client and platform tool resolution.
//!
//! - [`client`] — Generic MCP-over-HTTP client (JSON-RPC, tool discovery, tool calls)
//! - [`platform`] — Platform tool resolver trait and built-in implementation

pub mod client;
pub mod platform;

pub use client::{McpClient, McpTool, McpToolDef};
pub use platform::{NoopPlatformResolver, PlatformMcpResolver, PlatformToolResolver};
