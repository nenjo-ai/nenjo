//! Platform-backed manifest backend implementations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    ManifestResource, ManifestResourceKind, ModelManifest, ProjectManifest, RoutineManifest,
};
use nenjo::{ManifestReader, ManifestWriter};
use nenjo_knowledge::tools::{
    KnowledgeDocReadResult, KnowledgeListArgs, KnowledgeNeighborArgs, KnowledgePackSummary,
    KnowledgeReadArgs, KnowledgeRegistry, KnowledgeSearchArgs, KnowledgeTreeArgs, knowledge_filter,
    knowledge_manifest_result, knowledge_search_result, parse_knowledge_enum,
};
use nenjo_knowledge::{KnowledgeDocEdgeType, KnowledgePack};
use uuid::Uuid;

use crate::client::PlatformManifestClient;
use crate::knowledge_backend::{
    ResolvedKnowledgePack, ensure_known_pack_selector, is_default_library_pack_selector,
    library_pack_selector, parse_library_pack_selector, unknown_pack,
};
use crate::library_knowledge::LibraryKnowledgePack;
use crate::manifest_contract::ManifestKind;
use crate::manifest_mcp::*;
use crate::policy::ManifestAccessPolicy;
use crate::prompt_merge::merge_prompt_config;

fn local_council_from_document(council: &CouncilDocument) -> CouncilManifest {
    CouncilManifest {
        id: council.summary.id,
        name: council.summary.name.clone(),
        leader_agent_id: council.summary.leader_agent_id,
        members: council
            .members
            .iter()
            .map(|member| nenjo::manifest::CouncilMemberManifest {
                agent_id: member.agent_id,
                agent_name: member.agent_name.clone(),
                priority: member.priority,
            })
            .collect(),
        delegation_strategy: council.summary.delegation_strategy,
    }
}

#[async_trait]
/// Encodes and decodes sensitive manifest payloads that should not be sent in plaintext.
pub trait SensitivePayloadEncoder: Send + Sync {
    /// Encode a payload before it is sent to the platform.
    async fn encode_payload(
        &self,
        account_id: Uuid,
        object_id: Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>>;

    /// Decode a payload returned from the platform.
    ///
    /// Returning `Ok(None)` indicates that the caller cannot decode the payload yet.
    async fn decode_payload(
        &self,
        _payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Default)]
/// Encoder implementation that leaves sensitive payload handling to the platform.
pub struct NoopSensitivePayloadEncoder;

#[async_trait]
impl SensitivePayloadEncoder for NoopSensitivePayloadEncoder {
    async fn encode_payload(
        &self,
        _account_id: Uuid,
        _object_id: Uuid,
        _object_type: &str,
        _payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

#[derive(Clone)]
/// Manifest MCP backend that serves local state and persists mutations back to the platform API.
pub struct PlatformManifestBackend<L, E> {
    local_store: Arc<L>,
    platform_client: PlatformManifestClient,
    sensitive_payload_encoder: E,
    access_policy: Option<ManifestAccessPolicy>,
    workspace_dir: Option<PathBuf>,
    cached_org_id: Option<Uuid>,
    current_library_slug: Option<String>,
}

impl<L, E> PlatformManifestBackend<L, E> {
    /// Create a backend backed by a local manifest store and platform HTTP client.
    pub fn new(
        local_store: Arc<L>,
        platform_client: PlatformManifestClient,
        sensitive_payload_encoder: E,
    ) -> Self {
        Self {
            local_store,
            platform_client,
            sensitive_payload_encoder,
            access_policy: None,
            workspace_dir: None,
            cached_org_id: None,
            current_library_slug: None,
        }
    }

    /// Attach a scope-based access policy used to filter reads and validate writes.
    pub fn with_access_policy(mut self, access_policy: ManifestAccessPolicy) -> Self {
        self.access_policy = Some(access_policy);
        self
    }

    /// Attach the worker workspace root used for local-first library knowledge reads.
    pub fn with_workspace_dir(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    /// Attach the org id cached from worker bootstrap metadata.
    pub fn with_cached_org_id(mut self, org_id: Option<Uuid>) -> Self {
        self.cached_org_id = org_id.filter(|id| !id.is_nil());
        self
    }

    /// Attach the default library slug used to resolve the `workspace` library pack alias.
    pub fn with_current_library_slug(mut self, pack_slug: Option<String>) -> Self {
        self.current_library_slug = pack_slug.filter(|slug| !slug.trim().is_empty());
        self
    }

    /// Return the local manifest store used for cached reads and write-through updates.
    pub fn local_store(&self) -> &Arc<L> {
        &self.local_store
    }

    /// Return the platform HTTP client used for remote hydration and persistence.
    pub fn platform_client(&self) -> &PlatformManifestClient {
        &self.platform_client
    }
}

impl<L, E> PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder,
{
    fn allow_agent(&self, agent: &AgentManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_agent(agent))
            .unwrap_or(true)
    }

    fn allow_ability(&self, ability: &AbilityManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_ability(ability))
            .unwrap_or(true)
    }

    fn allow_domain(&self, domain: &DomainManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_domain(domain))
            .unwrap_or(true)
    }

