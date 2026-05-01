//! Tool re-exports and factory for the harness.
//!
//! Re-exports the `Tool` trait and built-in tools from `nenjo-tools`, and
//! provides a `HarnessToolFactory` that builds per-agent tool sets.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use nenjo::ToolFactory;
use nenjo::builtin_knowledge::{
    BuiltinDocAuthority, BuiltinDocEdgeType, BuiltinDocFilter, BuiltinDocKind, BuiltinDocStatus,
    builtin_knowledge_pack,
};
use nenjo::manifest::AgentManifest;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::manifest::store::ManifestReader;
use nenjo_platform::{
    ManifestAccessPolicy, ManifestMcpBackend, ManifestMcpContract, PlatformManifestBackend,
    PlatformManifestClient, ScopeResource, SensitivePayloadEncoder,
    client::{
        CreateExecutionRequest, ProjectDocumentEdge, ProjectDocumentMetadata,
        ProjectExecutionListQuery, ProjectTaskListQuery,
    },
};
use nenjo_tools::security::SecurityPolicy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::crypto::provider::WorkerAuthProvider;
use crate::crypto::{decrypt_text, encrypt_text};

// Re-export core tool types.
pub use nenjo_tools::{Tool, Tool as ToolTrait, ToolCategory, ToolResult, ToolSpec};

// Re-export built-in tool implementations.
pub use nenjo_tools::{
    BrowserOpenTool, BrowserTool, ContentSearchTool, FileEditTool, FileReadTool, FileWriteTool,
    GitOperationsTool, GlobSearchTool, HttpRequestTool, MemoryForgetTool, MemoryRecallTool,
    MemoryStoreTool, ScreenshotTool, ShellTool, WebFetchTool, WebSearchTool,
};

// Re-export per-ability tool type from nenjo SDK.
pub use nenjo::agents::abilities::AssignedAbilityTool;

/// A tool factory that builds per-agent tool sets for the harness.
///
/// Uses the agent's configuration, security policy, MCP server pool, and
/// manifest backend to build a complete tool set per agent.
pub struct HarnessToolFactory {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter>,
    config: crate::harness::config::Config,
    external_mcp: Arc<crate::harness::external_mcp::ExternalMcpPool>,
    manifest_store: Arc<LocalManifestStore>,
    platform_client: Option<Arc<PlatformManifestClient>>,
    payload_encoder: WorkerAgentPromptPayloadEncoder,
    manifest_backend:
        Option<Arc<PlatformManifestBackend<LocalManifestStore, WorkerAgentPromptPayloadEncoder>>>,
}

#[derive(Clone)]
struct WorkerAgentPromptPayloadEncoder {
    state_dir: std::path::PathBuf,
}

#[async_trait]
impl SensitivePayloadEncoder for WorkerAgentPromptPayloadEncoder {
    async fn encode_payload(
        &self,
        account_id: uuid::Uuid,
        object_id: uuid::Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let auth_provider = WorkerAuthProvider::load_or_create(self.state_dir.join("crypto"))
            .context("failed to load worker auth provider")?;
        let ack = auth_provider
            .load_ack()
            .await?
            .ok_or_else(|| anyhow!("worker has no enrolled ACK"))?;
        let key_version = auth_provider.current_key_version().await.unwrap_or(1);
        let encrypted_payload = encrypt_text(
            &ack,
            account_id,
            object_id,
            object_type,
            &serde_json::to_string(payload)?,
            key_version,
        )?;
        Ok(Some(serde_json::to_value(encrypted_payload)?))
    }

    async fn decode_payload(
        &self,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let encrypted_payload: nenjo_events::EncryptedPayload =
            serde_json::from_value(payload.clone()).context("invalid encrypted payload JSON")?;
        let auth_provider = WorkerAuthProvider::load_or_create(self.state_dir.join("crypto"))
            .context("failed to load worker auth provider")?;
        let ack = auth_provider
            .load_ack()
            .await?
            .ok_or_else(|| anyhow!("worker has no enrolled ACK"))?;
        let plaintext = decrypt_text(&ack, &encrypted_payload)?;
        Ok(Some(serde_json::from_str(&plaintext)?))
    }
}

impl HarnessToolFactory {
    pub fn new(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter>,
        config: crate::harness::config::Config,
        external_mcp: Arc<crate::harness::external_mcp::ExternalMcpPool>,
    ) -> Self {
        let local_store = Arc::new(LocalManifestStore::new(config.manifests_dir.clone()));
        let payload_encoder = WorkerAgentPromptPayloadEncoder {
            state_dir: config.state_dir.clone(),
        };
        let platform_client =
            PlatformManifestClient::new(config.backend_api_url(), &config.api_key)
                .map(Arc::new)
                .map_err(|error| {
                    warn!(error = %error, "Failed to initialize platform API client");
                    error
                })
                .ok();
        Self {
            manifest_backend: platform_client.as_ref().map(|client| {
                Arc::new(PlatformManifestBackend::new(
                    local_store.clone(),
                    client.as_ref().clone(),
                    payload_encoder.clone(),
                ))
            }),
            security,
            runtime,
            config,
            external_mcp,
            manifest_store: local_store,
            platform_client,
            payload_encoder,
        }
    }

    /// Build the base tool set (always included).
    pub fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.base_tools_with(&self.security)
    }

    /// Build the base tool set with a given security policy.
    fn base_tools_with(&self, security: &Arc<SecurityPolicy>) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ShellTool::new(security.clone(), self.runtime.clone())),
            Arc::new(FileReadTool::new(security.clone())),
            Arc::new(FileWriteTool::new(security.clone())),
            Arc::new(FileEditTool::new(security.clone())),
            Arc::new(GitOperationsTool::new(security.clone())),
            Arc::new(ContentSearchTool::new(security.clone())),
            Arc::new(GlobSearchTool::new(security.clone())),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::ListDocs,
            )),
            Arc::new(BuiltinKnowledgeTool::new(BuiltinKnowledgeToolKind::ReadDoc)),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::SearchDocs,
            )),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::SearchDocPaths,
            )),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::ListTree,
            )),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::ReadManifest,
            )),
            Arc::new(BuiltinKnowledgeTool::new(
                BuiltinKnowledgeToolKind::Neighbors,
            )),
        ]
    }

    /// Build all tools for an agent with a given security policy.
    async fn build_tools(
        &self,
        agent: &AgentManifest,
        security: &Arc<SecurityPolicy>,
    ) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base_tools_with(security);

        // Add MCP tools scoped to this agent's server assignments and platform scopes.
        if !agent.mcp_server_ids.is_empty() {
            let mcp_tools = self
                .external_mcp
                .tools_for_agent(
                    &agent.mcp_server_ids,
                    if agent.platform_scopes.is_empty() {
                        None
                    } else {
                        Some(&agent.platform_scopes)
                    },
                )
                .await;
            // Convert Box<dyn Tool> → Arc<dyn Tool>
            for t in mcp_tools {
                tools.push(Arc::from(t));
            }
        }

        let policy = ManifestAccessPolicy::new(agent.platform_scopes.clone());

        if let Some(backend) = self.manifest_backend.as_ref() {
            let backend: Arc<dyn ManifestMcpBackend> =
                Arc::new(backend.as_ref().clone().with_access_policy(policy.clone()));
            add_manifest_tools(&mut tools, backend, &policy);
        }

        if let Some(client) = self.platform_client.as_ref() {
            let project_backend = PlatformProjectToolsBackend {
                client: client.clone(),
                manifest_store: self.manifest_store.clone(),
                payload_encoder: self.payload_encoder.clone(),
            };
            add_project_knowledge_tools(&mut tools, project_backend.clone(), &policy);
            add_project_rest_tools(&mut tools, project_backend, &policy);
        }

        // Web fetch (always included with config, deny-by-default via allowed_domains)
        if self.config.web_fetch.enabled {
            tools.push(Arc::new(WebFetchTool::new(
                security.clone(),
                self.config.web_fetch.allowed_domains.clone(),
                self.config.web_fetch.blocked_domains.clone(),
                self.config.web_fetch.max_response_size,
                self.config.web_fetch.timeout_secs,
            )));
        }

        // Web search
        if self.config.web_search.enabled {
            tools.push(Arc::new(WebSearchTool::new(
                self.config.web_search.provider.clone(),
                self.config.web_search.brave_api_key.clone(),
                self.config.web_search.max_results,
                self.config.web_search.timeout_secs,
            )));
        }

        // HTTP request
        if self.config.http_request.enabled {
            tools.push(Arc::new(HttpRequestTool::new(
                security.clone(),
                self.config.http_request.allowed_domains.clone(),
                self.config.http_request.max_response_size,
                self.config.http_request.timeout_secs,
            )));
        }

        // Browser
        if self.config.browser.enabled {
            tools.push(Arc::new(BrowserOpenTool::new(
                security.clone(),
                self.config.browser.allowed_domains.clone(),
            )));
            tools.push(Arc::new(ScreenshotTool::new(security.clone())));
        }

        tools
    }
}

#[derive(Debug, Clone, Copy)]
enum BuiltinKnowledgeToolKind {
    ListDocs,
    ReadDoc,
    SearchDocs,
    SearchDocPaths,
    ListTree,
    ReadManifest,
    Neighbors,
}

struct BuiltinKnowledgeTool {
    kind: BuiltinKnowledgeToolKind,
}

