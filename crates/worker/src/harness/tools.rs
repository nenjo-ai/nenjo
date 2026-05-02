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
    ContentScope, ManifestAccessPolicy, ManifestKind, ManifestMcpBackend, ManifestMcpContract,
    PlatformManifestBackend, PlatformManifestClient, ScopeResource, SensitivePayloadEncoder,
    client::{CreateExecutionRequest, ProjectExecutionListQuery, ProjectTaskListQuery},
    rest::projects::project_rest_tools,
};
use nenjo_tools::security::SecurityPolicy;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::crypto::WorkerAuthProvider;
use crate::crypto::{decrypt_text_with_provider, encrypt_text_with_provider};

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

fn payload_scope_for_object_type(object_type: &str) -> ContentScope {
    ManifestKind::from_encrypted_object_type(object_type)
        .and_then(ManifestKind::encrypted_scope)
        .unwrap_or(ContentScope::User)
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
        let scope = payload_scope_for_object_type(object_type);
        let encrypted_payload = encrypt_text_with_provider(
            &auth_provider,
            scope,
            account_id,
            object_id,
            object_type,
            &serde_json::to_string(payload)?,
        )
        .await?;
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
        let plaintext = decrypt_text_with_provider(&auth_provider, &encrypted_payload).await?;
        Ok(Some(serde_json::from_str(&plaintext)?))
    }
}

impl WorkerAgentPromptPayloadEncoder {
    async fn encode_org_payload(
        &self,
        org_id: uuid::Uuid,
        object_id: uuid::Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let auth_provider = WorkerAuthProvider::load_or_create(self.state_dir.join("crypto"))
            .context("failed to load worker auth provider")?;
        let encrypted_payload = encrypt_text_with_provider(
            &auth_provider,
            ContentScope::Org,
            org_id,
            object_id,
            object_type,
            &serde_json::to_string(payload)?,
        )
        .await?;
        serde_json::to_value(encrypted_payload).context("failed to serialize encrypted org payload")
    }