    async fn local_manifest_org_id(&self) -> Result<Uuid> {
        if let Some(org_id) = self.cached_org_id {
            return Ok(org_id);
        }

        self.platform_client
            .current_org_id()
            .await
            .context("failed to derive org_id from authenticated platform context")
    }

    async fn cached_or_remote_ability(&self, id: Uuid) -> Result<AbilityManifest> {
        if let Some(ability) = self.local_store.get_ability(id).await? {
            return Ok(ability);
        }

        let Some(remote) = self.platform_client.fetch_ability_document(id).await? else {
            return Err(anyhow!("ability not found in local manifest: {}", id));
        };

        let hydrated = AbilityManifest {
            id: remote.summary.id,
            name: remote.summary.name,
            tool_name: remote.summary.tool_name,
            path: remote.summary.path,
            display_name: remote.summary.display_name,
            description: remote.summary.description,
            activation_condition: remote.activation_condition,
            prompt_config: Default::default(),
            platform_scopes: remote.platform_scopes,
            mcp_server_ids: remote.mcp_server_ids,
            source_type: "native".to_string(),
            read_only: false,
            metadata: serde_json::json!({}),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(hydrated.clone()))
            .await?;
        Ok(hydrated)
    }

    fn workspace_dir(&self) -> Result<&Path> {
        self.workspace_dir
            .as_deref()
            .ok_or_else(|| anyhow!("knowledge tools require a configured workspace_dir"))
    }

    async fn workspace_library_dir(&self, pack_slug: &str) -> Result<PathBuf> {
        Ok(self
            .workspace_dir()?
            .join("library")
            .join("platform")
            .join(pack_slug))
    }

    async fn library_knowledge_pack(&self, pack_slug: &str) -> Result<LibraryKnowledgePack> {
        let pack_dir = self.workspace_library_dir(pack_slug).await?;
        LibraryKnowledgePack::load(&pack_dir)
            .ok_or_else(|| anyhow!("knowledge pack '{pack_slug}' is not cached locally"))
    }

    async fn resolve_knowledge_pack(&self, selector: &str) -> Result<ResolvedKnowledgePack> {
        ensure_known_pack_selector(selector)?;
        if is_default_library_pack_selector(selector) {
            let pack_slug = self.current_library_slug.as_deref().ok_or_else(|| {
                anyhow!(
                    "knowledge pack 'lib' requires a selected pack; use lib:<slug> outside pack context"
                )
            })?;
            return self
                .library_knowledge_pack(pack_slug)
                .await
                .map(ResolvedKnowledgePack::Library);
        }
        if selector.starts_with("lib:") {
            let pack_slug = parse_library_pack_selector(selector)?;
            return self
                .library_knowledge_pack(pack_slug)
                .await
                .map(ResolvedKnowledgePack::Library);
        }
        if selector.starts_with("git://") {
            let library_dir = self.workspace_dir()?.join("library").join("repos");
            return find_repo_knowledge_pack(&library_dir, selector)
                .map(ResolvedKnowledgePack::Library)
                .ok_or_else(|| anyhow!("knowledge pack '{selector}' is not cached locally"));
        }
        Err(unknown_pack(selector))
    }
}

fn find_repo_knowledge_pack(root: &Path, selector: &str) -> Option<LibraryKnowledgePack> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if let Some(pack) = LibraryKnowledgePack::load(&path) {
            if pack.manifest().root_uri().trim_end_matches('/') == selector {
                return Some(pack);
            }
        } else if let Some(pack) = find_repo_knowledge_pack(&path, selector) {
            return Some(pack);
        }
    }
    None
}

fn list_repo_knowledge_packs(root: &Path) -> Vec<LibraryKnowledgePack> {
    let mut packs = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return packs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if let Some(pack) = LibraryKnowledgePack::load(&path) {
            packs.push(pack);
        } else {
            packs.extend(list_repo_knowledge_packs(&path));
        }
    }
    packs
}

#[async_trait]
impl<L, E> KnowledgeRegistry for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>> {
        let mut packs = Vec::new();
        if self.workspace_dir.is_some() {
            let library_dir = self.workspace_dir()?.join("library").join("platform");
            if let Ok(entries) = std::fs::read_dir(library_dir) {
                for entry in entries.flatten() {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    if !file_type.is_dir() {
                        continue;
                    }
                    let Some(slug) = entry.file_name().to_str().map(str::to_string) else {
                        continue;
                    };
                    let selector = library_pack_selector(&slug);
                    let Some(pack) = LibraryKnowledgePack::load(entry.path()) else {
                        continue;
                    };
                    if self.current_library_slug.as_deref() == Some(slug.as_str()) {
                        packs.push(KnowledgePackSummary::new("lib", pack.manifest()));
                    }
                    packs.push(KnowledgePackSummary::new(selector, pack.manifest()));
                }
            }
            let repos_dir = self.workspace_dir()?.join("library").join("repos");
            for pack in list_repo_knowledge_packs(&repos_dir) {
                let selector = pack.manifest().root_uri().trim_end_matches('/').to_string();
                if selector.starts_with("git://") {
                    packs.push(KnowledgePackSummary::new(selector, pack.manifest()));
                }
            }
        }
        Ok(packs)
    }

    async fn resolve_pack(&self, selector: &str) -> Result<Arc<dyn KnowledgePack>> {
        self.resolve_knowledge_pack(selector)
            .await
            .map(|pack| Arc::new(pack) as Arc<dyn KnowledgePack>)
    }
}

