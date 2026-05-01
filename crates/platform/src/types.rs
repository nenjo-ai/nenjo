use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Bootstrap payload returned by the platform to seed a local manifest cache.
pub struct BootstrapManifestResponse {
    /// Account or user ID that owns the manifest snapshot.
    pub user_id: Uuid,
    /// API key ID associated with the bootstrap response, when provided by the platform.
    #[serde(default)]
    pub api_key_id: Option<Uuid>,
    /// Project resources included in the bootstrap snapshot.
    pub projects: Vec<PlatformManifestItem>,
    /// Routine resources included in the bootstrap snapshot.
    pub routines: Vec<PlatformManifestItem>,
    /// Model resources included in the bootstrap snapshot.
    pub models: Vec<PlatformManifestItem>,
    /// Agent resources included in the bootstrap snapshot.
    pub agents: Vec<PlatformManifestItem>,
    /// Council resources included in the bootstrap snapshot.
    pub councils: Vec<PlatformManifestItem>,
    /// Domain resources included in the bootstrap snapshot.
    pub domains: Vec<PlatformManifestItem>,
    /// Legacy lambda resources included in the bootstrap snapshot.
    pub lambdas: Vec<PlatformManifestItem>,
    /// MCP server resources included in the bootstrap snapshot.
    pub mcp_servers: Vec<PlatformManifestItem>,
    /// Ability resources included in the bootstrap snapshot.
    pub abilities: Vec<PlatformManifestItem>,
    /// Context block resources included in the bootstrap snapshot.
    pub context_blocks: Vec<PlatformManifestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One manifest resource in bootstrap or write-through transport form.
pub struct PlatformManifestItem {
    /// Resource ID.
    pub id: Uuid,
    /// Resource payload encoded as JSON.
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Generic write request body for manifest persistence endpoints.
pub struct PlatformManifestWriteRequest {
    /// Resource type identifier expected by the platform API.
    pub resource_type: String,
    /// Resource ID being written.
    pub resource_id: Uuid,
    /// Canonical resource payload.
    pub payload: serde_json::Value,
}
