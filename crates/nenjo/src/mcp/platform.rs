//! Platform tool resolver — resolves platform MCP tools by scope.
//!
//! The [`PlatformToolResolver`] trait abstracts how platform-scoped tools are
//! resolved. The built-in [`PlatformMcpResolver`] implementation connects to
//! the backend's `/mcp` endpoint and filters tools by scope.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

use nenjo_tools::Tool;

use super::client::{McpClient, McpTool};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Resolves platform tools for a given set of scopes.
///
/// Implementations map `platform_scopes` (e.g. `["tasks:read", "agents:write"]`)
/// to concrete [`Tool`] instances that the agent can invoke.
#[async_trait]
pub trait PlatformToolResolver: Send + Sync {
    /// Resolve tools available for the given platform scopes.
    ///
    /// Returns an empty vec if no tools match or if scopes is empty.
    async fn resolve_tools(&self, platform_scopes: &[String]) -> Vec<Arc<dyn Tool>>;
}

// ---------------------------------------------------------------------------
// Built-in implementation
// ---------------------------------------------------------------------------

/// Resolves platform tools by calling the backend's `/mcp` endpoint.
///
/// Constructed with `(base_url, api_key)` and shared across agents.
pub struct PlatformMcpResolver {
    client: Arc<McpClient>,
}

impl PlatformMcpResolver {
    /// Create a new resolver pointing at the backend.
    ///
    /// `base_url` is the backend URL (e.g. `http://localhost:3001`).
    /// `api_key` is the worker's API key for authentication.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Arc::new(McpClient::new(base_url, api_key)),
        }
    }
}

#[async_trait]
impl PlatformToolResolver for PlatformMcpResolver {
    async fn resolve_tools(&self, platform_scopes: &[String]) -> Vec<Arc<dyn Tool>> {
        if platform_scopes.is_empty() {
            return Vec::new();
        }

        let all_tools = match self.client.list_tools(Some(platform_scopes)).await {
            Ok(tools) => tools,
            Err(e) => {
                warn!(error = %e, "Failed to fetch platform MCP tools");
                return Vec::new();
            }
        };

        let tools: Vec<Arc<dyn Tool>> = all_tools
            .into_iter()
            .filter(|def| has_scope(platform_scopes, &def.scope))
            .map(|def| -> Arc<dyn Tool> { Arc::new(McpTool::new(def, self.client.clone())) })
            .collect();

        debug!(
            count = tools.len(),
            scopes = ?platform_scopes,
            "Platform tools resolved"
        );

        tools
    }
}

// ---------------------------------------------------------------------------
// Scope matching
// ---------------------------------------------------------------------------

/// Check if the given scopes grant access to the required scope.
///
/// Write scopes implicitly include read access for the same resource.
/// For example, `tasks:write` grants access to tools requiring `tasks:read`.
pub fn has_scope(scopes: &[String], required: &str) -> bool {
    if scopes.is_empty() {
        return true;
    }
    if scopes.iter().any(|s| s == required) {
        return true;
    }
    // Write implies read.
    if required.ends_with(":read") {
        let write_scope = required.replace(":read", ":write");
        return scopes.iter().any(|s| s == &write_scope);
    }
    false
}

// ---------------------------------------------------------------------------
// Noop implementation
// ---------------------------------------------------------------------------

/// A no-op resolver that always returns an empty tool set.
///
/// Useful in tests or configurations where platform MCP is not available.
pub struct NoopPlatformResolver;

#[async_trait]
impl PlatformToolResolver for NoopPlatformResolver {
    async fn resolve_tools(&self, _platform_scopes: &[String]) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_scope_empty_is_full_access() {
        assert!(has_scope(&[], "tasks:read"));
        assert!(has_scope(&[], "tasks:write"));
    }

    #[test]
    fn has_scope_write_implies_read() {
        let scopes = vec!["tasks:write".to_string()];
        assert!(has_scope(&scopes, "tasks:read"));
        assert!(has_scope(&scopes, "tasks:write"));
        assert!(!has_scope(&scopes, "projects:read"));
    }

    #[test]
    fn has_scope_exact_match() {
        let scopes = vec!["tasks:read".to_string()];
        assert!(has_scope(&scopes, "tasks:read"));
        assert!(!has_scope(&scopes, "tasks:write"));
    }

    #[test]
    fn has_scope_multiple() {
        let scopes = vec!["tasks:read".to_string(), "agents:write".to_string()];
        assert!(has_scope(&scopes, "tasks:read"));
        assert!(has_scope(&scopes, "agents:write"));
        assert!(has_scope(&scopes, "agents:read")); // write implies read
        assert!(!has_scope(&scopes, "tasks:write"));
        assert!(!has_scope(&scopes, "projects:read"));
    }
}