#[async_trait]
impl<L, E> KnowledgeManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_knowledge_packs(&self) -> Result<serde_json::Value> {
        let packs = KnowledgeRegistry::list_packs(self).await?;
        serde_json::to_value(packs).map_err(Into::into)
    }

    async fn list_knowledge_docs(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeListArgs =
            serde_json::from_value(params).context("invalid list_knowledge_docs args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let filter = knowledge_filter(args.filter)?;
        serde_json::to_value(
            pack.list_docs(filter)
                .into_iter()
                .map(|doc| knowledge_manifest_result(&args.pack, doc))
                .collect::<Vec<_>>(),
        )
        .map_err(Into::into)
    }

    async fn read_knowledge_doc_manifest(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: KnowledgeReadArgs =
            serde_json::from_value(params).context("invalid read_knowledge_doc_manifest args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let manifest = pack.read_manifest(&args.path).ok_or_else(|| {
            anyhow!(
                "unknown knowledge doc '{}' in pack '{}'",
                args.path,
                args.pack
            )
        })?;
        serde_json::to_value(knowledge_manifest_result(&args.pack, manifest)).map_err(Into::into)
    }

    async fn read_knowledge_doc(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeReadArgs =
            serde_json::from_value(params).context("invalid read_knowledge_doc args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let doc = pack.read_doc(&args.path).ok_or_else(|| {
            anyhow!(
                "unknown knowledge doc '{}' in pack '{}'",
                args.path,
                args.pack
            )
        })?;
        serde_json::to_value(KnowledgeDocReadResult {
            manifest: knowledge_manifest_result(&args.pack, &doc.manifest),
            content: doc.content,
        })
        .map_err(Into::into)
    }

    async fn search_knowledge(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeSearchArgs =
            serde_json::from_value(params).context("invalid search_knowledge args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let filter = knowledge_filter(args.filter)?;
        serde_json::to_value(
            pack.search_docs(&args.query, filter)
                .into_iter()
                .map(|hit| knowledge_search_result(&args.pack, hit))
                .collect::<Vec<_>>(),
        )
        .map_err(Into::into)
    }

    async fn search_knowledge_paths(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeSearchArgs =
            serde_json::from_value(params).context("invalid search_knowledge_paths args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let filter = knowledge_filter(args.filter)?;
        serde_json::to_value(
            pack.search_paths(&args.query, filter)
                .into_iter()
                .map(|hit| knowledge_search_result(&args.pack, hit))
                .collect::<Vec<_>>(),
        )
        .map_err(Into::into)
    }

    async fn list_knowledge_tree(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeTreeArgs =
            serde_json::from_value(params).context("invalid list_knowledge_tree args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        serde_json::to_value(pack.list_tree(args.prefix.as_deref())).map_err(Into::into)
    }

    async fn list_knowledge_neighbors(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: KnowledgeNeighborArgs =
            serde_json::from_value(params).context("invalid list_knowledge_neighbors args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let edge_type: Option<KnowledgeDocEdgeType> = parse_knowledge_enum(args.edge_type)?;
        serde_json::to_value(pack.neighbors(&args.path, edge_type)).map_err(Into::into)
    }
}

#[async_trait]
impl<L, E> AgentManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_agents(&self) -> Result<AgentsListResult> {
        let agents: Vec<AgentSummary> = self
            .local_store
            .list_agents()
            .await?
            .into_iter()
            .filter(|agent| self.allow_agent(agent))
            .map(|agent| AgentDocument::from(agent).summary)
            .collect();
        Ok(AgentsListResult { agents })
    }

    async fn get_agent(&self, params: AgentsGetParams) -> Result<AgentGetResult> {
        let agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        Ok(AgentGetResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn get_agent_prompt(&self, params: AgentPromptGetParams) -> Result<AgentPromptGetResult> {
        let agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        Ok(AgentPromptGetResult {
            agent: AgentPromptDocument::from(agent),
        })
    }

    async fn create_agent(&self, params: AgentCreateParams) -> Result<AgentMutationResult> {
        let create = AgentCreateDocument {
            name: params.data.name,
            description: params.data.description,
            color: params.data.color,
            model_id: params.data.model_id,
        };

        let created = self.platform_client.create_agent_document(&create).await?;

        let local_agent: AgentManifest = created.clone().into();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;

        Ok(AgentMutationResult { agent: created })
    }

    async fn update_agent(&self, params: AgentUpdateParams) -> Result<AgentMutationResult> {
        let existing = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        let merged = AgentUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            color: Some(params.data.color.unwrap_or_else(|| existing.color.clone())),
            model_id: Some(params.data.model_id.unwrap_or(existing.model_id)),
        };
        let updated = self
            .platform_client
            .update_agent_document(params.id, &merged)
            .await?;

        let mut local_agent: AgentManifest = updated.clone().into();
        local_agent.prompt_config = existing.prompt_config.clone();
        local_agent.heartbeat = existing.heartbeat.clone();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;

        Ok(AgentMutationResult { agent: updated })
    }

    async fn update_agent_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult> {
        let mut agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        if agent.prompt_locked {
            return Err(anyhow!("agent prompt is locked: {}", params.id));
        }
        if let Some(prompt_patch) = params.prompt_config {
            agent.prompt_config = merge_prompt_config(&agent.prompt_config, prompt_patch)?;
        }
        let prompt_patch = agent.prompt_config.clone();
        let prompt_payload = serde_json::to_value(&prompt_patch)?;
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                params.id,
                ManifestKind::Agent
                    .encrypted_object_type()
                    .expect("agent prompt object type"),
                &prompt_payload,
            )
            .await?;
        let prompt_config = self
            .platform_client
            .update_agent_prompt_document(params.id, &prompt_payload, encrypted_payload)
            .await?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or(prompt_patch);

        agent.prompt_config = prompt_config.clone();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(agent))
            .await?;

        Ok(AgentPromptMutationResult { prompt_config })
    }

    async fn delete_agent(&self, params: AgentDeleteParams) -> Result<DeleteResult> {
        let existing = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        self.platform_client
            .delete_agent_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Agent, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> AbilityManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_abilities(&self) -> Result<AbilitiesListResult> {
        let abilities: Vec<AbilitySummary> = self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .filter(|ability| self.allow_ability(ability))
            .map(|ability| AbilityDocument::from(ability).summary)
            .collect();
        Ok(AbilitiesListResult { abilities })
    }

    async fn get_ability(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult> {
        let ability = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        Ok(AbilityGetResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn get_ability_prompt(
        &self,
        params: AbilityPromptGetParams,
    ) -> Result<AbilityPromptGetResult> {
        let ability = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        Ok(AbilityPromptGetResult {
            ability: AbilityPromptDocument::from(ability),
        })
    }

    async fn create_ability(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                Uuid::new_v4(),
                ManifestKind::Ability
                    .encrypted_object_type()
                    .expect("ability prompt object type"),
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_ability_document(&params.data, encrypted_payload)
            .await?;
        let local_ability = AbilityManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            tool_name: created.summary.tool_name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            activation_condition: created.activation_condition.clone(),
            prompt_config: params.data.prompt_config.clone(),
            platform_scopes: created.platform_scopes.clone(),
            mcp_server_ids: created.mcp_server_ids.clone(),
            source_type: "native".to_string(),
            read_only: false,
            metadata: serde_json::json!({}),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityMutationResult { ability: created })
    }

    async fn update_ability(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "ability update requires at least one field in data"
            ));
        }
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        let merged = AbilityUpdateDocument {
            tool_name: params
                .data
                .tool_name
                .or_else(|| Some(existing.tool_name.clone())),
            display_name: Some(
                params
                    .data
                    .display_name
                    .unwrap_or_else(|| existing.display_name.clone()),
            ),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            activation_condition: params
                .data
                .activation_condition
                .or_else(|| Some(existing.activation_condition.clone())),
            mcp_server_ids: params
                .data
                .mcp_server_ids
                .or_else(|| Some(existing.mcp_server_ids.clone())),
        };
        let updated = self
            .platform_client
            .update_ability_document(params.id, &merged)
            .await?;
        let local_ability = AbilityManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            tool_name: updated.summary.tool_name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            activation_condition: updated.activation_condition.clone(),
            prompt_config: existing.prompt_config.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            mcp_server_ids: updated.mcp_server_ids.clone(),
            source_type: existing.source_type.clone(),
            read_only: existing.read_only,
            metadata: existing.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityMutationResult { ability: updated })
    }

    async fn update_ability_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult> {
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        let prompt_config = params.prompt_config;
        let updated = self
            .platform_client
            .update_ability_prompt_document(
                params.id,
                &prompt_config,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        params.id,
                        ManifestKind::Ability
                            .encrypted_object_type()
                            .expect("ability prompt object type"),
                        &serde_json::json!(prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_ability = AbilityManifest {
            id: existing.id,
            name: existing.name,
            tool_name: existing.tool_name,
            path: existing.path,
            display_name: existing.display_name,
            description: existing.description,
            activation_condition: existing.activation_condition,
            prompt_config: updated.prompt_config.clone(),
            platform_scopes: existing.platform_scopes,
            mcp_server_ids: existing.mcp_server_ids,
            source_type: existing.source_type,
            read_only: existing.read_only,
            metadata: existing.metadata,
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityPromptMutationResult {
            prompt_config: updated.prompt_config,
        })
    }

    async fn delete_ability(&self, params: AbilityDeleteParams) -> Result<DeleteResult> {
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        self.platform_client
            .delete_ability_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Ability, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> DomainManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_domains(&self) -> Result<DomainsListResult> {
        let domains: Vec<DomainSummary> = self
            .local_store
            .list_domains()
            .await?
            .into_iter()
            .filter(|domain| self.allow_domain(domain))
            .map(|domain| DomainDocument::from(domain).summary)
            .collect();
        Ok(DomainsListResult { domains })
    }

    async fn get_domain(&self, params: DomainsGetParams) -> Result<DomainGetResult> {
        let domain = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        Ok(DomainGetResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn get_domain_prompt(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult> {
        let domain = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        Ok(DomainManifestGetResult {
            domain: DomainManifestDocument::from(domain),
        })
    }

    async fn create_domain(&self, params: DomainCreateParams) -> Result<DomainMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                Uuid::new_v4(),
                ManifestKind::Domain
                    .encrypted_object_type()
                    .expect("domain prompt object type"),
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_domain_document(&params.data, encrypted_payload)
            .await?;
        let local_domain = DomainManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            command: created.command.clone(),
            platform_scopes: created.platform_scopes.clone(),
            ability_ids: params.data.ability_ids.clone().unwrap_or_default(),
            mcp_server_ids: params.data.mcp_server_ids.clone().unwrap_or_default(),
            prompt_config: params.data.prompt_config.clone().unwrap_or_default(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainMutationResult { domain: created })
    }

    async fn update_domain(&self, params: DomainUpdateParams) -> Result<DomainMutationResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        if params.data.is_empty() {
            return Err(anyhow!("domain update requires at least one field"));
        }
        let merged = DomainUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            display_name: params
                .data
                .display_name
                .or_else(|| Some(existing.display_name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            command: params
                .data
                .command
                .or_else(|| Some(existing.command.clone())),
            ability_ids: Some(
                params
                    .data
                    .ability_ids
                    .unwrap_or_else(|| existing.ability_ids.clone()),
            ),
            mcp_server_ids: Some(
                params
                    .data
                    .mcp_server_ids
                    .unwrap_or_else(|| existing.mcp_server_ids.clone()),
            ),
        };
        let updated = self
            .platform_client
            .update_domain_document(params.id, &merged)
            .await?;
        let local_domain = DomainManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            command: updated.command.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            ability_ids: updated.ability_ids.clone(),
            mcp_server_ids: updated.mcp_server_ids.clone(),
            prompt_config: existing.prompt_config.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainMutationResult { domain: updated })
    }

    async fn update_domain_prompt(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        if let Some(policy) = &self.access_policy
            && !policy.validate_domain_scopes(&existing.platform_scopes)
        {
            return Err(anyhow!("requested domain scopes exceed caller permissions"));
        }
        let updated = self
            .platform_client
            .update_domain_manifest_document(
                params.id,
                params.prompt_config.clone(),
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        params.id,
                        ManifestKind::Domain
                            .encrypted_object_type()
                            .expect("domain prompt object type"),
                        &serde_json::json!(params.prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_domain = DomainManifest {
            id: existing.id,
            name: existing.name.clone(),
            path: existing.path.clone(),
            display_name: existing.display_name.clone(),
            description: existing.description.clone(),
            command: existing.command.clone(),
            platform_scopes: existing.platform_scopes.clone(),
            ability_ids: existing.ability_ids.clone(),
            mcp_server_ids: existing.mcp_server_ids.clone(),
            prompt_config: updated.prompt_config.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainManifestMutationResult {
            prompt_config: updated.prompt_config,
        })
    }

    async fn delete_domain(&self, params: DomainDeleteParams) -> Result<DeleteResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        self.platform_client
            .delete_domain_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Domain, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> ProjectManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_projects(&self) -> Result<ProjectsListResult> {
        let projects: Vec<ProjectSummary> = self
            .local_store
            .list_projects()
            .await?
            .into_iter()
            .map(|project| ProjectDocument::from(project).summary)
            .collect();
        Ok(ProjectsListResult { projects })
    }

    async fn get_project(&self, params: ProjectsGetParams) -> Result<ProjectGetResult> {
        let project = self
            .local_store
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found in local manifest: {}", params.id))?;
        Ok(ProjectGetResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn create_project(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult> {
        let created = self
            .platform_client
            .create_project_document(&params.data)
            .await?;
        let local_project = ProjectManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            slug: created.summary.slug.clone(),
            description: created.summary.description.clone(),
            settings: created.settings.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Project(local_project))
            .await?;
        Ok(ProjectMutationResult { project: created })
    }

    async fn update_project(&self, params: ProjectUpdateParams) -> Result<ProjectMutationResult> {
        let existing = self
            .local_store
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found in local manifest: {}", params.id))?;
        let merged = ProjectUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            repo_url: Some(params.data.repo_url.unwrap_or_else(|| {
                existing
                    .settings
                    .get("repo_url")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
            })),
        };
        let updated = self
            .platform_client
            .update_project_document(params.id, &merged)
            .await?;
        let local_project = ProjectManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            slug: updated.summary.slug.clone(),
            description: updated.summary.description.clone(),
            settings: updated.settings.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Project(local_project))
            .await?;
        Ok(ProjectMutationResult { project: updated })
    }

    async fn delete_project(&self, params: ProjectDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_project_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Project, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn create_knowledge_item(
        &self,
        params: KnowledgeItemCreateParams,
    ) -> Result<KnowledgeItemMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                Uuid::new_v4(),
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
                &serde_json::Value::String(params.data.content.clone()),
            )
            .await?;
        let knowledge_item = self
            .platform_client
            .create_knowledge_item(&params.data, encrypted_payload)
            .await?;
        Ok(KnowledgeItemMutationResult { knowledge_item })
    }

    async fn update_knowledge_item_content(
        &self,
        params: KnowledgeItemContentUpdateParams,
    ) -> Result<KnowledgeItemContentMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                params.item_id,
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
                &serde_json::Value::String(params.content.clone()),
            )
            .await?;
        let knowledge_item = self
            .platform_client
            .update_knowledge_item_content(
                params.pack_id,
                params.item_id,
                &params.content,
                encrypted_payload,
            )
            .await?;
        Ok(KnowledgeItemContentMutationResult { knowledge_item })
    }

    async fn delete_knowledge_item(
        &self,
        params: KnowledgeItemDeleteParams,
    ) -> Result<DeleteResult> {
        self.platform_client
            .delete_knowledge_item(params.pack_id, params.item_id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.item_id,
        })
    }
}

#[async_trait]
impl<L, E> RoutineManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_routines(&self) -> Result<RoutinesListResult> {
        let routines = self
            .local_store
            .list_routines()
            .await?
            .into_iter()
            .map(|routine| RoutineDocument::from(routine).summary)
            .collect();
        Ok(RoutinesListResult { routines })
    }

    async fn get_routine(&self, params: RoutinesGetParams) -> Result<RoutineGetResult> {
        let routine = self
            .local_store
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found in local manifest: {}", params.id))?;
        Ok(RoutineGetResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn create_routine(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult> {
        let created = self
            .platform_client
            .create_routine_document(&params.data)
            .await?;
        let local_routine = RoutineManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            description: created.summary.description.clone(),
            trigger: created.summary.trigger,
            steps: created.steps.clone(),
            edges: created.edges.clone(),
            metadata: created.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        Ok(RoutineMutationResult { routine: created })
    }

    async fn update_routine(&self, params: RoutineUpdateParams) -> Result<RoutineMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "routine update requires at least one field in data"
            ));
        }
        let existing = self
            .local_store
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found in local manifest: {}", params.id))?;
        let merged = RoutineUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            trigger: params.data.trigger.or(Some(existing.trigger)),
            metadata: params
                .data
                .metadata
                .or_else(|| Some(existing.metadata.clone())),
            graph: params
                .data
                .graph
                .or_else(|| Some(RoutineDocument::from(existing).graph_input())),
        };
        let updated = self
            .platform_client
            .update_routine_document(params.id, &merged)
            .await?;
        let local_routine = RoutineManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            description: updated.summary.description.clone(),
            trigger: updated.summary.trigger,
            steps: updated.steps.clone(),
            edges: updated.edges.clone(),
            metadata: updated.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        Ok(RoutineMutationResult { routine: updated })
    }

    async fn delete_routine(&self, params: RoutineDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_routine_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Routine, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> ModelManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_models(&self) -> Result<ModelsListResult> {
        let models = self
            .local_store
            .list_models()
            .await?
            .into_iter()
            .map(|model| ModelDocument::from(model).summary)
            .collect();
        Ok(ModelsListResult { models })
    }

    async fn get_model(&self, params: ModelsGetParams) -> Result<ModelGetResult> {
        let model = self
            .local_store
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found in local manifest: {}", params.id))?;
        Ok(ModelGetResult {
            model: ModelDocument::from(model),
        })
    }

    async fn create_model(&self, params: ModelCreateParams) -> Result<ModelMutationResult> {
        let created = self
            .platform_client
            .create_model_document(&params.data)
            .await?;
        let local_model = ModelManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            description: created.summary.description.clone(),
            model: created.summary.model.clone(),
            model_provider: created.summary.model_provider.clone(),
            temperature: created.temperature,
            base_url: created.base_url.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Model(local_model))
            .await?;
        Ok(ModelMutationResult { model: created })
    }

    async fn update_model(&self, params: ModelUpdateParams) -> Result<ModelMutationResult> {
        let existing = self
            .local_store
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found in local manifest: {}", params.id))?;
        let merged = ModelUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            model: params.data.model.or_else(|| Some(existing.model.clone())),
            model_provider: params
                .data
                .model_provider
                .or_else(|| Some(existing.model_provider.clone())),
            temperature: params.data.temperature.or(existing.temperature),
            base_url: Some(params.data.base_url.unwrap_or(existing.base_url.clone())),
        };
        let updated = self
            .platform_client
            .update_model_document(params.id, &merged)
            .await?;
        let local_model = ModelManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            description: updated.summary.description.clone(),
            model: updated.summary.model.clone(),
            model_provider: updated.summary.model_provider.clone(),
            temperature: updated.temperature,
            base_url: updated.base_url.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Model(local_model))
            .await?;
        Ok(ModelMutationResult { model: updated })
    }

    async fn delete_model(&self, params: ModelDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_model_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Model, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> CouncilManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_councils(&self) -> Result<CouncilsListResult> {
        let councils = self
            .local_store
            .list_councils()
            .await?
            .into_iter()
            .map(|council| CouncilDocument::from(council).summary)
            .collect();
        Ok(CouncilsListResult { councils })
    }

    async fn get_council(&self, params: CouncilsGetParams) -> Result<CouncilGetResult> {
        let council = self
            .local_store
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found in local manifest: {}", params.id))?;
        Ok(CouncilGetResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn create_council(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult> {
        let created = self
            .platform_client
            .create_council_document(&params.data)
            .await?;
        let local_council = local_council_from_document(&created);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: created })
    }

    async fn update_council(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult> {
        let existing = self
            .local_store
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found in local manifest: {}", params.id))?;
        let merged = CouncilUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: params.data.description,
            delegation_strategy: params
                .data
                .delegation_strategy
                .or(Some(existing.delegation_strategy)),
            config: params.data.config,
        };
        let updated = self
            .platform_client
            .update_council_document(params.id, &merged)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn add_council_member(
        &self,
        params: CouncilAddMemberParams,
    ) -> Result<CouncilMutationResult> {
        let updated = self
            .platform_client
            .add_council_member_document(params.council_id, &params.data)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn update_council_member(
        &self,
        params: CouncilUpdateMemberParams,
    ) -> Result<CouncilMutationResult> {
        if params.data.is_empty() {
            bail!("council member update requires at least one field");
        }
        let updated = self
            .platform_client
            .update_council_member_document(params.council_id, params.agent_id, &params.data)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn remove_council_member(
        &self,
        params: CouncilRemoveMemberParams,
    ) -> Result<CouncilMutationResult> {
        let updated = self
            .platform_client
            .remove_council_member_document(params.council_id, params.agent_id)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn delete_council(&self, params: CouncilDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_council_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Council, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<L, E> ContextBlockManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_context_blocks(&self) -> Result<ContextBlocksListResult> {
        let context_blocks: Vec<ContextBlockSummary> = self
            .local_store
            .list_context_blocks()
            .await?
            .into_iter()
            .map(|context_block| ContextBlockDocument::from(context_block).summary)
            .collect();
        Ok(ContextBlocksListResult { context_blocks })
    }

    async fn get_context_block(
        &self,
        params: ContextBlocksGetParams,
    ) -> Result<ContextBlockGetResult> {
        let context_block = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        Ok(ContextBlockGetResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn get_context_block_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult> {
        let context_block = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        Ok(ContextBlockContentGetResult {
            context_block: ContextBlockContentDocument::from(context_block),
        })
    }

    async fn create_context_block(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                Uuid::new_v4(),
                ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type"),
                &serde_json::json!(params.data.template.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_context_block_document(&params.data, encrypted_payload)
            .await?;
        let local_context_block = ContextBlockManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            template: params.data.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: created,
        })
    }

    async fn update_context_block(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult> {
        let existing = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        let merged = ContextBlockUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            display_name: Some(
                params
                    .data
                    .display_name
                    .unwrap_or_else(|| existing.display_name.clone()),
            ),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            template: None,
        };
        let updated = self
            .platform_client
            .update_context_block_document(params.id, &merged)
            .await?;
        let local_context_block = ContextBlockManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            template: existing.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: updated,
        })
    }

    async fn update_context_block_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult> {
        let existing = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        let template = params.template.unwrap_or_else(|| existing.template.clone());
        let updated = self
            .platform_client
            .update_context_block_content_document(
                params.id,
                &template,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        params.id,
                        ManifestKind::ContextBlock
                            .encrypted_object_type()
                            .expect("context block content object type"),
                        &serde_json::json!(template.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_context_block = ContextBlockManifest {
            id: existing.id,
            name: existing.name.clone(),
            path: existing.path.clone(),
            display_name: existing.display_name.clone(),
            description: existing.description.clone(),
            template: updated.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockContentMutationResult {
            template: updated.template,
        })
    }

    async fn delete_context_block(&self, params: ContextBlockDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_context_block_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::ContextBlock, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    use nenjo::manifest::ProjectManifest;
    use nenjo::manifest::local::LocalManifestStore;
    use nenjo::{ManifestResource, ManifestWriter};

    use super::*;

    async fn library_backend_fixture() -> Result<(
        PlatformManifestBackend<LocalManifestStore, NoopSensitivePayloadEncoder>,
        Uuid,
        String,
        TempDir,
    )> {
        let temp = tempdir()?;
        let manifests_dir = temp.path().join("manifests");
        let workspace_dir = temp.path().join("workspace");
        let project_id = Uuid::new_v4();
        let pack_slug = "graph-eval";
        let library_dir = workspace_dir
            .join("library")
            .join("platform")
            .join(pack_slug);
        std::fs::create_dir_all(library_dir.join("docs"))?;

        let store = Arc::new(LocalManifestStore::new(manifests_dir));
        store
            .upsert_resource(&ManifestResource::Project(ProjectManifest {
                id: project_id,
                name: "Graph Eval".to_string(),
                slug: pack_slug.to_string(),
                description: None,
                settings: json!({}),
            }))
            .await?;

        let overview_path = format!("library://{pack_slug}/docs/overview.md");
        let routine_path = format!("library://{pack_slug}/docs/routine.md");
        let gate_path = format!("library://{pack_slug}/docs/gate.md");
        let unrelated_path = format!("library://{pack_slug}/docs/unrelated.md");
        let manifest = json!({
            "pack_id": format!("library-knowledge-{pack_slug}"),
            "pack_version": "1",
            "schema_version": 1,
            "root_uri": format!("library://{pack_slug}/"),
            "synced_at": "2026-01-01T00:00:00Z",
            "docs": [
                {
                    "id": "overview",
                    "virtual_path": overview_path,
                    "source_path": "docs/overview.md",
                    "title": "Overview",
                    "summary": "Library overview",
                    "description": null,
                    "kind": "guide",
                    "authority": "canonical",
                    "status": "stable",
                    "tags": ["domain:library"],
                    "aliases": ["overview.md"],
                    "keywords": ["overview"],
                    "related": [
                        {
                            "type": "references",
                            "target": routine_path,
                            "description": "Overview references routine design"
                        }
                    ]
                },
                {
                    "id": "routine",
                    "virtual_path": routine_path,
                    "source_path": "docs/routine.md",
                    "title": "Routine",
                    "summary": "Routine design",
                    "description": null,
                    "kind": "guide",
                    "authority": "canonical",
                    "status": "stable",
                    "tags": ["resource:routine"],
                    "aliases": ["routine.md"],
                    "keywords": ["routine"],
                    "related": [
                        {
                            "type": "depends_on",
                            "target": gate_path,
                            "description": "Routine depends on gate design"
                        }
                    ]
                },
                {
                    "id": "gate",
                    "virtual_path": gate_path,
                    "source_path": "docs/gate.md",
                    "title": "Gate",
                    "summary": "Gate design",
                    "description": null,
                    "kind": "reference",
                    "authority": "reference",
                    "status": "stable",
                    "tags": ["resource:gate"],
                    "aliases": ["gate.md"],
                    "keywords": ["gate"],
                    "related": []
                },
                {
                    "id": "unrelated",
                    "virtual_path": unrelated_path,
                    "source_path": "docs/unrelated.md",
                    "title": "Unrelated",
                    "summary": "Unrelated document",
                    "description": null,
                    "kind": "reference",
                    "authority": "reference",
                    "status": "stable",
                    "tags": ["domain:other"],
                    "aliases": ["unrelated.md"],
                    "keywords": ["unrelated"],
                    "related": []
                }
            ]
        });
        std::fs::write(
            library_dir.join(LibraryKnowledgePack::MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        for filename in ["overview.md", "routine.md", "gate.md", "unrelated.md"] {
            std::fs::write(
                library_dir.join("docs").join(filename),
                format!("# {filename}\n"),
            )?;
        }

        let client = PlatformManifestClient::new("http://localhost:9", "test")?;
        let backend = PlatformManifestBackend::new(store, client, NoopSensitivePayloadEncoder)
            .with_workspace_dir(workspace_dir);

        Ok((backend, project_id, pack_slug.to_string(), temp))
    }

    #[tokio::test]
    async fn library_knowledge_alias_resolves_default_library_pack() {
        let (backend, _project_id, pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let backend = backend.with_current_library_slug(Some(pack_slug.clone()));

        let packs = backend.list_knowledge_packs().await.unwrap();
        let packs = packs.as_array().expect("packs array");
        assert!(packs.iter().any(|pack| pack["pack"] == "lib"));

        let value = backend
            .read_knowledge_doc_manifest(json!({
                "pack": "lib",
                "path": "routine"
            }))
            .await
            .unwrap();

        assert_eq!(value["pack"], "lib");
        assert_eq!(
            value["virtual_path"],
            format!("library://{pack_slug}/docs/routine.md")
        );
    }

    #[tokio::test]
    async fn library_knowledge_neighbors_expose_outgoing_and_incoming_edges() {
        let (backend, _project_id, pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let routine_path = format!("library://{pack_slug}/docs/routine.md");
        let overview_path = format!("library://{pack_slug}/docs/overview.md");
        let gate_path = format!("library://{pack_slug}/docs/gate.md");

        let value = backend
            .list_knowledge_neighbors(json!({
                "pack": format!("lib:{pack_slug}"),
                "path": "routine"
            }))
            .await
            .unwrap();
        let neighbors = value.as_array().expect("neighbors array");

        assert!(neighbors.iter().any(|neighbor| {
            neighbor["target"] == overview_path
                && neighbor["edges"].as_array().is_some_and(|edges| {
                    edges.iter().any(|edge| {
                        edge["edge_type"] == "references"
                            && edge["source"] == overview_path
                            && edge["target"] == routine_path
                            && edge["note"] == "Overview references routine design"
                    })
                })
        }));
        assert!(neighbors.iter().any(|neighbor| {
            neighbor["target"] == gate_path
                && neighbor["edges"].as_array().is_some_and(|edges| {
                    edges.iter().any(|edge| {
                        edge["edge_type"] == "depends_on"
                            && edge["source"] == routine_path
                            && edge["target"] == gate_path
                            && edge["note"] == "Routine depends on gate design"
                    })
                })
        }));

        let filtered = backend
            .list_knowledge_neighbors(json!({
                "pack": format!("lib:{pack_slug}"),
                "path": routine_path,
                "edge_type": "depends_on"
            }))
            .await
            .unwrap();
        let filtered = filtered.as_array().expect("filtered neighbors array");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["target"], gate_path);
        assert_eq!(filtered[0]["edges"][0]["edge_type"], "depends_on");
    }

    #[tokio::test]
    async fn local_manifest_org_id_uses_cached_bootstrap_org_id() {
        let (backend, _project_id, _pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let org_id = Uuid::new_v4();
        let backend = backend.with_cached_org_id(Some(org_id));

        assert_eq!(backend.local_manifest_org_id().await.unwrap(), org_id);
    }
}