#[derive(Debug, Default, Deserialize)]
struct BuiltinFilterArgs {
    #[serde(default)]
    tags: Vec<String>,
    kind: Option<BuiltinDocKind>,
    authority: Option<BuiltinDocAuthority>,
    status: Option<BuiltinDocStatus>,
    path_prefix: Option<String>,
    related_to: Option<String>,
    edge_type: Option<BuiltinDocEdgeType>,
}

#[derive(Debug, Deserialize)]
struct BuiltinLookupArgs {
    #[serde(alias = "id", alias = "path")]
    id_or_path: String,
}

#[derive(Debug, Deserialize)]
struct BuiltinSearchArgs {
    query: String,
    #[serde(flatten)]
    filter: BuiltinFilterArgs,
}

#[derive(Debug, Deserialize)]
struct BuiltinTreeArgs {
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BuiltinNeighborArgs {
    #[serde(alias = "id", alias = "path")]
    id_or_path: String,
    edge_type: Option<BuiltinDocEdgeType>,
}

impl BuiltinKnowledgeTool {
    fn new(kind: BuiltinKnowledgeToolKind) -> Self {
        Self { kind }
    }
}

#[async_trait]
impl Tool for BuiltinKnowledgeTool {
    fn name(&self) -> &str {
        match self.kind {
            BuiltinKnowledgeToolKind::ListDocs => "list_builtin_docs",
            BuiltinKnowledgeToolKind::ReadDoc => "read_builtin_doc",
            BuiltinKnowledgeToolKind::SearchDocs => "search_builtin_docs",
            BuiltinKnowledgeToolKind::SearchDocPaths => "search_builtin_doc_paths",
            BuiltinKnowledgeToolKind::ListTree => "list_builtin_doc_tree",
            BuiltinKnowledgeToolKind::ReadManifest => "read_builtin_doc_manifest",
            BuiltinKnowledgeToolKind::Neighbors => "list_builtin_doc_neighbors",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            BuiltinKnowledgeToolKind::ListDocs => {
                "List compact metadata for embedded Nenjo builtin docs under builtin://nenjo/."
            }
            BuiltinKnowledgeToolKind::ReadDoc => {
                "Read a full embedded Nenjo builtin doc by id or builtin://nenjo/ path."
            }
            BuiltinKnowledgeToolKind::SearchDocs => {
                "Search embedded Nenjo builtin docs and return matching metadata plus full markdown content."
            }
            BuiltinKnowledgeToolKind::SearchDocPaths => {
                "Search embedded Nenjo builtin docs and return compact path metadata without full bodies."
            }
            BuiltinKnowledgeToolKind::ListTree => {
                "List the embedded Nenjo builtin virtual filesystem tree under builtin://nenjo/."
            }
            BuiltinKnowledgeToolKind::ReadManifest => {
                "Read compact manifest metadata for one embedded Nenjo builtin doc by id or path."
            }
            BuiltinKnowledgeToolKind::Neighbors => {
                "List graph neighbors for one embedded Nenjo builtin doc by id or path."
            }
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        match self.kind {
            BuiltinKnowledgeToolKind::ReadDoc | BuiltinKnowledgeToolKind::ReadManifest => json!({
                "type": "object",
                "properties": {
                    "id_or_path": {
                        "type": "string",
                        "description": "Builtin doc id such as nenjo.guide.routines or path such as builtin://nenjo/guide/routines.md"
                    }
                },
                "required": ["id_or_path"]
            }),
            BuiltinKnowledgeToolKind::SearchDocs | BuiltinKnowledgeToolKind::SearchDocPaths => {
                filter_schema(
                    Some(json!({
                        "query": {
                            "type": "string",
                            "description": "Search query, alias, keyword, tag, title, or body text"
                        }
                    })),
                    &["query"],
                )
            }
            BuiltinKnowledgeToolKind::ListTree => json!({
                "type": "object",
                "properties": {
                    "prefix": {
                        "type": "string",
                        "description": "Optional builtin://nenjo/ path prefix"
                    }
                }
            }),
            BuiltinKnowledgeToolKind::ListDocs => filter_schema(None, &[]),
            BuiltinKnowledgeToolKind::Neighbors => json!({
                "type": "object",
                "properties": {
                    "id_or_path": {
                        "type": "string",
                        "description": "Builtin doc id or builtin://nenjo/ path"
                    },
                    "edge_type": {
                        "type": "string",
                        "enum": ["part_of", "defines", "governs", "classifies", "references", "depends_on", "extends", "related_to"],
                        "description": "Optional canonical relationship type"
                    }
                },
                "required": ["id_or_path"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let pack = builtin_knowledge_pack();
        let output = match self.kind {
            BuiltinKnowledgeToolKind::ListDocs => {
                let args: BuiltinFilterArgs =
                    serde_json::from_value(args).context("invalid list_builtin_docs args")?;
                serde_json::to_value(pack.list_docs(args.into_filter()))?
            }
            BuiltinKnowledgeToolKind::ReadDoc => {
                let args: BuiltinLookupArgs =
                    serde_json::from_value(args).context("invalid read_builtin_doc args")?;
                serde_json::to_value(pack.read_doc(&args.id_or_path).ok_or_else(|| {
                    anyhow!(
                        "unknown builtin doc '{}'; use an id or builtin://nenjo/ path",
                        args.id_or_path
                    )
                })?)?
            }
            BuiltinKnowledgeToolKind::SearchDocs => {
                let args: BuiltinSearchArgs =
                    serde_json::from_value(args).context("invalid search_builtin_docs args")?;
                serde_json::to_value(pack.search_docs(&args.query, args.filter.into_filter()))?
            }
            BuiltinKnowledgeToolKind::SearchDocPaths => {
                let args: BuiltinSearchArgs = serde_json::from_value(args)
                    .context("invalid search_builtin_doc_paths args")?;
                serde_json::to_value(pack.search_paths(&args.query, args.filter.into_filter()))?
            }
            BuiltinKnowledgeToolKind::ListTree => {
                let args: BuiltinTreeArgs =
                    serde_json::from_value(args).context("invalid list_builtin_doc_tree args")?;
                serde_json::to_value(pack.list_tree(args.prefix.as_deref()))?
            }
            BuiltinKnowledgeToolKind::ReadManifest => {
                let args: BuiltinLookupArgs = serde_json::from_value(args)
                    .context("invalid read_builtin_doc_manifest args")?;
                serde_json::to_value(pack.read_manifest(&args.id_or_path).ok_or_else(|| {
                    anyhow!(
                        "unknown builtin doc '{}'; use an id or builtin://nenjo/ path",
                        args.id_or_path
                    )
                })?)?
            }
            BuiltinKnowledgeToolKind::Neighbors => {
                let args: BuiltinNeighborArgs = serde_json::from_value(args)
                    .context("invalid list_builtin_doc_neighbors args")?;
                serde_json::to_value(pack.neighbors(&args.id_or_path, args.edge_type))?
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }
}

impl BuiltinFilterArgs {
    fn into_filter(self) -> BuiltinDocFilter {
        BuiltinDocFilter {
            tags: self.tags,
            kind: self.kind,
            authority: self.authority,
            status: self.status,
            path_prefix: self.path_prefix,
            related_to: self.related_to,
            edge_type: self.edge_type,
        }
    }
}

fn filter_schema(
    extra_properties: Option<serde_json::Value>,
    required: &[&str],
) -> serde_json::Value {
    let mut properties = json!({
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional tags that all returned docs must have"
        },
        "kind": {
            "type": "string",
            "enum": ["guide", "reference", "taxonomy", "domain", "entity", "policy"]
        },
        "authority": {
            "type": "string",
            "enum": ["canonical", "pattern", "reference", "advisory", "example"]
        },
        "status": {
            "type": "string",
            "enum": ["stable", "draft", "deprecated"]
        },
        "path_prefix": {
            "type": "string",
            "description": "Optional builtin://nenjo/ path prefix"
        },
        "related_to": {
            "type": "string",
            "description": "Optional target doc id that returned docs must relate to"
        },
        "edge_type": {
            "type": "string",
            "enum": ["part_of", "defines", "governs", "classifies", "references", "depends_on", "extends", "related_to"],
            "description": "Optional canonical relationship type used with related_to"
        }
    });

    if let Some(extra) = extra_properties
        && let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (key, value) in extra_map {
            map.insert(key.clone(), value.clone());
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

const AGENT_READ_TOOLS: &[&str] = &["list_agents", "get_agent", "get_agent_prompt"];
const AGENT_WRITE_TOOLS: &[&str] = &[
    "create_agent",
    "update_agent",
    "update_agent_prompt",
    "delete_agent",
];
const ABILITY_READ_TOOLS: &[&str] = &["list_abilities", "get_ability", "get_ability_prompt"];
const ABILITY_WRITE_TOOLS: &[&str] = &[
    "create_ability",
    "update_ability",
    "update_ability_prompt",
    "delete_ability",
];
const DOMAIN_READ_TOOLS: &[&str] = &["list_domains", "get_domain", "get_domain_prompt"];
const DOMAIN_WRITE_TOOLS: &[&str] = &[
    "create_domain",
    "update_domain",
    "update_domain_prompt",
    "delete_domain",
];
const PROJECT_READ_TOOLS: &[&str] = &[
    "list_projects",
    "get_project",
    "list_project_documents",
    "get_project_document",
    "get_project_document_content",
];
const PROJECT_WRITE_TOOLS: &[&str] = &[
    "create_project",
    "update_project",
    "delete_project",
    "create_project_document",
    "update_project_document_content",
    "delete_project_document",
];
const PROJECT_KNOWLEDGE_READ_TOOLS: &[&str] = &[
    "list_project_docs",
    "read_project_doc",
    "search_project_docs",
    "search_project_doc_paths",
    "list_project_doc_tree",
    "read_project_doc_manifest",
    "list_project_doc_neighbors",
];
const PROJECT_NATIVE_READ_TOOLS: &[&str] = &[
    "list_project_tasks",
    "get_project_task",
    "list_project_execution_runs",
    "get_project_execution_run",
];
const PROJECT_NATIVE_WRITE_TOOLS: &[&str] = &[
    "create_project_task",
    "bulk_create_project_tasks",
    "update_project_task",
    "delete_project_task",
    "start_project_execution",
    "pause_project_execution",
    "resume_project_execution",
];
const ROUTINE_READ_TOOLS: &[&str] = &["list_routines", "get_routine"];
const ROUTINE_WRITE_TOOLS: &[&str] = &["create_routine", "update_routine", "delete_routine"];
const MODEL_READ_TOOLS: &[&str] = &["list_models", "get_model"];
const MODEL_WRITE_TOOLS: &[&str] = &["create_model", "update_model", "delete_model"];
const COUNCIL_READ_TOOLS: &[&str] = &["list_councils", "get_council"];
const COUNCIL_WRITE_TOOLS: &[&str] = &[
    "create_council",
    "update_council",
    "add_council_member",
    "remove_council_member",
    "delete_council",
];
const CONTEXT_BLOCK_READ_TOOLS: &[&str] = &[
    "list_context_blocks",
    "get_context_block",
    "get_context_block_content",
];
const CONTEXT_BLOCK_WRITE_TOOLS: &[&str] = &[
    "create_context_block",
    "update_context_block",
    "update_context_block_content",
    "delete_context_block",
];

const MANIFEST_TOOL_GROUPS: &[(ScopeResource, &[&str], &[&str])] = &[
    (ScopeResource::Agents, AGENT_READ_TOOLS, AGENT_WRITE_TOOLS),
    (
        ScopeResource::Abilities,
        ABILITY_READ_TOOLS,
        ABILITY_WRITE_TOOLS,
    ),
    (
        ScopeResource::Domains,
        DOMAIN_READ_TOOLS,
        DOMAIN_WRITE_TOOLS,
    ),
    (
        ScopeResource::Projects,
        PROJECT_READ_TOOLS,
        PROJECT_WRITE_TOOLS,
    ),
    (
        ScopeResource::Routines,
        ROUTINE_READ_TOOLS,
        ROUTINE_WRITE_TOOLS,
    ),
    (ScopeResource::Models, MODEL_READ_TOOLS, MODEL_WRITE_TOOLS),
    (
        ScopeResource::Councils,
        COUNCIL_READ_TOOLS,
        COUNCIL_WRITE_TOOLS,
    ),
    (
        ScopeResource::ContextBlocks,
        CONTEXT_BLOCK_READ_TOOLS,
        CONTEXT_BLOCK_WRITE_TOOLS,
    ),
];

fn add_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    policy: &ManifestAccessPolicy,
) {
    let specs = manifest_tool_specs();
    for (resource, read_tools, write_tools) in MANIFEST_TOOL_GROUPS {
        if policy.can_read_resource(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, read_tools);
        }
        if policy.can_write_resource(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, write_tools);
        }
    }
}

fn add_project_rest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformProjectToolsBackend,
    policy: &ManifestAccessPolicy,
) {
    if policy.can_read_resource(ScopeResource::Projects) {
        add_named_project_rest_tools(tools, backend.clone(), PROJECT_NATIVE_READ_TOOLS);
    }
    if policy.can_write_resource(ScopeResource::Projects) {
        add_named_project_rest_tools(tools, backend, PROJECT_NATIVE_WRITE_TOOLS);
    }
}

fn add_project_knowledge_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformProjectToolsBackend,
    policy: &ManifestAccessPolicy,
) {
    if policy.can_read_resource(ScopeResource::Projects) {
        for tool_name in PROJECT_KNOWLEDGE_READ_TOOLS {
            if tools.iter().any(|existing| existing.name() == *tool_name) {
                continue;
            }
            if let Some(tool) = ProjectKnowledgeTool::from_name(tool_name, backend.clone()) {
                tools.push(Arc::new(tool));
            }
        }
    }
}

fn add_named_project_rest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformProjectToolsBackend,
    tool_names: &[&str],
) {
    for tool_name in tool_names {
        if tools.iter().any(|existing| existing.name() == *tool_name) {
            continue;
        }
        if let Some(tool) = ProjectRestTool::from_name(tool_name, backend.clone()) {
            tools.push(Arc::new(tool));
        }
    }
}

fn add_named_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    specs: &HashMap<String, nenjo::ToolSpec>,
    tool_names: &[&str],
) {
    for tool_name in tool_names {
        let Some(spec) = specs.get(*tool_name) else {
            continue;
        };
        if tools.iter().any(|existing| existing.name() == spec.name) {
            continue;
        }
        tools.push(Arc::new(ManifestContractTool::new(
            spec.clone(),
            backend.clone(),
        )));
    }
}

fn manifest_tool_specs() -> HashMap<String, nenjo::ToolSpec> {
    ManifestMcpContract::tools()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect()
}

struct ManifestContractTool {
    spec: nenjo::ToolSpec,
    backend: Arc<dyn ManifestMcpBackend>,
}

#[derive(Clone)]
struct PlatformProjectToolsBackend {
    client: Arc<PlatformManifestClient>,
    manifest_store: Arc<LocalManifestStore>,
    payload_encoder: WorkerAgentPromptPayloadEncoder,
}

#[derive(Debug, Clone, Copy)]
enum ProjectKnowledgeToolKind {
    ListDocs,
    ReadDoc,
    SearchDocs,
    SearchDocPaths,
    ListTree,
    ReadManifest,
    Neighbors,
}

struct ProjectKnowledgeTool {
    kind: ProjectKnowledgeToolKind,
    backend: PlatformProjectToolsBackend,
}

#[derive(Debug, Default, Deserialize)]
struct ProjectDocFilterArgs {
    #[serde(default)]
    tags: Vec<String>,
    kind: Option<String>,
    authority: Option<String>,
    status: Option<String>,
    path_prefix: Option<String>,
    related_to: Option<String>,
    edge_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectDocListArgs {
    project_id: Uuid,
    #[serde(flatten)]
    filter: ProjectDocFilterArgs,
}

#[derive(Debug, Deserialize)]
struct ProjectDocLookupArgs {
    project_id: Uuid,
    #[serde(alias = "id", alias = "path")]
    id_or_path: String,
}

#[derive(Debug, Deserialize)]
struct ProjectDocSearchArgs {
    project_id: Uuid,
    query: String,
    #[serde(flatten)]
    filter: ProjectDocFilterArgs,
}

#[derive(Debug, Deserialize)]
struct ProjectDocTreeArgs {
    project_id: Uuid,
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectDocNeighborArgs {
    project_id: Uuid,
    #[serde(alias = "id", alias = "path")]
    id_or_path: String,
    edge_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocManifest {
    id: String,
    project_id: String,
    virtual_path: String,
    filename: String,
    path: Option<String>,
    title: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    kind: String,
    authority: String,
    status: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocRead {
    manifest: ProjectDocManifest,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocSearchHit {
    id: String,
    virtual_path: String,
    title: String,
    summary: String,
    kind: String,
    authority: String,
    tags: Vec<String>,
    score: usize,
    matched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocTree {
    root_uri: String,
    entries: Vec<ProjectDocTreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocTreeEntry {
    path: String,
    title: String,
    kind: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocNeighbor {
    edge_type: String,
    direction: String,
    target: String,
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskContentPayload {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CurrentTaskState {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

impl PlatformProjectToolsBackend {
    async fn account_id(&self) -> Result<Uuid> {
        let manifest = self.manifest_store.load_manifest().await?;
        manifest
            .auth
            .map(|auth| auth.user_id)
            .filter(|user_id| !user_id.is_nil())
            .ok_or_else(|| anyhow!("worker manifest is missing auth.user_id"))
    }

    async fn encode_task_payload(
        &self,
        task_id: Uuid,
        payload: &TaskContentPayload,
    ) -> Result<serde_json::Value> {
        let account_id = self.account_id().await?;
        self.payload_encoder
            .encode_payload(
                account_id,
                task_id,
                "task_content",
                &serde_json::to_value(payload).context("failed to encode task content payload")?,
            )
            .await?
            .ok_or_else(|| anyhow!("task encryption did not return a payload"))
    }

    async fn maybe_encode_task_payload(
        &self,
        task_id: Uuid,
        payload: &TaskContentPayload,
    ) -> Result<Option<serde_json::Value>> {
        if payload.description.is_none() && payload.acceptance_criteria.is_none() {
            return Ok(None);
        }
        self.encode_task_payload(task_id, payload).await.map(Some)
    }

    async fn create_task_body(&self, args: &CreateProjectTaskArgs) -> Result<serde_json::Value> {
        let task_id = Uuid::new_v4();
        let payload = TaskContentPayload {
            description: args.description.clone(),
            acceptance_criteria: args.acceptance_criteria.clone(),
        };
        let encrypted_payload = self.maybe_encode_task_payload(task_id, &payload).await?;

        Ok(json!({
            "id": task_id,
            "project_id": args.project_id,
            "title": args.title,
            "status": args.status,
            "priority": args.priority,
            "type": args.task_type,
            "metadata": args.metadata,
            "tags": args.tags,
            "required_tags": args.required_tags,
            "slug": args.slug,
            "complexity": args.complexity,
            "order_index": args.order_index,
            "assigned_to": args.assigned_to,
            "assigned_agent_id": args.assigned_agent_id,
            "routine_id": args.routine_id,
            "encrypted_payload": encrypted_payload,
        }))
    }

    async fn bulk_create_task_body(
        &self,
        args: &BulkCreateProjectTasksArgs,
    ) -> Result<serde_json::Value> {
        let mut tasks = Vec::with_capacity(args.tasks.len());
        for task in &args.tasks {
            let body = self
                .create_task_body(&CreateProjectTaskArgs {
                    project_id: args.project_id,
                    title: task.title.clone(),
                    description: task.description.clone(),
                    acceptance_criteria: task.acceptance_criteria.clone(),
                    status: task.status.clone(),
                    priority: task.priority.clone(),
                    task_type: task.task_type.clone(),
                    complexity: task.complexity,
                    tags: task.tags.clone(),
                    required_tags: task.required_tags.clone(),
                    slug: task.slug.clone(),
                    order_index: task.order_index,
                    assigned_to: task.assigned_to,
                    assigned_agent_id: task.assigned_agent_id,
                    routine_id: task.routine_id,
                    metadata: task.metadata.clone(),
                })
                .await?;
            tasks.push(body);
        }
        Ok(json!({ "tasks": tasks }))
    }

    async fn update_task_body(&self, args: &UpdateProjectTaskArgs) -> Result<serde_json::Value> {
        let mut body = serde_json::Map::new();

        if let Some(status) = args.status.as_ref() {
            body.insert("status".into(), json!(status));
        }
        if let Some(priority) = args.priority.as_ref() {
            body.insert("priority".into(), json!(priority));
        }
        if let Some(task_type) = args.task_type.as_ref() {
            body.insert("type".into(), json!(task_type));
        }
        if let Some(metadata) = args.metadata.as_ref() {
            body.insert("metadata".into(), metadata.clone());
        }
        if let Some(tags) = args.tags.as_ref() {
            body.insert("tags".into(), json!(tags));
        }
        if let Some(required_tags) = args.required_tags.as_ref() {
            body.insert("required_tags".into(), json!(required_tags));
        }
        if let Some(slug) = args.slug.as_ref() {
            body.insert("slug".into(), json!(slug));
        }
        if let Some(complexity) = args.complexity {
            body.insert("complexity".into(), json!(complexity));
        }
        if let Some(order_index) = args.order_index {
            body.insert("order_index".into(), json!(order_index));
        }
        if let Some(assigned_to) = args.assigned_to {
            body.insert("assigned_to".into(), json!(assigned_to));
        }
        if let Some(assigned_agent_id) = args.assigned_agent_id {
            body.insert("assigned_agent_id".into(), json!(assigned_agent_id));
        }
        if let Some(routine_id) = args.routine_id {
            body.insert("routine_id".into(), json!(routine_id));
        }

        if let Some(title) = args.title.as_ref() {
            body.insert("title".into(), json!(title));
        }

        let needs_encrypted_payload =
            args.description.is_some() || args.acceptance_criteria.is_some();

        if needs_encrypted_payload {
            let current = self.client.get_project_task(args.task_id).await?;
            let current: CurrentTaskState =
                serde_json::from_value(current).context("failed to decode current task state")?;
            let payload = TaskContentPayload {
                description: args.description.clone().or(current.description),
                acceptance_criteria: args
                    .acceptance_criteria
                    .clone()
                    .or(current.acceptance_criteria),
            };
            body.insert(
                "encrypted_payload".into(),
                self.encode_task_payload(args.task_id, &payload).await?,
            );
        }

        Ok(serde_json::Value::Object(body))
    }
}

impl ProjectKnowledgeTool {
    fn from_name(name: &str, backend: PlatformProjectToolsBackend) -> Option<Self> {
        let kind = match name {
            "list_project_docs" => ProjectKnowledgeToolKind::ListDocs,
            "read_project_doc" => ProjectKnowledgeToolKind::ReadDoc,
            "search_project_docs" => ProjectKnowledgeToolKind::SearchDocs,
            "search_project_doc_paths" => ProjectKnowledgeToolKind::SearchDocPaths,
            "list_project_doc_tree" => ProjectKnowledgeToolKind::ListTree,
            "read_project_doc_manifest" => ProjectKnowledgeToolKind::ReadManifest,
            "list_project_doc_neighbors" => ProjectKnowledgeToolKind::Neighbors,
            _ => return None,
        };
        Some(Self { kind, backend })
    }
}

#[async_trait]
impl Tool for ProjectKnowledgeTool {
    fn name(&self) -> &str {
        match self.kind {
            ProjectKnowledgeToolKind::ListDocs => "list_project_docs",
            ProjectKnowledgeToolKind::ReadDoc => "read_project_doc",
            ProjectKnowledgeToolKind::SearchDocs => "search_project_docs",
            ProjectKnowledgeToolKind::SearchDocPaths => "search_project_doc_paths",
            ProjectKnowledgeToolKind::ListTree => "list_project_doc_tree",
            ProjectKnowledgeToolKind::ReadManifest => "read_project_doc_manifest",
            ProjectKnowledgeToolKind::Neighbors => "list_project_doc_neighbors",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            ProjectKnowledgeToolKind::ListDocs => {
                "List compact metadata for one project's documents using builtin-style filters."
            }
            ProjectKnowledgeToolKind::ReadDoc => {
                "Read a full project document by id, relative path, or project://<project_id>/ path."
            }
            ProjectKnowledgeToolKind::SearchDocs => {
                "Search one project's documents and return matching metadata plus full text content."
            }
            ProjectKnowledgeToolKind::SearchDocPaths => {
                "Search one project's documents and return compact path metadata without full bodies."
            }
            ProjectKnowledgeToolKind::ListTree => {
                "List the virtual filesystem tree for one project's documents."
            }
            ProjectKnowledgeToolKind::ReadManifest => {
                "Read compact manifest metadata for one project document by id or path."
            }
            ProjectKnowledgeToolKind::Neighbors => {
                "List graph neighbors for one project document by id or path."
            }
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        match self.kind {
            ProjectKnowledgeToolKind::ListDocs => project_doc_filter_schema(None, &["project_id"]),
            ProjectKnowledgeToolKind::ReadDoc | ProjectKnowledgeToolKind::ReadManifest => json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target project id"
                    },
                    "id_or_path": {
                        "type": "string",
                        "description": "Project doc id, relative path, filename, or project://<project_id>/... path"
                    }
                },
                "required": ["project_id", "id_or_path"],
                "additionalProperties": false
            }),
            ProjectKnowledgeToolKind::SearchDocs | ProjectKnowledgeToolKind::SearchDocPaths => {
                project_doc_filter_schema(
                    Some(json!({
                        "query": {
                            "type": "string",
                            "description": "Search query, path, title, tag, summary, or body text"
                        }
                    })),
                    &["project_id", "query"],
                )
            }
            ProjectKnowledgeToolKind::ListTree => json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target project id"
                    },
                    "prefix": {
                        "type": "string",
                        "description": "Optional relative path or project://<project_id>/ prefix"
                    }
                },
                "required": ["project_id"],
                "additionalProperties": false
            }),
            ProjectKnowledgeToolKind::Neighbors => json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target project id"
                    },
                    "id_or_path": {
                        "type": "string",
                        "description": "Project doc id, relative path, filename, or project://<project_id>/... path"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Optional relationship type filter such as references or depends_on"
                    }
                },
                "required": ["project_id", "id_or_path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let output = match self.kind {
            ProjectKnowledgeToolKind::ListDocs => {
                let args: ProjectDocListArgs =
                    serde_json::from_value(args).context("invalid list_project_docs args")?;
                let docs = self
                    .backend
                    .client
                    .list_project_document_metadata(args.project_id)
                    .await?;
                let docs = filter_project_docs(docs, &args.filter);
                serde_json::to_value(
                    docs.iter()
                        .map(project_doc_manifest)
                        .collect::<Vec<ProjectDocManifest>>(),
                )?
            }
            ProjectKnowledgeToolKind::ReadDoc => {
                let args: ProjectDocLookupArgs =
                    serde_json::from_value(args).context("invalid read_project_doc args")?;
                let manifest =
                    lookup_project_doc(&self.backend, args.project_id, &args.id_or_path).await?;
                let content = self
                    .backend
                    .client
                    .get_project_document_content(args.project_id, manifest.id)
                    .await?
                    .description;
                serde_json::to_value(ProjectDocRead {
                    manifest: project_doc_manifest(&manifest),
                    content,
                })?
            }
            ProjectKnowledgeToolKind::SearchDocs => {
                let args: ProjectDocSearchArgs =
                    serde_json::from_value(args).context("invalid search_project_docs args")?;
                let docs = self
                    .backend
                    .client
                    .list_project_document_metadata(args.project_id)
                    .await?;
                let hits = search_project_docs(
                    &self.backend,
                    args.project_id,
                    docs,
                    &args.query,
                    &args.filter,
                    true,
                )
                .await?;
                serde_json::to_value(hits)?
            }
            ProjectKnowledgeToolKind::SearchDocPaths => {
                let args: ProjectDocSearchArgs = serde_json::from_value(args)
                    .context("invalid search_project_doc_paths args")?;
                let docs = self
                    .backend
                    .client
                    .list_project_document_metadata(args.project_id)
                    .await?;
                let hits = search_project_docs(
                    &self.backend,
                    args.project_id,
                    docs,
                    &args.query,
                    &args.filter,
                    false,
                )
                .await?;
                serde_json::to_value(hits)?
            }
            ProjectKnowledgeToolKind::ListTree => {
                let args: ProjectDocTreeArgs =
                    serde_json::from_value(args).context("invalid list_project_doc_tree args")?;
                let docs = self
                    .backend
                    .client
                    .list_project_document_metadata(args.project_id)
                    .await?;
                serde_json::to_value(project_doc_tree(
                    args.project_id,
                    &docs,
                    args.prefix.as_deref(),
                ))?
            }
            ProjectKnowledgeToolKind::ReadManifest => {
                let args: ProjectDocLookupArgs = serde_json::from_value(args)
                    .context("invalid read_project_doc_manifest args")?;
                let manifest =
                    lookup_project_doc(&self.backend, args.project_id, &args.id_or_path).await?;
                serde_json::to_value(project_doc_manifest(&manifest))?
            }
            ProjectKnowledgeToolKind::Neighbors => {
                let args: ProjectDocNeighborArgs = serde_json::from_value(args)
                    .context("invalid list_project_doc_neighbors args")?;
                let manifest =
                    lookup_project_doc(&self.backend, args.project_id, &args.id_or_path).await?;
                let neighbors = project_doc_neighbors(
                    &self.backend,
                    args.project_id,
                    &manifest,
                    args.edge_type.as_deref(),
                )
                .await?;
                serde_json::to_value(neighbors)?
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }
}

fn project_doc_filter_schema(
    extra_properties: Option<serde_json::Value>,
    required: &[&str],
) -> serde_json::Value {
    let mut properties = json!({
        "project_id": {
            "type": "string",
            "format": "uuid",
            "description": "Target project id"
        },
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional tags that all returned docs must have"
        },
        "kind": {
            "type": "string",
            "description": "Optional kind filter"
        },
        "authority": {
            "type": "string",
            "description": "Optional authority filter"
        },
        "status": {
            "type": "string",
            "description": "Optional status filter"
        },
        "path_prefix": {
            "type": "string",
            "description": "Optional relative path or project://<project_id>/ prefix"
        },
        "related_to": {
            "type": "string",
            "description": "Optional related doc id or path that returned docs must connect to"
        },
        "edge_type": {
            "type": "string",
            "description": "Optional relationship type used with related_to"
        }
    });

    if let Some(extra) = extra_properties
        && let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (key, value) in extra_map {
            map.insert(key.clone(), value.clone());
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn project_doc_virtual_path(project_id: Uuid, doc: &ProjectDocumentMetadata) -> String {
    let mut path = doc.path.clone().unwrap_or_default();
    path = path.trim_matches('/').to_string();
    if path.is_empty() {
        format!("project://{project_id}/{}", doc.filename)
    } else {
        format!("project://{project_id}/{path}/{}", doc.filename)
    }
}

fn project_doc_relative_path(doc: &ProjectDocumentMetadata) -> String {
    let mut path = doc.path.clone().unwrap_or_default();
    path = path.trim_matches('/').to_string();
    if path.is_empty() {
        doc.filename.clone()
    } else {
        format!("{path}/{}", doc.filename)
    }
}

fn normalize_project_lookup(project_id: Uuid, value: &str) -> String {
    let trimmed = value.trim().trim_matches('/').to_string();
    if let Some(stripped) = trimmed.strip_prefix(&format!("project://{project_id}/")) {
        stripped.trim_matches('/').to_string()
    } else {
        trimmed
    }
}

fn project_doc_manifest(doc: &ProjectDocumentMetadata) -> ProjectDocManifest {
    ProjectDocManifest {
        id: doc.id.to_string(),
        project_id: doc.project_id.to_string(),
        virtual_path: project_doc_virtual_path(doc.project_id, doc),
        filename: doc.filename.clone(),
        path: doc.path.clone(),
        title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
        summary: doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Project document {}", project_doc_relative_path(doc))),
        description: None,
        kind: doc.kind.clone().unwrap_or_else(|| "reference".to_string()),
        authority: doc.authority.clone(),
        status: doc.status.clone().unwrap_or_else(|| "stable".to_string()),
        tags: doc.tags.clone(),
    }
}

fn filter_project_docs(
    docs: Vec<ProjectDocumentMetadata>,
    filter: &ProjectDocFilterArgs,
) -> Vec<ProjectDocumentMetadata> {
    docs.into_iter()
        .filter(|doc| {
            if !filter.tags.is_empty()
                && !filter.tags.iter().all(|tag| {
                    doc.tags
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(tag))
                })
            {
                return false;
            }
            if let Some(kind) = filter.kind.as_ref()
                && doc
                    .kind
                    .as_deref()
                    .map(|value| !value.eq_ignore_ascii_case(kind))
                    .unwrap_or(true)
            {
                return false;
            }
            if let Some(authority) = filter.authority.as_ref()
                && !doc.authority.eq_ignore_ascii_case(authority)
            {
                return false;
            }
            if let Some(status) = filter.status.as_ref()
                && doc
                    .status
                    .as_deref()
                    .map(|value| !value.eq_ignore_ascii_case(status))
                    .unwrap_or(true)
            {
                return false;
            }
            if let Some(path_prefix) = filter.path_prefix.as_ref() {
                let prefix = path_prefix.trim_matches('/');
                let relative = project_doc_relative_path(doc);
                let virtual_path = project_doc_virtual_path(doc.project_id, doc);
                if !relative.starts_with(prefix) && !virtual_path.starts_with(path_prefix) {
                    return false;
                }
            }
            true
        })
        .collect()
}

async fn lookup_project_doc(
    backend: &PlatformProjectToolsBackend,
    project_id: Uuid,
    id_or_path: &str,
) -> Result<ProjectDocumentMetadata> {
    let docs = backend
        .client
        .list_project_document_metadata(project_id)
        .await?;
    let normalized = normalize_project_lookup(project_id, id_or_path);
    docs.into_iter()
        .find(|doc| {
            doc.id.to_string() == id_or_path
                || project_doc_virtual_path(project_id, doc) == id_or_path
                || project_doc_relative_path(doc) == normalized
                || doc.filename == normalized
        })
        .ok_or_else(|| {
            anyhow!(
                "unknown project doc '{}'; use a document id, relative path, filename, or project://{project_id}/ path",
                id_or_path
            )
        })
}

fn project_doc_score(
    doc: &ProjectDocumentMetadata,
    query: &str,
    content: Option<&str>,
) -> Option<(usize, Vec<String>)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }

    let mut score = 0;
    let mut matched = Vec::new();
    let relative_path = project_doc_relative_path(doc).to_lowercase();
    let virtual_path = project_doc_virtual_path(doc.project_id, doc).to_lowercase();
    let title = doc
        .title
        .clone()
        .unwrap_or_else(|| doc.filename.clone())
        .to_lowercase();
    let summary = doc.summary.clone().unwrap_or_default().to_lowercase();
    let kind = doc.kind.clone().unwrap_or_default().to_lowercase();
    let authority = doc.authority.to_lowercase();
    let status = doc.status.clone().unwrap_or_default().to_lowercase();
    let tags = doc
        .tags
        .iter()
        .map(|tag| tag.to_lowercase())
        .collect::<Vec<_>>();

    if relative_path.contains(&query) || virtual_path.contains(&query) {
        score += 6;
        matched.push("path".to_string());
    }
    if title.contains(&query) {
        score += 5;
        matched.push("title".to_string());
    }
    if summary.contains(&query) {
        score += 4;
        matched.push("summary".to_string());
    }
    if kind.contains(&query) {
        score += 3;
        matched.push("kind".to_string());
    }
    if authority.contains(&query) {
        score += 2;
        matched.push("authority".to_string());
    }
    if status.contains(&query) {
        score += 2;
        matched.push("status".to_string());
    }
    if tags.iter().any(|tag| tag.contains(&query)) {
        score += 4;
        matched.push("tags".to_string());
    }
    if let Some(content) = content
        && content.to_lowercase().contains(&query)
    {
        score += 3;
        matched.push("content".to_string());
    }

    if score == 0 {
        None
    } else {
        matched.sort();
        matched.dedup();
        Some((score, matched))
    }
}

async fn search_project_docs(
    backend: &PlatformProjectToolsBackend,
    project_id: Uuid,
    docs: Vec<ProjectDocumentMetadata>,
    query: &str,
    filter: &ProjectDocFilterArgs,
    include_content: bool,
) -> Result<Vec<ProjectDocSearchHit>> {
    let docs = filter_project_docs(docs, filter);
    let related_doc = if let Some(related_to) = filter.related_to.as_ref() {
        Some(lookup_project_doc(backend, project_id, related_to).await?)
    } else {
        None
    };
    let mut hits = Vec::new();

    for doc in docs {
        if let Some(related) = related_doc.as_ref() {
            let neighbors =
                project_doc_neighbors(backend, project_id, related, filter.edge_type.as_deref())
                    .await?;
            let target = project_doc_virtual_path(doc.project_id, &doc);
            if !neighbors.iter().any(|neighbor| neighbor.target == target) {
                continue;
            }
        }

        let content = backend
            .client
            .get_project_document_content(project_id, doc.id)
            .await
            .ok()
            .map(|document| document.description);

        let Some((score, matched)) = project_doc_score(&doc, query, content.as_deref()) else {
            continue;
        };

        hits.push(ProjectDocSearchHit {
            id: doc.id.to_string(),
            virtual_path: project_doc_virtual_path(doc.project_id, &doc),
            title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
            summary: doc
                .summary
                .clone()
                .unwrap_or_else(|| format!("Project document {}", project_doc_relative_path(&doc))),
            kind: doc.kind.clone().unwrap_or_else(|| "reference".to_string()),
            authority: doc.authority.clone(),
            tags: doc.tags.clone(),
            score,
            matched,
            content: if include_content { content } else { None },
        });
    }

    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.virtual_path.cmp(&right.virtual_path))
    });