    async fn decode_task_payload(
        &self,
        payload: &nenjo_events::EncryptedPayload,
    ) -> Result<serde_json::Value> {
        let auth_provider = WorkerAuthProvider::load_or_create(self.state_dir.join("crypto"))
            .context("failed to load worker auth provider")?;
        let plaintext = decrypt_text_with_provider(&auth_provider, payload).await?;
        serde_json::from_str(&plaintext).context("failed to decode encrypted task payload JSON")
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
                Arc::new(
                    PlatformManifestBackend::new(
                        local_store.clone(),
                        client.as_ref().clone(),
                        payload_encoder.clone(),
                    )
                    .with_workspace_dir(config.workspace_dir.clone()),
                )
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

        let manifest_backend = self.manifest_backend.as_ref().map(|backend| {
            Arc::new(backend.as_ref().clone().with_access_policy(policy.clone()))
                as Arc<dyn ManifestMcpBackend>
        });

        if let Some(backend) = manifest_backend.as_ref() {
            add_manifest_tools(&mut tools, backend.clone(), &policy);
        }

        let project_backend =
            self.platform_client
                .as_ref()
                .map(|client| PlatformProjectToolsBackend {
                    client: client.clone(),
                    manifest_store: self.manifest_store.clone(),
                    payload_encoder: self.payload_encoder.clone(),
                });
        add_project_tools(&mut tools, manifest_backend, project_backend, &policy);

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
                "List builtin docs as compact metadata only. Use this to browse or filter the builtin knowledge set without loading full document content."
            }
            BuiltinKnowledgeToolKind::ReadDoc => {
                "Read one full builtin doc, including its body content, by id or builtin://nenjo/ path. Use this when you want the actual document text, not just metadata."
            }
            BuiltinKnowledgeToolKind::SearchDocs => {
                "Search builtin docs and return matches with body content. Use this when you want to inspect or quote the matching text, not just identify candidate docs."
            }
            BuiltinKnowledgeToolKind::SearchDocPaths => {
                "Search builtin docs and return compact metadata without body content. Use this for fast discovery or navigation when you only need to know which docs match."
            }
            BuiltinKnowledgeToolKind::ListTree => {
                "List the builtin doc tree under builtin://nenjo/. Use this when you want a filesystem-style view of the builtin knowledge namespace instead of a search result."
            }
            BuiltinKnowledgeToolKind::ReadManifest => {
                "Read one builtin doc's metadata only by id or path. Use this when you need title, tags, path, or other manifest fields but do not need the document body."
            }
            BuiltinKnowledgeToolKind::Neighbors => {
                "List graph neighbors for one builtin doc by id or path. Use this when you want related builtin docs connected by knowledge edges."
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
const PROJECT_MANIFEST_READ_TOOLS: &[&str] = &[
    "list_projects",
    "get_project",
    "list_project_documents",
    "read_project_document_manifest",
    "read_project_document",
    "search_project_documents",
    "search_project_document_paths",
    "list_project_document_tree",
    "list_project_document_neighbors",
];
const PROJECT_REST_READ_TOOLS: &[&str] = &[
    "list_project_tasks",
    "get_project_task",
    "list_project_execution_runs",
    "get_project_execution_run",
];
const PROJECT_MANIFEST_WRITE_TOOLS: &[&str] = &[
    "create_project",
    "update_project",
    "delete_project",
    "create_project_document",
    "update_project_document_content",
    "delete_project_document",
];
const PROJECT_REST_WRITE_TOOLS: &[&str] = &[
    "create_project_tasks",
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

fn add_project_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    manifest_backend: Option<Arc<dyn ManifestMcpBackend>>,
    project_backend: Option<PlatformProjectToolsBackend>,
    policy: &ManifestAccessPolicy,
) {
    let specs = manifest_tool_specs();
    if policy.can_read_resource(ScopeResource::Projects) {
        if let Some(backend) = manifest_backend.as_ref() {
            add_named_manifest_tools(tools, backend.clone(), &specs, PROJECT_MANIFEST_READ_TOOLS);
        }
        if let Some(backend) = project_backend.as_ref() {
            add_named_project_rest_tools(tools, backend.clone(), PROJECT_REST_READ_TOOLS);
        }
    }
    if policy.can_write_resource(ScopeResource::Projects) {
        if let Some(backend) = manifest_backend.as_ref() {
            add_named_manifest_tools(tools, backend.clone(), &specs, PROJECT_MANIFEST_WRITE_TOOLS);
        }
        if let Some(backend) = project_backend.as_ref() {
            add_named_project_rest_tools(tools, backend.clone(), PROJECT_REST_WRITE_TOOLS);
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

#[derive(Debug, Clone, Serialize)]
struct TaskContentPayload {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CurrentTaskState {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

impl PlatformProjectToolsBackend {
    async fn normalize_project_tool_args(
        &self,
        mut args: serde_json::Value,
        kind: ProjectRestToolKind,
    ) -> Result<serde_json::Value> {
        if !matches!(
            kind,
            ProjectRestToolKind::ListProjectTasks
                | ProjectRestToolKind::CreateProjectTasks
                | ProjectRestToolKind::ListProjectExecutionRuns
                | ProjectRestToolKind::StartProjectExecution
        ) {
            return Ok(args);
        }

        let Some(object) = args.as_object_mut() else {
            return Ok(args);
        };
        let Some((field_name, raw_project_id)) = object
            .get("project_id")
            .or_else(|| object.get("id"))
            .and_then(|value| value.as_str())
            .map(|value| {
                if object.contains_key("project_id") {
                    ("project_id", value.to_string())
                } else {
                    ("id", value.to_string())
                }
            })
        else {
            return Ok(args);
        };

        if Uuid::parse_str(&raw_project_id).is_ok() {
            return Ok(args);
        }

        let manifest = self.manifest_store.load_manifest().await?;
        let project_ids = manifest
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();

        if let Some(corrected) = unique_near_uuid_match(&raw_project_id, &project_ids) {
            object.insert(field_name.to_string(), json!(corrected));
        }

        Ok(args)
    }

    async fn org_id(&self) -> Result<Uuid> {
        let manifest = self.manifest_store.load_manifest().await?;
        if let Some(org_id) = manifest
            .auth
            .map(|auth| auth.org_id)
            .filter(|org_id| !org_id.is_nil())
        {
            return Ok(org_id);
        }

        self.client
            .current_org_id()
            .await
            .context("failed to derive org_id from authenticated API key")
    }

    async fn encode_task_payload(
        &self,
        task_id: Uuid,
        payload: &TaskContentPayload,
    ) -> Result<serde_json::Value> {
        let org_id = self.org_id().await?;
        self.payload_encoder
            .encode_org_payload(
                org_id,
                task_id,
                ManifestKind::Task
                    .encrypted_object_type()
                    .expect("task content object type"),
                &serde_json::to_value(payload).context("failed to encode task content payload")?,
            )
            .await
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

    async fn create_task_body(
        &self,
        project_id: Uuid,
        args: &CreateProjectTaskItemArgs,
    ) -> Result<serde_json::Value> {
        let task_id = Uuid::new_v4();
        let payload = TaskContentPayload {
            description: args.description.clone(),
            acceptance_criteria: args.acceptance_criteria.clone(),
        };
        let encrypted_payload = self.maybe_encode_task_payload(task_id, &payload).await?;

        Ok(json!({
            "id": task_id,
            "project_id": project_id,
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
            "assigned_agent_id": args.assigned_agent_id,
            "routine_id": args.routine_id,
            "encrypted_payload": encrypted_payload,
        }))
    }

    async fn create_tasks_body(&self, args: &CreateProjectTasksArgs) -> Result<serde_json::Value> {
        let mut tasks = Vec::with_capacity(args.tasks.len());
        for task in &args.tasks {
            let body = self.create_task_body(args.project_id, task).await?;
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
            let current_state = if args.description.is_some() && args.acceptance_criteria.is_some()
            {
                CurrentTaskState::default()
            } else {
                let current = self.client.get_project_task(args.task_id).await?;
                let mut current_state: CurrentTaskState =
                    serde_json::from_value(current.clone())
                        .context("failed to decode current task state")?;
                if let Some(encrypted_payload) = current
                    .get("encrypted_payload")
                    .cloned()
                    .map(serde_json::from_value::<nenjo_events::EncryptedPayload>)
                    .transpose()
                    .context("failed to parse current task encrypted payload")?
                {
                    current_state = serde_json::from_value(
                        self.payload_encoder
                            .decode_task_payload(&encrypted_payload)
                            .await?,
                    )
                    .context("failed to decode current task encrypted state")?;
                }
                current_state
            };
            let payload = TaskContentPayload {
                description: args.description.clone().or(current_state.description),
                acceptance_criteria: args
                    .acceptance_criteria
                    .clone()
                    .or(current_state.acceptance_criteria),
            };
            body.insert(
                "encrypted_payload".into(),
                self.encode_task_payload(args.task_id, &payload).await?,
            );
        }

        Ok(serde_json::Value::Object(body))
    }
}

#[derive(Debug, Clone, Copy)]
enum ProjectRestToolKind {
    ListProjectTasks,
    GetProjectTask,
    CreateProjectTasks,
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
            "create_project_tasks" => Some(Self::CreateProjectTasks),
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

    fn tool_name(&self) -> &'static str {
        match self {
            Self::ListProjectTasks => "list_project_tasks",
            Self::GetProjectTask => "get_project_task",
            Self::CreateProjectTasks => "create_project_tasks",
            Self::UpdateProjectTask => "update_project_task",
            Self::DeleteProjectTask => "delete_project_task",
            Self::ListProjectExecutionRuns => "list_project_execution_runs",
            Self::GetProjectExecutionRun => "get_project_execution_run",
            Self::StartProjectExecution => "start_project_execution",
            Self::PauseProjectExecution => "pause_project_execution",
            Self::ResumeProjectExecution => "resume_project_execution",
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
            spec: project_rest_tool_spec(kind)?,
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
        let args = self
            .backend
            .normalize_project_tool_args(args, self.kind)
            .await?;
        let output = match self.kind {
            ProjectRestToolKind::ListProjectTasks => {
                let args: ListProjectTasksArgs = parse_project_tool_args(
                    args,
                    "list_project_tasks",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .list_project_tasks(&ProjectTaskListQuery {
                        project_id: args.project_id,
                        status: args.status,
                        priority: args.priority,
                        task_type: args.task_type,
                        tags: args.tags.map(|tags| tags.join(",")),
                        routine_id: args.routine_id,
                        assigned_agent_id: args.assigned_agent_id,
                        limit: args.limit,
                        offset: args.offset,
                    })
                    .await?
            }
            ProjectRestToolKind::GetProjectTask => {
                let args: GetProjectTaskArgs = parse_project_tool_args(
                    args,
                    "get_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend.client.get_project_task(args.task_id).await?
            }
            ProjectRestToolKind::CreateProjectTasks => {
                let args: CreateProjectTasksArgs = parse_project_tool_args(
                    args,
                    "create_project_tasks",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\",\"tasks\":[{\"title\":\"...\"}]}.",
                )?;
                let body = self.backend.create_tasks_body(&args).await?;
                self.backend.client.bulk_create_project_tasks(&body).await?
            }
            ProjectRestToolKind::UpdateProjectTask => {
                let args: UpdateProjectTaskArgs = parse_project_tool_args(
                    args,
                    "update_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\", ...fields}.",
                )?;
                let body = self.backend.update_task_body(&args).await?;
                self.backend
                    .client
                    .update_project_task(args.task_id, &body)
                    .await?
            }
            ProjectRestToolKind::DeleteProjectTask => {
                let args: DeleteProjectTaskArgs = parse_project_tool_args(
                    args,
                    "delete_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .delete_project_task(args.task_id)
                    .await?;
                json!({ "deleted": true, "task_id": args.task_id })
            }
            ProjectRestToolKind::ListProjectExecutionRuns => {
                let args: ListProjectExecutionRunsArgs = parse_project_tool_args(
                    args,
                    "list_project_execution_runs",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
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
                let args: GetProjectExecutionRunArgs = parse_project_tool_args(
                    args,
                    "get_project_execution_run",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .get_project_execution_run(args.execution_run_id)
                    .await?
            }
            ProjectRestToolKind::StartProjectExecution => {
                let args: StartProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "start_project_execution",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
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
                let args: CommandProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "pause_project_execution",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .command_project_execution_run(args.execution_run_id, "pause")
                    .await?
            }
            ProjectRestToolKind::ResumeProjectExecution => {
                let args: CommandProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "resume_project_execution",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
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
#[serde(deny_unknown_fields)]
struct ListProjectTasksArgs {
    #[serde(alias = "id")]
    project_id: Uuid,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    tags: Option<Vec<String>>,
    routine_id: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "CreateProjectTasksInput")]
struct CreateProjectTasksArgs {
    project_id: Uuid,
    tasks: Vec<CreateProjectTaskItemArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
enum CreateProjectTasksInput {
    Bulk {
        #[serde(alias = "id")]
        project_id: Uuid,
        tasks: Vec<CreateProjectTaskItemArgs>,
    },
    SingleNested {
        #[serde(alias = "id")]
        project_id: Uuid,
        task: CreateProjectTaskItemArgs,
    },
    SingleFlat {
        #[serde(alias = "id")]
        project_id: Uuid,
        #[serde(flatten)]
        task: CreateProjectTaskItemArgs,
    },
}

impl TryFrom<CreateProjectTasksInput> for CreateProjectTasksArgs {
    type Error = String;

    fn try_from(input: CreateProjectTasksInput) -> std::result::Result<Self, Self::Error> {
        let (project_id, tasks) = match input {
            CreateProjectTasksInput::Bulk { project_id, tasks } => (project_id, tasks),
            CreateProjectTasksInput::SingleNested { project_id, task }
            | CreateProjectTasksInput::SingleFlat { project_id, task } => (project_id, vec![task]),
        };

        if tasks.is_empty() {
            return Err("tasks must contain at least one task".into());
        }

        Ok(Self { project_id, tasks })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectTaskItemArgs {
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
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ListProjectExecutionRunsArgs {
    #[serde(alias = "id")]
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
    #[serde(alias = "id")]
    project_id: Uuid,
    config: Option<serde_json::Value>,
    model_count: Option<i32>,
    parallel_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CommandProjectExecutionArgs {
    execution_run_id: Uuid,
}

fn project_rest_tool_spec(kind: ProjectRestToolKind) -> Option<nenjo::ToolSpec> {
    project_rest_tools()
        .into_iter()
        .find(|tool| tool.name == kind.tool_name())
}

fn parse_project_tool_args<T>(
    args: serde_json::Value,
    tool_name: &str,
    expected_shape: &str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        let received = serde_json::to_string(&args).unwrap_or_else(|_| "<unprintable>".into());
        anyhow!("invalid {tool_name} args: {error}. {expected_shape} Received: {received}")
    })
}

fn unique_near_uuid_match(raw: &str, candidates: &[Uuid]) -> Option<Uuid> {
    let normalized_raw = raw.trim().to_ascii_lowercase();
    let mut matches = candidates
        .iter()
        .copied()
        .filter(|candidate| edit_distance_at_most_one(&normalized_raw, &candidate.to_string()));

    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    let left = left.as_bytes();
    let right = right.as_bytes();
    let len_delta = left.len().abs_diff(right.len());
    if len_delta > 1 {
        return false;
    }

    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;

    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            i += 1;
            j += 1;
            continue;
        }

        edits += 1;
        if edits > 1 {
            return false;
        }

        match left.len().cmp(&right.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }

    edits + usize::from(i < left.len() || j < right.len()) <= 1
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
    use nenjo_platform::{
        AbilitiesGetParams, AbilityManifestBackend, AgentManifestBackend, AgentsGetParams,
        DomainManifestBackend, DomainsGetParams,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn payload_scope_uses_org_for_org_owned_manifest_resources() {
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Agent
                    .encrypted_object_type()
                    .expect("agent prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Ability
                    .encrypted_object_type()
                    .expect("ability prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Domain
                    .encrypted_object_type()
                    .expect("domain prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
            ),
            ContentScope::Org
        );
    }

    #[test]
    fn payload_scope_falls_back_to_user_for_other_payloads() {
        assert_eq!(
            payload_scope_for_object_type("chat.message"),
            ContentScope::User
        );
    }

    #[test]
    fn create_project_tasks_args_accept_bulk_and_single_task_shapes() {
        let project_id = Uuid::new_v4();

        let bulk: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "tasks": [
                    {
                        "title": "Bulk task",
                        "description": "Bulk task body"
                    }
                ]
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(bulk.project_id, project_id);
        assert_eq!(bulk.tasks.len(), 1);
        assert_eq!(bulk.tasks[0].title, "Bulk task");

        let nested: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "task": {
                    "title": "Nested single task"
                }
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(nested.tasks.len(), 1);
        assert_eq!(nested.tasks[0].title, "Nested single task");

        let flat: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "title": "Flat single task",
                "acceptance_criteria": "Done criteria"
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(flat.tasks.len(), 1);
        assert_eq!(flat.tasks[0].title, "Flat single task");
        assert_eq!(
            flat.tasks[0].acceptance_criteria.as_deref(),
            Some("Done criteria")
        );
    }

    #[test]
    fn project_tool_arg_errors_include_uuid_detail_and_received_args() {
        let error = parse_project_tool_args::<ListProjectTasksArgs>(
            json!({"project_id": "48e857455-ebb8-4678-8dd8-a9c1b7e9e140"}),
            "list_project_tasks",
            "Expected project_id.",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("invalid list_project_tasks args"));
        assert!(error.contains("UUID parsing failed"));
        assert!(error.contains("Received:"));
        assert!(error.contains("48e857455-ebb8-4678-8dd8-a9c1b7e9e140"));
    }

    #[test]
    fn malformed_project_id_can_be_repaired_from_unique_cached_project_id() {
        let project_id = Uuid::parse_str("48e85745-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();
        let raw = "48e857455-ebb8-46f8-8dd8-a9c1b7e9e140";

        assert_eq!(unique_near_uuid_match(raw, &[project_id]), Some(project_id));
    }

    #[test]
    fn malformed_project_id_repair_requires_unique_match() {
        let first = Uuid::parse_str("48e85745-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();
        let second = Uuid::parse_str("48e85755-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();

        assert_eq!(
            unique_near_uuid_match("48e8575-ebb8-46f8-8dd8-a9c1b7e9e140", &[first, second]),
            None
        );
    }

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
                    org_id: Uuid::new_v4(),
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
        assert!(names.iter().any(|name| name == "list_project_documents"));
        assert!(names.iter().any(|name| name == "read_project_document"));
        assert!(names.iter().any(|name| name == "search_project_documents"));
        assert!(
            names
                .iter()
                .any(|name| name == "search_project_document_paths")
        );
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

        let neighbor_tool = BuiltinKnowledgeTool::new(BuiltinKnowledgeToolKind::Neighbors);
        let result = neighbor_tool
            .execute(json!({"id_or_path": "nenjo.guide.routines"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("\"edge_type\""));
        assert!(result.output.contains("\"edges\""));
        assert!(result.output.contains("\"source\""));
        assert!(result.output.contains("\"target\""));
        assert!(
            result
                .output
                .contains("builtin://nenjo/taxonomy/workflow-patterns.md")
        );
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

        assert!(names.iter().any(|name| name == "create_project_tasks"));
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

        let agents = backend.list_agents().await.unwrap();
        assert_eq!(agents.agents.len(), 1);
        assert_eq!(agents.agents[0].id, visible_agent.id);
        assert!(
            backend
                .get_agent(AgentsGetParams {
                    id: hidden_agent.id
                })
                .await
                .is_err()
        );

        let abilities = backend.list_abilities().await.unwrap();
        assert_eq!(abilities.abilities.len(), 1);
        assert_eq!(abilities.abilities[0].id, visible_ability.id);
        assert!(
            backend
                .get_ability(AbilitiesGetParams {
                    id: hidden_ability.id
                })
                .await
                .is_err()
        );

        let domains = backend.list_domains().await.unwrap();
        assert_eq!(domains.domains.len(), 1);
        assert!(
            domains
                .domains
                .iter()
                .any(|domain| domain.id == visible_domain.id)
        );
        assert!(
            backend
                .get_domain(DomainsGetParams {
                    id: hidden_domain.id
                })
                .await
                .is_err()
        );
    }
}