    Ok(hits)
}

fn project_doc_tree(
    project_id: Uuid,
    docs: &[ProjectDocumentMetadata],
    prefix: Option<&str>,
) -> ProjectDocTree {
    let normalized_prefix = prefix.map(|value| normalize_project_lookup(project_id, value));
    let mut entries = docs
        .iter()
        .filter_map(|doc| {
            let relative = project_doc_relative_path(doc);
            if let Some(prefix) = normalized_prefix.as_ref()
                && !relative.starts_with(prefix)
            {
                return None;
            }
            Some(ProjectDocTreeEntry {
                path: project_doc_virtual_path(project_id, doc),
                title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
                kind: doc.kind.clone().unwrap_or_else(|| "reference".to_string()),
                tags: doc.tags.clone(),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    ProjectDocTree {
        root_uri: format!("project://{project_id}/"),
        entries,
    }
}

async fn project_doc_neighbors(
    backend: &PlatformProjectToolsBackend,
    project_id: Uuid,
    doc: &ProjectDocumentMetadata,
    edge_type: Option<&str>,
) -> Result<Vec<ProjectDocNeighbor>> {
    let edges = backend
        .client
        .list_project_document_edges(project_id, doc.id)
        .await?;
    let docs = backend
        .client
        .list_project_document_metadata(project_id)
        .await?;
    let mut docs_by_id = HashMap::new();
    for entry in docs {
        docs_by_id.insert(entry.id, entry);
    }

    let mut neighbors = Vec::new();
    for edge in edges {
        if let Some(filter_edge_type) = edge_type
            && !edge.edge_type.eq_ignore_ascii_case(filter_edge_type)
        {
            continue;
        }
        if edge.source_document_id == doc.id {
            if let Some(target) = docs_by_id.get(&edge.target_document_id) {
                neighbors.push(project_doc_neighbor_from_edge(&edge, "outgoing", target));
            }
        } else if edge.target_document_id == doc.id
            && let Some(source) = docs_by_id.get(&edge.source_document_id)
        {
            neighbors.push(project_doc_neighbor_from_edge(&edge, "incoming", source));
        }
    }

    neighbors.sort_by(|left, right| left.target.cmp(&right.target));
    Ok(neighbors)
}

fn project_doc_neighbor_from_edge(
    edge: &ProjectDocumentEdge,
    direction: &str,
    target: &ProjectDocumentMetadata,
) -> ProjectDocNeighbor {
    ProjectDocNeighbor {
        edge_type: edge.edge_type.clone(),
        direction: direction.to_string(),
        target: project_doc_virtual_path(target.project_id, target),
        note: edge.note.clone(),
    }
}

#[derive(Debug, Clone, Copy)]
enum ProjectRestToolKind {
    ListProjectTasks,
    GetProjectTask,
    CreateProjectTask,
    BulkCreateProjectTasks,
    UpdateProjectTask,
    DeleteProjectTask,
    ListProjectExecutionRuns,
    GetProjectExecutionRun,
    StartProjectExecution,
    PauseProjectExecution,
    ResumeProjectExecution,
}

impl ProjectRestToolKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "list_project_tasks" => Some(Self::ListProjectTasks),
            "get_project_task" => Some(Self::GetProjectTask),
            "create_project_task" => Some(Self::CreateProjectTask),
            "bulk_create_project_tasks" => Some(Self::BulkCreateProjectTasks),
            "update_project_task" => Some(Self::UpdateProjectTask),
            "delete_project_task" => Some(Self::DeleteProjectTask),
            "list_project_execution_runs" => Some(Self::ListProjectExecutionRuns),
            "get_project_execution_run" => Some(Self::GetProjectExecutionRun),
            "start_project_execution" => Some(Self::StartProjectExecution),
            "pause_project_execution" => Some(Self::PauseProjectExecution),
            "resume_project_execution" => Some(Self::ResumeProjectExecution),
            _ => None,
        }
    }
}

struct ProjectRestTool {
    kind: ProjectRestToolKind,
    backend: PlatformProjectToolsBackend,
    spec: nenjo::ToolSpec,
}

impl ProjectRestTool {
    fn from_name(name: &str, backend: PlatformProjectToolsBackend) -> Option<Self> {
        let kind = ProjectRestToolKind::from_name(name)?;
        Some(Self {
            kind,
            backend,
            spec: project_rest_tool_spec(kind),
        })
    }
}

#[async_trait]
impl Tool for ProjectRestTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let output = match self.kind {
            ProjectRestToolKind::ListProjectTasks => {
                let args: ListProjectTasksArgs =
                    serde_json::from_value(args).context("invalid list_project_tasks args")?;
                self.backend
                    .client
                    .list_project_tasks(&ProjectTaskListQuery {
                        project_id: args.project_id,
                        status: args.status,
                        priority: args.priority,
                        task_type: args.task_type,
                        tags: args.tags.map(|tags| tags.join(",")),
                        routine_id: args.routine_id,
                        assigned_to: args.assigned_to,
                        assigned_agent_id: args.assigned_agent_id,
                        limit: args.limit,
                        offset: args.offset,
                    })
                    .await?
            }
            ProjectRestToolKind::GetProjectTask => {
                let args: GetProjectTaskArgs =
                    serde_json::from_value(args).context("invalid get_project_task args")?;
                self.backend.client.get_project_task(args.task_id).await?
            }
            ProjectRestToolKind::CreateProjectTask => {
                let args: CreateProjectTaskArgs =
                    serde_json::from_value(args).context("invalid create_project_task args")?;
                let body = self.backend.create_task_body(&args).await?;
                self.backend.client.create_project_task(&body).await?
            }
            ProjectRestToolKind::BulkCreateProjectTasks => {
                let args: BulkCreateProjectTasksArgs = serde_json::from_value(args)
                    .context("invalid bulk_create_project_tasks args")?;
                let body = self.backend.bulk_create_task_body(&args).await?;
                self.backend.client.bulk_create_project_tasks(&body).await?
            }
            ProjectRestToolKind::UpdateProjectTask => {
                let args: UpdateProjectTaskArgs =
                    serde_json::from_value(args).context("invalid update_project_task args")?;
                let body = self.backend.update_task_body(&args).await?;
                self.backend
                    .client
                    .update_project_task(args.task_id, &body)
                    .await?
            }
            ProjectRestToolKind::DeleteProjectTask => {
                let args: DeleteProjectTaskArgs =
                    serde_json::from_value(args).context("invalid delete_project_task args")?;
                self.backend
                    .client
                    .delete_project_task(args.task_id)
                    .await?;
                json!({ "deleted": true, "task_id": args.task_id })
            }
            ProjectRestToolKind::ListProjectExecutionRuns => {
                let args: ListProjectExecutionRunsArgs = serde_json::from_value(args)
                    .context("invalid list_project_execution_runs args")?;
                self.backend
                    .client
                    .list_project_execution_runs(&ProjectExecutionListQuery {
                        project_id: args.project_id,
                        agent_id: args.agent_id,
                        routine_id: args.routine_id,
                        status: args.status,
                        limit: args.limit,
                        offset: args.offset,
                    })
                    .await?
            }
            ProjectRestToolKind::GetProjectExecutionRun => {
                let args: GetProjectExecutionRunArgs = serde_json::from_value(args)
                    .context("invalid get_project_execution_run args")?;
                self.backend
                    .client
                    .get_project_execution_run(args.execution_run_id)
                    .await?
            }
            ProjectRestToolKind::StartProjectExecution => {
                let args: StartProjectExecutionArgs =
                    serde_json::from_value(args).context("invalid start_project_execution args")?;
                self.backend
                    .client
                    .create_execution_run(&CreateExecutionRequest {
                        project_id: args.project_id,
                        config: args.config.unwrap_or_else(|| json!({})),
                        model_count: args.model_count,
                        parallel_count: args.parallel_count,
                        initial_status: Some("running".to_string()),
                    })
                    .await?
            }
            ProjectRestToolKind::PauseProjectExecution => {
                let args: CommandProjectExecutionArgs =
                    serde_json::from_value(args).context("invalid pause_project_execution args")?;
                self.backend
                    .client
                    .command_project_execution_run(args.execution_run_id, "pause")
                    .await?
            }
            ProjectRestToolKind::ResumeProjectExecution => {
                let args: CommandProjectExecutionArgs = serde_json::from_value(args)
                    .context("invalid resume_project_execution args")?;
                self.backend
                    .client
                    .command_project_execution_run(args.execution_run_id, "resume")
                    .await?
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }
}

#[derive(Debug, Deserialize)]
struct ListProjectTasksArgs {
    project_id: Uuid,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    tags: Option<Vec<String>>,
    routine_id: Option<Uuid>,
    assigned_to: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GetProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct CreateProjectTaskArgs {
    project_id: Uuid,
    title: String,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    complexity: Option<i16>,
    tags: Option<Vec<String>>,
    required_tags: Option<Vec<String>>,
    slug: Option<String>,
    order_index: Option<i32>,
    assigned_to: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct BulkCreateProjectTasksArgs {
    project_id: Uuid,
    tasks: Vec<BulkCreateProjectTaskItemArgs>,
}

#[derive(Debug, Deserialize)]
struct BulkCreateProjectTaskItemArgs {
    title: String,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    complexity: Option<i16>,
    tags: Option<Vec<String>>,
    required_tags: Option<Vec<String>>,
    slug: Option<String>,
    order_index: Option<i32>,
    assigned_to: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct UpdateProjectTaskArgs {
    task_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    complexity: Option<i16>,
    tags: Option<Vec<String>>,
    required_tags: Option<Vec<String>>,
    slug: Option<String>,
    order_index: Option<i32>,
    assigned_to: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct DeleteProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ListProjectExecutionRunsArgs {
    project_id: Uuid,
    agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GetProjectExecutionRunArgs {
    execution_run_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct StartProjectExecutionArgs {
    project_id: Uuid,
    config: Option<serde_json::Value>,
    model_count: Option<i32>,
    parallel_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CommandProjectExecutionArgs {
    execution_run_id: Uuid,
}

fn project_rest_tool_spec(kind: ProjectRestToolKind) -> nenjo::ToolSpec {
    match kind {
        ProjectRestToolKind::ListProjectTasks => nenjo::ToolSpec {
            name: "list_project_tasks".into(),
            description: "List tasks for a project, with optional task-state filters.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": {"type": "string", "format": "uuid"},
                    "status": {"type": "string"},
                    "priority": {"type": "string"},
                    "type": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "routine_id": {"type": "string", "format": "uuid"},
                    "assigned_to": {"type": "string", "format": "uuid"},
                    "assigned_agent_id": {"type": "string", "format": "uuid"},
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"}
                },
                "required": ["project_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ProjectRestToolKind::GetProjectTask => nenjo::ToolSpec {
            name: "get_project_task".into(),
            description: "Fetch one task by ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "format": "uuid"}
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ProjectRestToolKind::CreateProjectTask => nenjo::ToolSpec {
            name: "create_project_task".into(),
            description: "Create a new task for a project. Task content is encrypted before it is sent to the platform.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": {"type": "string", "format": "uuid"},
                    "title": {"type": "string"},
                    "description": {"type": "string"},
                    "acceptance_criteria": {"type": "string"},
                    "status": {"type": "string"},
                    "priority": {"type": "string"},
                    "type": {"type": "string"},
                    "complexity": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "required_tags": {"type": "array", "items": {"type": "string"}},
                    "slug": {"type": "string"},
                    "order_index": {"type": "integer"},
                    "assigned_to": {"type": "string", "format": "uuid"},
                    "assigned_agent_id": {"type": "string", "format": "uuid"},
                    "routine_id": {"type": "string", "format": "uuid"},
                    "metadata": {"type": "object"}
                },
                "required": ["project_id", "title"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::BulkCreateProjectTasks => nenjo::ToolSpec {
            name: "bulk_create_project_tasks".into(),
            description: "Create multiple tasks for one project in a single request. Each task's content is encrypted before it is sent to the platform.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": {"type": "string", "format": "uuid"},
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "acceptance_criteria": {"type": "string"},
                                "status": {"type": "string"},
                                "priority": {"type": "string"},
                                "type": {"type": "string"},
                                "complexity": {"type": "integer"},
                                "tags": {"type": "array", "items": {"type": "string"}},
                                "required_tags": {"type": "array", "items": {"type": "string"}},
                                "slug": {"type": "string"},
                                "order_index": {"type": "integer"},
                                "assigned_to": {"type": "string", "format": "uuid"},
                                "assigned_agent_id": {"type": "string", "format": "uuid"},
                                "routine_id": {"type": "string", "format": "uuid"},
                                "metadata": {"type": "object"}
                            },
                            "required": ["title"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["project_id", "tasks"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::UpdateProjectTask => nenjo::ToolSpec {
            name: "update_project_task".into(),
            description: "Update a task. If you change title, description, acceptance criteria, tags, status, priority, type, complexity, or slug, the harness re-encrypts the task content automatically.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "format": "uuid"},
                    "title": {"type": "string"},
                    "description": {"type": "string"},
                    "acceptance_criteria": {"type": "string"},
                    "status": {"type": "string"},
                    "priority": {"type": "string"},
                    "type": {"type": "string"},
                    "complexity": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "required_tags": {"type": "array", "items": {"type": "string"}},
                    "slug": {"type": "string"},
                    "order_index": {"type": "integer"},
                    "assigned_to": {"type": "string", "format": "uuid"},
                    "assigned_agent_id": {"type": "string", "format": "uuid"},
                    "routine_id": {"type": "string", "format": "uuid"},
                    "metadata": {"type": "object"}
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::DeleteProjectTask => nenjo::ToolSpec {
            name: "delete_project_task".into(),
            description: "Delete a task by ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "format": "uuid"}
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::ListProjectExecutionRuns => nenjo::ToolSpec {
            name: "list_project_execution_runs".into(),
            description: "List execution runs for a project.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": {"type": "string", "format": "uuid"},
                    "agent_id": {"type": "string", "format": "uuid"},
                    "routine_id": {"type": "string", "format": "uuid"},
                    "status": {"type": "string"},
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"}
                },
                "required": ["project_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ProjectRestToolKind::GetProjectExecutionRun => nenjo::ToolSpec {
            name: "get_project_execution_run".into(),
            description: "Fetch one execution run by ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": {"type": "string", "format": "uuid"}
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ProjectRestToolKind::StartProjectExecution => nenjo::ToolSpec {
            name: "start_project_execution".into(),
            description: "Start a new execution run for a project immediately.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": {"type": "string", "format": "uuid"},
                    "config": {"type": "object"},
                    "model_count": {"type": "integer"},
                    "parallel_count": {"type": "integer"}
                },
                "required": ["project_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::PauseProjectExecution => nenjo::ToolSpec {
            name: "pause_project_execution".into(),
            description: "Pause a running execution run.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": {"type": "string", "format": "uuid"}
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ProjectRestToolKind::ResumeProjectExecution => nenjo::ToolSpec {
            name: "resume_project_execution".into(),
            description: "Resume a paused execution run.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": {"type": "string", "format": "uuid"}
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    }
}

impl ManifestContractTool {
    fn new(spec: nenjo::ToolSpec, backend: Arc<dyn ManifestMcpBackend>) -> Self {
        Self { spec, backend }
    }
}

#[async_trait]
impl Tool for ManifestContractTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let value =
            ManifestMcpContract::dispatch(self.backend.as_ref(), &self.spec.name, args).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&value)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }
}

#[async_trait]
impl ToolFactory for HarnessToolFactory {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        self.build_tools(agent, &self.security).await
    }

    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        security: Arc<SecurityPolicy>,
    ) -> Vec<Arc<dyn Tool>> {
        self.build_tools(agent, &security).await
    }

    fn workspace_dir(&self) -> std::path::PathBuf {
        self.security.workspace_dir.clone()
    }
}

// ---------------------------------------------------------------------------
// NativeRuntime — default RuntimeAdapter for local execution
// ---------------------------------------------------------------------------

/// Native runtime that uses local shell and filesystem.
pub struct NativeRuntime;

impl nenjo_tools::runtime::RuntimeAdapter for NativeRuntime {
    fn name(&self) -> &str {
        "native"
    }

    fn has_shell_access(&self) -> bool {
        true
    }

    fn has_filesystem_access(&self) -> bool {
        true
    }

    fn storage_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(".")
    }

    fn supports_long_running(&self) -> bool {
        true
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &std::path::Path,
    ) -> Result<tokio::process::Command> {
        let shell = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let flag = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };
        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg(flag).arg(command).current_dir(workspace_dir);
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nenjo::ManifestWriter;
    use nenjo::agents::prompts::PromptConfig;
    use nenjo::manifest::local::LocalManifestStore;
    use nenjo::manifest::{AbilityManifest, DomainManifest, Manifest};
    use nenjo_platform::{AbilitiesGetParams, AgentsGetParams, DomainsGetParams};
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;

    async fn scoped_backend(
        caller_scopes: Vec<String>,
    ) -> (
        PlatformManifestBackend<LocalManifestStore, WorkerAgentPromptPayloadEncoder>,
        AgentManifest,
        AgentManifest,
        AbilityManifest,
        AbilityManifest,
        DomainManifest,
        DomainManifest,
    ) {
        let temp = tempdir().unwrap();
        let root = temp.keep();
        let store = Arc::new(LocalManifestStore::new(root.join("manifests")));

        let visible_agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "visible-agent".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: None,
            domain_ids: vec![],
            platform_scopes: vec!["projects:read".into()],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        let hidden_agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "hidden-agent".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: None,
            domain_ids: vec![],
            platform_scopes: vec!["projects:write".into()],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };

        let visible_ability = AbilityManifest {
            id: Uuid::new_v4(),
            name: "visible-ability".into(),
            path: String::new(),
            display_name: None,
            description: None,
            activation_condition: "visible".into(),
            prompt_config: nenjo::types::AbilityPromptConfig {
                developer_prompt: "visible prompt".into(),
            },
            platform_scopes: vec!["projects:read".into()],
            mcp_server_ids: vec![],
            tool_name: "visible_ability".into(),
        };
        let hidden_ability = AbilityManifest {
            id: Uuid::new_v4(),
            name: "hidden-ability".into(),
            path: String::new(),
            display_name: None,
            description: None,
            activation_condition: "hidden".into(),
            prompt_config: nenjo::types::AbilityPromptConfig {
                developer_prompt: "hidden prompt".into(),
            },
            platform_scopes: vec!["projects:write".into()],
            mcp_server_ids: vec![],
            tool_name: "hidden_ability".into(),
        };

        let visible_domain = DomainManifest {
            id: Uuid::new_v4(),
            name: "visible-domain".into(),
            path: String::new(),
            display_name: "Visible Domain".into(),
            description: None,
            command: "#visible".into(),
            platform_scopes: vec!["projects:read".into()],
            ability_ids: vec![],
            mcp_server_ids: vec![],
            prompt_config: nenjo::types::DomainPromptConfig::default(),
        };
        let hidden_domain = DomainManifest {
            id: Uuid::new_v4(),
            name: "hidden-domain".into(),
            path: String::new(),
            display_name: "Hidden Domain".into(),
            description: None,
            command: "#hidden".into(),
            platform_scopes: vec!["projects:write".into()],
            ability_ids: vec![],
            mcp_server_ids: vec![],
            prompt_config: nenjo::types::DomainPromptConfig::default(),
        };

        store
            .replace_manifest(&Manifest {
                auth: Some(nenjo::manifest::ManifestAuth {
                    user_id: Uuid::new_v4(),
                    api_key_id: Some(Uuid::new_v4()),
                }),
                agents: vec![visible_agent.clone(), hidden_agent.clone()],
                abilities: vec![visible_ability.clone(), hidden_ability.clone()],
                domains: vec![visible_domain.clone(), hidden_domain.clone()],
                ..Default::default()
            })
            .await
            .unwrap();

        let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
        let inner = Arc::new(PlatformManifestBackend::new(
            store.clone(),
            client,
            WorkerAgentPromptPayloadEncoder {
                state_dir: root.join("state"),
            },
        ));

        (
            inner
                .as_ref()
                .clone()
                .with_access_policy(ManifestAccessPolicy::new(caller_scopes)),
            visible_agent,
            hidden_agent,
            visible_ability,
            hidden_ability,
            visible_domain,
            hidden_domain,
        )
    }

    #[tokio::test]
    async fn harness_factory_exposes_manifest_tools_without_legacy_platform_tools() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let config = crate::harness::config::Config {
            workspace_dir: root.join("workspace"),
            state_dir: root.join("state"),
            manifests_dir: root.join("manifests"),
            backend_api_url: Some("http://localhost:3001".into()),
            api_key: "test-api-key".into(),
            ..Default::default()
        };

        let security = Arc::new(SecurityPolicy::with_workspace_dir(
            config.workspace_dir.clone(),
        ));
        let runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter> = Arc::new(NativeRuntime);
        let external_mcp = Arc::new(crate::harness::external_mcp::ExternalMcpPool::new());
        let factory = HarnessToolFactory::new(security, runtime, config, external_mcp);

        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "tester".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: None,
            domain_ids: vec![],
            platform_scopes: vec![
                "agents:read".into(),
                "agents:write".into(),
                "projects:read".into(),
            ],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };

        let tools = factory.create_tools(&agent).await;
        let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

        assert!(names.iter().any(|name| name == "list_agents"));
        assert!(names.iter().any(|name| name == "get_agent"));
        assert!(names.iter().any(|name| name == "create_agent"));
        assert!(names.iter().any(|name| name == "update_agent"));
        assert!(names.iter().any(|name| name == "list_projects"));
        assert!(names.iter().any(|name| name == "get_project"));
        assert!(names.iter().any(|name| name == "list_project_docs"));
        assert!(names.iter().any(|name| name == "read_project_doc"));
        assert!(names.iter().any(|name| name == "search_project_docs"));
        assert!(names.iter().any(|name| name == "search_project_doc_paths"));
        assert!(names.iter().any(|name| name == "list_project_tasks"));
        assert!(names.iter().any(|name| name == "get_project_task"));
        assert!(
            names
                .iter()
                .any(|name| name == "list_project_execution_runs")
        );
        assert!(names.iter().any(|name| name == "get_project_execution_run"));
        assert!(names.iter().any(|name| name == "list_builtin_docs"));
        assert!(names.iter().any(|name| name == "read_builtin_doc"));
        assert!(names.iter().any(|name| name == "search_builtin_docs"));
        assert!(names.iter().any(|name| name == "search_builtin_doc_paths"));
        assert!(names.iter().any(|name| name == "list_builtin_doc_tree"));
        assert!(names.iter().any(|name| name == "read_builtin_doc_manifest"));
        assert!(
            names
                .iter()
                .any(|name| name == "list_builtin_doc_neighbors")
        );
        assert!(!names.iter().any(|name| name == "create_project_task"));
        assert!(!names.iter().any(|name| name == "start_project_execution"));

        assert!(!names.iter().any(|name| name == "platform_read"));
        assert!(!names.iter().any(|name| name == "platform_write"));
        assert!(!names.iter().any(|name| name == "platform_graph"));
    }

    #[tokio::test]
    async fn builtin_knowledge_tools_read_embedded_docs() {
        let read_tool = BuiltinKnowledgeTool::new(BuiltinKnowledgeToolKind::ReadDoc);
        let result = read_tool
            .execute(json!({"id_or_path": "nenjo.guide.routines"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("builtin://nenjo/guide/routines.md"));
        assert!(result.output.contains("# Routines"));

        let search_tool = BuiltinKnowledgeTool::new(BuiltinKnowledgeToolKind::SearchDocPaths);
        let result = search_tool
            .execute(json!({"query": "permission"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("nenjo.guide.scopes"));
        assert!(!result.output.contains("# Platform Scopes"));
    }

    #[tokio::test]
    async fn harness_factory_exposes_project_write_rest_tools_under_project_write_scope() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let config = crate::harness::config::Config {
            workspace_dir: root.join("workspace"),
            state_dir: root.join("state"),
            manifests_dir: root.join("manifests"),
            backend_api_url: Some("http://localhost:3001".into()),
            api_key: "test-api-key".into(),
            ..Default::default()
        };

        let security = Arc::new(SecurityPolicy::with_workspace_dir(
            config.workspace_dir.clone(),
        ));
        let runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter> = Arc::new(NativeRuntime);
        let external_mcp = Arc::new(crate::harness::external_mcp::ExternalMcpPool::new());
        let factory = HarnessToolFactory::new(security, runtime, config, external_mcp);

        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "tester".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: None,
            domain_ids: vec![],
            platform_scopes: vec!["projects:write".into()],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };

        let tools = factory.create_tools(&agent).await;
        let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

        assert!(names.iter().any(|name| name == "create_project_task"));
        assert!(names.iter().any(|name| name == "bulk_create_project_tasks"));
        assert!(names.iter().any(|name| name == "update_project_task"));
        assert!(names.iter().any(|name| name == "delete_project_task"));
        assert!(names.iter().any(|name| name == "start_project_execution"));
        assert!(names.iter().any(|name| name == "pause_project_execution"));
        assert!(names.iter().any(|name| name == "resume_project_execution"));
    }

    #[tokio::test]
    async fn platform_manifest_backend_filters_agents_abilities_and_domains_by_scopes() {
        let (
            backend,
            visible_agent,
            hidden_agent,
            visible_ability,
            hidden_ability,
            visible_domain,
            hidden_domain,
        ) = scoped_backend(vec!["projects:read".into()]).await;

        let agents = backend.agents_list().await.unwrap();
        assert_eq!(agents.agents.len(), 1);
        assert_eq!(agents.agents[0].id, visible_agent.id);
        assert!(
            backend
                .agents_get(AgentsGetParams {
                    id: hidden_agent.id
                })
                .await
                .is_err()
        );

        let abilities = backend.abilities_list().await.unwrap();
        assert_eq!(abilities.abilities.len(), 1);
        assert_eq!(abilities.abilities[0].id, visible_ability.id);
        assert!(
            backend
                .abilities_get(AbilitiesGetParams {
                    id: hidden_ability.id
                })
                .await
                .is_err()
        );

        let domains = backend.domains_list().await.unwrap();
        assert_eq!(domains.domains.len(), 1);
        assert!(
            domains
                .domains
                .iter()
                .any(|domain| domain.id == visible_domain.id)
        );
        assert!(
            backend
                .domains_get(DomainsGetParams {
                    id: hidden_domain.id
                })
                .await
                .is_err()
        );
    }
}
