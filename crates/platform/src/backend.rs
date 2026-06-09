//! Platform-backed manifest backend implementations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    HasManifestSlug, ManifestResource, ManifestResourceKind, ModelManifest, ProjectManifest,
    RoutineManifest,
};
use nenjo::{ManifestReader, ManifestWriter, Slug};
use nenjo_knowledge::tools::{
    KnowledgeDocReadResult, KnowledgeNeighborArgs, KnowledgePackSummary, KnowledgeReadArgs,
    KnowledgeRegistry, KnowledgeSearchArgs, knowledge_document_metadata, knowledge_filter,
    knowledge_neighbors_result, knowledge_search_result, parse_knowledge_enum,
};
use nenjo_knowledge::{KnowledgeDocEdgeType, KnowledgePack};
use uuid::Uuid;

use crate::knowledge_contract::KnowledgeDocumentRecord;
use crate::client::{CouncilCreateApiBody, CouncilCreateMemberApiBody, PlatformManifestClient};
use crate::knowledge_backend::{
    ResolvedKnowledgePack, ensure_known_pack_selector, library_pack_selector,
    parse_library_pack_selector, unknown_pack,
};
use crate::library_knowledge::{
    LibraryKnowledgePack, LibraryKnowledgePackManifest, library_doc_relative_path,
    upsert_library_knowledge_entry, write_library_document_content,
    write_library_knowledge_manifest,
};
use crate::manifest_kinds::SensitiveContentKind;
use crate::manifest_mcp::*;
use crate::policy::ManifestAccessPolicy;
use crate::prompt_merge::merge_prompt_config;
use crate::resource_ids::{PlatformResourceIdStore, PlatformResourceKind};

fn string_to_manifest_path(path: String) -> Option<String> {
    if path.is_empty() { None } else { Some(path) }
}

fn local_agent_from_document(
    agent: AgentDocument,
    prompt_config: nenjo::agents::prompts::PromptConfig,
) -> AgentManifest {
    AgentManifest {
        name: agent.summary.name,
        slug: agent.summary.slug,
        description: agent.summary.description,
        prompt_config,
        color: agent.summary.color,
        model: agent.summary.model,
        domains: agent.domains,
        platform_scopes: agent.platform_scopes,
        mcp_servers: agent.mcp_servers,
        script_tools: agent.script_tools,
        abilities: agent.abilities,
        prompt_locked: agent.prompt_locked,
        heartbeat: agent.heartbeat,
    }
}

fn local_council_from_document(council: &CouncilDocument) -> CouncilManifest {
    CouncilManifest {
        name: council.summary.name.clone(),
        leader_agent: council.summary.leader_agent.clone(),
        members: council
            .members
            .iter()
            .map(|member| nenjo::manifest::CouncilMemberManifest {
                agent: member.agent.clone(),
                priority: member.priority,
            })
            .collect(),
        delegation_strategy: council.summary.delegation_strategy,
    }
}

fn local_routine_from_document(routine: &RoutineDocument) -> RoutineManifest {
    RoutineManifest {
        name: routine.summary.name.clone(),
        slug: routine.summary.slug.clone(),
        description: routine.summary.description.clone(),
        trigger: routine.summary.trigger,
        steps: routine
            .steps
            .iter()
            .map(|step| nenjo::manifest::RoutineStepManifest {
                slug: step.slug.clone(),
                routine: step.routine.clone(),
                name: step.name.clone(),
                step_type: step.step_type,
                council: step.council.clone(),
                agent: step.agent.clone(),
                config: step.config.clone(),
                order_index: step.order_index,
            })
            .collect(),
        edges: routine
            .edges
            .iter()
            .map(|edge| nenjo::manifest::RoutineEdgeManifest {
                routine: edge.routine.clone(),
                source_step: edge.source_step.clone(),
                target_step: edge.target_step.clone(),
                condition: edge.condition,
                metadata: edge.metadata.clone(),
            })
            .collect(),
        metadata: routine.metadata.clone(),
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
    library_dir: Option<PathBuf>,
    cached_org_id: Option<Uuid>,
    current_library_slug: Option<String>,
    resource_ids: Option<Arc<PlatformResourceIdStore>>,
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
            library_dir: None,
            cached_org_id: None,
            current_library_slug: None,
            resource_ids: None,
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

    /// Attach the local library cache root used for local-first knowledge reads.
    pub fn with_library_dir(mut self, library_dir: PathBuf) -> Self {
        self.library_dir = Some(library_dir);
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

    /// Attach the platform-private resource id sidecar used for encrypted write metadata.
    pub fn with_resource_id_store(mut self, resource_ids: Arc<PlatformResourceIdStore>) -> Self {
        self.resource_ids = Some(resource_ids);
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

    fn platform_object_id(&self, kind: PlatformResourceKind, slug: &Slug) -> Result<Uuid> {
        let store = self.resource_ids.as_ref().ok_or_else(|| {
            anyhow!(
                "missing platform resource id store for {} {}; refresh manifest",
                kind.as_str(),
                slug
            )
        })?;
        store.get(kind, slug)?.ok_or_else(|| {
            anyhow!(
                "missing platform object id for {} {}; refresh manifest",
                kind.as_str(),
                slug
            )
        })
    }

    fn record_platform_object_id(
        &self,
        kind: PlatformResourceKind,
        slug: &Slug,
        id: Uuid,
    ) -> Result<()> {
        if let Some(store) = self.resource_ids.as_ref() {
            store.upsert(kind, slug, id)?;
        }
        Ok(())
    }

    fn move_platform_object_id(
        &self,
        kind: PlatformResourceKind,
        old_slug: &Slug,
        new_slug: &Slug,
    ) -> Result<()> {
        if let Some(store) = self.resource_ids.as_ref() {
            if let Some(id) = store.get(kind, old_slug)? {
                store.upsert(kind, new_slug, id)?;
                if old_slug != new_slug {
                    store.remove(kind, old_slug)?;
                }
            }
        }
        Ok(())
    }

    fn remove_platform_object_id(&self, kind: PlatformResourceKind, slug: &Slug) -> Result<()> {
        if let Some(store) = self.resource_ids.as_ref() {
            store.remove(kind, slug)?;
        }
        Ok(())
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

    async fn replace_knowledge_doc_related(
        &self,
        pack: &Slug,
        doc: &Slug,
        related: &[KnowledgeDocRelatedDocument],
    ) -> Result<()> {
        let existing = self
            .platform_client
            .list_knowledge_doc_edges(pack, doc)
            .await?;
        for edge in existing.into_iter().filter(|edge| edge.source_doc == *doc) {
            self.platform_client
                .delete_knowledge_doc_edge(pack, doc, edge.id)
                .await?;
        }
        for edge in related {
            self.platform_client
                .create_knowledge_doc_edge(
                    pack,
                    doc,
                    &edge.target_doc,
                    &edge.edge_type,
                    edge.note.as_deref(),
                )
                .await?;
        }
        Ok(())
    }

    async fn cached_or_remote_ability(&self, ability_ref: &Slug) -> Result<AbilityManifest> {
        let local = self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .find(|ability| Slug::derive(&ability.name) == *ability_ref);
        if let Some(ability) = local {
            return Ok(ability);
        }

        let Some(remote) = self
            .platform_client
            .fetch_ability_document(ability_ref)
            .await?
        else {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                ability_ref
            ));
        };

        let hydrated = AbilityManifest {
            name: remote.summary.name,
            path: string_to_manifest_path(remote.summary.path),
            description: remote.summary.description,
            activation_condition: remote.activation_condition,
            prompt_config: Default::default(),
            platform_scopes: remote.platform_scopes,
            mcp_servers: remote.mcp_servers,
            script_tools: remote.script_tools,
            source_type: "native".to_string(),
            read_only: false,
            metadata: serde_json::json!({}),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(hydrated.clone()))
            .await?;
        Ok(hydrated)
    }

    async fn cached_agent(&self, agent: &Slug) -> Result<AgentManifest> {
        self.local_store
            .list_agents()
            .await?
            .into_iter()
            .find(|item| Slug::derive(&item.name) == *agent)
            .ok_or_else(|| anyhow!("agent not found in local manifest: {agent}"))
    }

    async fn cached_routine(&self, routine: &Slug) -> Result<RoutineManifest> {
        self.local_store
            .list_routines()
            .await?
            .into_iter()
            .find(|item| Slug::derive(&item.name) == *routine)
            .ok_or_else(|| anyhow!("routine not found in local manifest: {routine}"))
    }

    async fn cached_council(&self, council: &Slug) -> Result<CouncilManifest> {
        self.local_store
            .list_councils()
            .await?
            .into_iter()
            .find(|item| Slug::derive(&item.name) == *council)
            .ok_or_else(|| anyhow!("council not found in local manifest: {council}"))
    }

    async fn cached_model(&self, model: &Slug) -> Result<ModelManifest> {
        self.local_store
            .list_models()
            .await?
            .into_iter()
            .find(|item| Slug::derive(&item.name) == *model)
            .ok_or_else(|| anyhow!("model not found in local manifest: {model}"))
    }

    async fn cached_context_block(&self, context_block: &Slug) -> Result<ContextBlockManifest> {
        self.local_store
            .list_context_blocks()
            .await?
            .into_iter()
            .find(|item| item.slug() == *context_block)
            .ok_or_else(|| anyhow!("context block not found in local manifest: {context_block}"))
    }

    async fn cached_domain(&self, domain: &Slug) -> Result<DomainManifest> {
        self.local_store
            .list_domains()
            .await?
            .into_iter()
            .find(|item| item.slug() == *domain)
            .ok_or_else(|| anyhow!("domain not found in local manifest: {domain}"))
    }

    async fn cached_project(&self, project: &Slug) -> Result<ProjectManifest> {
        self.local_store
            .list_projects()
            .await?
            .into_iter()
            .find(|item| item.slug == *project)
            .ok_or_else(|| anyhow!("project not found in local manifest: {project}"))
    }

    fn workspace_dir(&self) -> Result<&Path> {
        self.workspace_dir
            .as_deref()
            .ok_or_else(|| anyhow!("knowledge tools require a configured workspace_dir"))
    }

    fn library_root(&self) -> Result<PathBuf> {
        if let Some(library_dir) = &self.library_dir {
            return Ok(library_dir.clone());
        }
        Ok(self.workspace_dir()?.join("library"))
    }

    async fn workspace_library_dir(&self, pack_slug: &str) -> Result<PathBuf> {
        Ok(self.library_root()?.join(pack_slug))
    }

    async fn library_knowledge_pack(&self, pack_slug: &str) -> Result<LibraryKnowledgePack> {
        let pack_dir = self.workspace_library_dir(pack_slug).await?;
        LibraryKnowledgePack::load(&pack_dir)
            .ok_or_else(|| anyhow!("knowledge pack '{pack_slug}' is not cached locally"))
    }

    fn cache_knowledge_pack(&self, pack: &KnowledgePackDocument) -> Result<()> {
        let pack_slug = pack.slug.as_str();
        let pack_dir = self.library_root()?.join(pack_slug);
        if LibraryKnowledgePack::load(&pack_dir).is_none() {
            write_library_knowledge_manifest(
                &pack_dir,
                &LibraryKnowledgePackManifest::library_pack(pack_slug),
            )?;
        }
        Ok(())
    }

    fn cache_knowledge_doc(
        &self,
        doc: &KnowledgeDocSummary,
        content: Option<&str>,
        related: &[KnowledgeDocRelatedDocument],
    ) -> Result<()> {
        let pack_slug = doc.pack.as_str();
        let pack_dir = self.library_root()?.join(pack_slug);
        let now = chrono::Utc::now();
        let record = KnowledgeDocumentRecord {
            id: Uuid::new_v4(),
            org_id: Uuid::nil(),
            pack_id: Uuid::nil(),
            pack_slug: pack_slug.to_string(),
            slug: doc.slug.as_str().to_string(),
            filename: doc.filename.clone(),
            path: doc.path.clone(),
            title: doc.title.clone(),
            kind: doc.kind.clone(),
            summary: doc.summary.clone(),
            tags: doc.tags.clone(),
            content_type: doc.content_type.clone(),
            created_at: now,
            updated_at: chrono::DateTime::parse_from_rfc3339(&doc.updated_at)
                .map(|value| value.with_timezone(&chrono::Utc))
                .unwrap_or(now),
            edges: related
                .iter()
                .map(|edge| crate::knowledge_contract::KnowledgeDocumentEdgeRecord {
                    id: Uuid::new_v4(),
                    org_id: Uuid::nil(),
                    source_item_id: Uuid::nil(),
                    source_doc: doc.slug.as_str().to_string(),
                    target_item_id: Uuid::nil(),
                    target_doc: edge.target_doc.as_str().to_string(),
                    edge_type: edge.edge_type.clone(),
                    note: edge.note.clone(),
                    created_at: now,
                    updated_at: now,
                })
                .collect(),
        };
        upsert_library_knowledge_entry(&pack_dir, pack_slug, &record)?;
        if let Some(content) = content {
            write_library_document_content(
                &pack_dir,
                &library_doc_relative_path(&record),
                content,
            )?;
        }
        Ok(())
    }

    async fn resolve_knowledge_pack(&self, selector: &str) -> Result<ResolvedKnowledgePack> {
        ensure_known_pack_selector(selector)?;
        if selector.starts_with("lib:") {
            let pack_slug = parse_library_pack_selector(selector)?;
            return self
                .library_knowledge_pack(pack_slug)
                .await
                .map(ResolvedKnowledgePack::Library);
        }
        Err(unknown_pack(selector))
    }
}

#[async_trait]
impl<L, E> KnowledgeRegistry for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>> {
        let mut packs = Vec::new();
        if let Ok(library_dir) = self.library_root() {
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

    async fn read_knowledge_doc(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: KnowledgeReadArgs =
            serde_json::from_value(params).context("invalid read_knowledge_doc args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let doc = pack.read_doc(&args.selector).ok_or_else(|| {
            anyhow!(
                "unknown knowledge doc '{}' in pack '{}'",
                args.selector,
                args.pack
            )
        })?;
        serde_json::to_value(KnowledgeDocReadResult {
            document: knowledge_document_metadata(args.pack, &doc.manifest),
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
            pack.search(&args.query, filter)
                .into_iter()
                .map(|hit| knowledge_search_result(args.pack.clone(), hit))
                .collect::<Vec<_>>(),
        )
        .map_err(Into::into)
    }

    async fn list_knowledge_neighbors(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: KnowledgeNeighborArgs =
            serde_json::from_value(params).context("invalid list_knowledge_neighbors args")?;
        let pack = self.resolve_knowledge_pack(&args.pack).await?;
        let edge_type: Option<KnowledgeDocEdgeType> = parse_knowledge_enum(args.edge_type)?;
        let neighbors = pack.neighbors(&args.selector, edge_type).ok_or_else(|| {
            anyhow!(
                "unknown knowledge doc '{}' in pack '{}'",
                args.selector,
                args.pack
            )
        })?;
        serde_json::to_value(knowledge_neighbors_result(args.pack, neighbors)).map_err(Into::into)
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
        let agent = self.cached_agent(&params.agent).await?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!(
                "agent not found in local manifest: {}",
                params.agent
            ));
        }
        Ok(AgentGetResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn get_agent_prompt(&self, params: AgentPromptGetParams) -> Result<AgentPromptGetResult> {
        let agent = self.cached_agent(&params.agent).await?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!(
                "agent not found in local manifest: {}",
                params.agent
            ));
        }
        Ok(AgentPromptGetResult {
            agent: AgentPromptDocument::from(agent),
        })
    }

    async fn create_agent(&self, params: AgentCreateParams) -> Result<AgentMutationResult> {
        let agent_id = Uuid::new_v4();
        let create = AgentCreateDocument {
            name: params.data.name,
            description: params.data.description,
            color: params.data.color,
            model: params.data.model,
        };

        let created = self
            .platform_client
            .create_agent_document(&create, Some(agent_id))
            .await?;

        let local_agent = local_agent_from_document(created.clone(), Default::default());
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;
        self.record_platform_object_id(
            PlatformResourceKind::Agent,
            &created.summary.slug,
            agent_id,
        )?;

        Ok(AgentMutationResult { agent: created })
    }

    async fn update_agent(&self, params: AgentUpdateParams) -> Result<AgentMutationResult> {
        let existing = self.cached_agent(&params.agent).await?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!(
                "agent not found in local manifest: {}",
                params.agent
            ));
        }
        let updated = self
            .platform_client
            .update_agent_document(&params.agent, &params.data)
            .await?;

        let mut local_agent =
            local_agent_from_document(updated.clone(), existing.prompt_config.clone());
        local_agent.heartbeat = existing.heartbeat.clone();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;
        self.move_platform_object_id(
            PlatformResourceKind::Agent,
            &params.agent,
            &updated.summary.slug,
        )?;

        Ok(AgentMutationResult { agent: updated })
    }

    async fn update_agent_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult> {
        let mut agent = self.cached_agent(&params.agent).await?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!(
                "agent not found in local manifest: {}",
                params.agent
            ));
        }
        if agent.prompt_locked {
            return Err(anyhow!("agent prompt is locked: {}", params.agent));
        }
        if let Some(prompt_patch) = params.prompt_config {
            agent.prompt_config = merge_prompt_config(&agent.prompt_config, prompt_patch)?;
        }
        let prompt_patch = agent.prompt_config.clone();
        let prompt_payload = serde_json::to_value(&prompt_patch)?;
        let agent_object_id =
            self.platform_object_id(PlatformResourceKind::Agent, &params.agent)?;
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                agent_object_id,
                SensitiveContentKind::AgentPrompt.encrypted_object_type(),
                &prompt_payload,
            )
            .await?;
        let prompt_config = self
            .platform_client
            .update_agent_prompt_document(&params.agent, &prompt_payload, encrypted_payload)
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
        let existing = self.cached_agent(&params.agent).await?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!(
                "agent not found in local manifest: {}",
                params.agent
            ));
        }
        self.platform_client
            .delete_agent_document(&params.agent)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Agent, &existing.manifest_slug())
            .await?;
        self.remove_platform_object_id(PlatformResourceKind::Agent, &existing.manifest_slug())?;
        Ok(DeleteResult { deleted: true })
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
        let ability = self.cached_or_remote_ability(&params.ability).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.ability
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
        let ability = self.cached_or_remote_ability(&params.ability).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.ability
            ));
        }
        Ok(AbilityPromptGetResult {
            ability: AbilityPromptDocument::from(ability),
        })
    }

    async fn create_ability(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult> {
        let ability_id = Uuid::new_v4();
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                ability_id,
                SensitiveContentKind::AbilityPrompt.encrypted_object_type(),
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_ability_document(&params.data, Some(ability_id), encrypted_payload)
            .await?;
        let local_ability = AbilityManifest {
            name: created.summary.name.clone(),
            path: string_to_manifest_path(created.summary.path.clone()),
            description: created.summary.description.clone(),
            activation_condition: created.activation_condition.clone(),
            prompt_config: params.data.prompt_config.clone(),
            platform_scopes: created.platform_scopes.clone(),
            mcp_servers: created.mcp_servers.clone(),
            script_tools: created.script_tools.clone(),
            source_type: "native".to_string(),
            read_only: false,
            metadata: serde_json::json!({}),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        let created_slug = Slug::derive(&created.summary.name);
        self.record_platform_object_id(PlatformResourceKind::Ability, &created_slug, ability_id)?;
        Ok(AbilityMutationResult { ability: created })
    }

    async fn update_ability(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "ability update requires at least one field in data"
            ));
        }
        let existing = self.cached_or_remote_ability(&params.ability).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.ability
            ));
        }
        let updated = self
            .platform_client
            .update_ability_document(&params.ability, &params.data)
            .await?;
        let local_ability = AbilityManifest {
            name: updated.summary.name.clone(),
            path: string_to_manifest_path(updated.summary.path.clone()),
            description: updated.summary.description.clone(),
            activation_condition: updated.activation_condition.clone(),
            prompt_config: existing.prompt_config.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            mcp_servers: updated.mcp_servers.clone(),
            script_tools: updated.script_tools.clone(),
            source_type: existing.source_type.clone(),
            read_only: existing.read_only,
            metadata: existing.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        let updated_slug = Slug::derive(&updated.summary.name);
        self.move_platform_object_id(
            PlatformResourceKind::Ability,
            &params.ability,
            &updated_slug,
        )?;
        Ok(AbilityMutationResult { ability: updated })
    }

    async fn update_ability_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult> {
        let existing = self.cached_or_remote_ability(&params.ability).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.ability
            ));
        }
        let prompt_config = params.prompt_config;
        let ability_object_id =
            self.platform_object_id(PlatformResourceKind::Ability, &params.ability)?;
        let updated = self
            .platform_client
            .update_ability_prompt_document(
                &params.ability,
                &prompt_config,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        ability_object_id,
                        SensitiveContentKind::AbilityPrompt.encrypted_object_type(),
                        &serde_json::json!(prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_ability = AbilityManifest {
            name: existing.name,
            path: existing.path,
            description: existing.description,
            activation_condition: existing.activation_condition,
            prompt_config: updated.prompt_config.clone(),
            platform_scopes: existing.platform_scopes,
            mcp_servers: existing.mcp_servers,
            script_tools: existing.script_tools,
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
        let existing = self.cached_or_remote_ability(&params.ability).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.ability
            ));
        }
        self.platform_client
            .delete_ability_document(&params.ability)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Ability, &existing.manifest_slug())
            .await?;
        self.remove_platform_object_id(PlatformResourceKind::Ability, &existing.manifest_slug())?;
        Ok(DeleteResult { deleted: true })
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
        let domain = self.cached_domain(&params.domain).await?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                params.domain
            ));
        }
        Ok(DomainGetResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn get_domain_prompt(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult> {
        let domain = self.cached_domain(&params.domain).await?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                params.domain
            ));
        }
        Ok(DomainManifestGetResult {
            domain: DomainManifestDocument::from(domain),
        })
    }

    async fn create_domain(&self, params: DomainCreateParams) -> Result<DomainMutationResult> {
        let domain_id = Uuid::new_v4();
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                domain_id,
                SensitiveContentKind::DomainPrompt.encrypted_object_type(),
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_domain_document(&params.data, Some(domain_id), encrypted_payload)
            .await?;
        let local_domain = DomainManifest {
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            description: created.summary.description.clone(),
            command: created.command.clone(),
            platform_scopes: created.platform_scopes.clone(),
            abilities: params.data.abilities.clone().unwrap_or_default(),
            mcp_servers: params.data.mcp_servers.clone().unwrap_or_default(),
            script_tools: params.data.script_tools.clone().unwrap_or_default(),
            prompt_config: params.data.prompt_config.clone().unwrap_or_default(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        self.record_platform_object_id(
            PlatformResourceKind::Domain,
            &created.summary.slug,
            domain_id,
        )?;
        Ok(DomainMutationResult { domain: created })
    }

    async fn update_domain(&self, params: DomainUpdateParams) -> Result<DomainMutationResult> {
        let existing = self.cached_domain(&params.domain).await?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                params.domain
            ));
        }
        if params.data.is_empty() {
            return Err(anyhow!("domain update requires at least one field"));
        }
        let updated = self
            .platform_client
            .update_domain_document(&params.domain, &params.data)
            .await?;
        let local_domain = DomainManifest {
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            description: updated.summary.description.clone(),
            command: updated.command.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            abilities: updated.abilities.clone(),
            mcp_servers: updated.mcp_servers.clone(),
            script_tools: updated.script_tools.clone(),
            prompt_config: existing.prompt_config.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        self.move_platform_object_id(
            PlatformResourceKind::Domain,
            &params.domain,
            &updated.summary.slug,
        )?;
        Ok(DomainMutationResult { domain: updated })
    }

    async fn update_domain_prompt(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult> {
        let existing = self.cached_domain(&params.domain).await?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                params.domain
            ));
        }
        if let Some(policy) = &self.access_policy
            && !policy.validate_domain_scopes(&existing.platform_scopes)
        {
            return Err(anyhow!("requested domain scopes exceed caller permissions"));
        }
        let domain_object_id =
            self.platform_object_id(PlatformResourceKind::Domain, &params.domain)?;
        let updated = self
            .platform_client
            .update_domain_manifest_document(
                &params.domain,
                params.prompt_config.clone(),
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        domain_object_id,
                        SensitiveContentKind::DomainPrompt.encrypted_object_type(),
                        &serde_json::json!(params.prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_domain = DomainManifest {
            name: existing.name.clone(),
            path: existing.path.clone(),
            description: existing.description.clone(),
            command: existing.command.clone(),
            platform_scopes: existing.platform_scopes.clone(),
            abilities: existing.abilities.clone(),
            mcp_servers: existing.mcp_servers.clone(),
            script_tools: existing.script_tools.clone(),
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
        let existing = self.cached_domain(&params.domain).await?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                params.domain
            ));
        }
        self.platform_client
            .delete_domain_document(&params.domain)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Domain, &existing.manifest_slug())
            .await?;
        self.remove_platform_object_id(PlatformResourceKind::Domain, &existing.manifest_slug())?;
        Ok(DeleteResult { deleted: true })
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
        let project = self.cached_project(&params.project).await?;
        Ok(ProjectGetResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn create_project(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult> {
        let project_id = Uuid::new_v4();
        let created = self
            .platform_client
            .create_project_document(&params.data, Some(project_id))
            .await?;
        let local_project = ProjectManifest {
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
        let existing = self.cached_project(&params.project).await?;
        let merged = ProjectUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            slug: params.data.slug.or_else(|| Some(existing.slug.clone())),
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
            .update_project_document(&params.project, &merged)
            .await?;
        let local_project = ProjectManifest {
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
        let existing = self.cached_project(&params.project).await?;
        self.platform_client
            .delete_project_document(&params.project)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Project, &existing.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
    }
}

#[async_trait]
impl<L, E> LibraryManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn create_knowledge_pack(
        &self,
        params: KnowledgePackCreateParams,
    ) -> Result<KnowledgePackMutationResult> {
        let knowledge_pack = self
            .platform_client
            .create_knowledge_pack(&params.data)
            .await?;
        if let Err(error) = self.cache_knowledge_pack(&knowledge_pack) {
            tracing::warn!(
                pack = %knowledge_pack.slug,
                error = %error,
                "Failed to cache created knowledge pack locally"
            );
        }
        Ok(KnowledgePackMutationResult { knowledge_pack })
    }

    async fn update_knowledge_pack(
        &self,
        params: KnowledgePackUpdateParams,
    ) -> Result<KnowledgePackMutationResult> {
        let knowledge_pack = self
            .platform_client
            .update_knowledge_pack(&params.pack, &params.data)
            .await?;
        if let Err(error) = self.cache_knowledge_pack(&knowledge_pack) {
            tracing::warn!(
                pack = %knowledge_pack.slug,
                error = %error,
                "Failed to cache updated knowledge pack locally"
            );
        }
        Ok(KnowledgePackMutationResult { knowledge_pack })
    }

    async fn create_knowledge_doc(
        &self,
        params: KnowledgeDocCreateParams,
    ) -> Result<KnowledgeDocMutationResult> {
        let data = params.data;
        let doc_id = Uuid::new_v4();
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                doc_id,
                SensitiveContentKind::DocumentContent.encrypted_object_type(),
                &serde_json::Value::String(data.content.clone()),
            )
            .await?;
        let knowledge_doc = self
            .platform_client
            .create_knowledge_doc(&data.pack, doc_id, &data, encrypted_payload)
            .await?;
        self.replace_knowledge_doc_related(&data.pack, &knowledge_doc.slug, &data.related)
            .await?;
        if let Err(error) =
            self.cache_knowledge_doc(&knowledge_doc, Some(&data.content), &data.related)
        {
            tracing::warn!(
                pack = %data.pack,
                slug = %knowledge_doc.slug,
                error = %error,
                "Failed to cache created knowledge document locally"
            );
        }
        Ok(KnowledgeDocMutationResult { knowledge_doc })
    }

    async fn update_knowledge_doc(
        &self,
        params: KnowledgeDocUpdateParams,
    ) -> Result<KnowledgeDocMutationResult> {
        let doc_id = self
            .platform_client
            .resolve_knowledge_doc_slug(&params.pack, &params.slug)
            .await?;
        let mut knowledge_doc = if let Some(content) = params.data.content.as_deref() {
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    doc_id,
                    SensitiveContentKind::DocumentContent.encrypted_object_type(),
                    &serde_json::Value::String(content.to_string()),
                )
                .await?;
            self.platform_client
                .update_knowledge_doc_content(
                    &params.pack,
                    &params.slug,
                    content,
                    encrypted_payload,
                )
                .await?
        } else {
            self.platform_client
                .update_knowledge_doc_metadata(&params.pack, &params.slug, &params.data)
                .await?
        };

        if params.data.content.is_some()
            && (params.data.filename.is_some()
                || params.data.path.is_some()
                || params.data.title.is_some()
                || params.data.kind.is_some()
                || params.data.summary.is_some()
                || params.data.tags.is_some())
        {
            knowledge_doc = self
                .platform_client
                .update_knowledge_doc_metadata(&params.pack, &params.slug, &params.data)
                .await?;
        }

        if let Some(related) = params.data.related.as_deref() {
            self.replace_knowledge_doc_related(&params.pack, &params.slug, related)
                .await?;
        }

        if let Some(related) = params.data.related.as_deref() {
            if let Err(error) =
                self.cache_knowledge_doc(&knowledge_doc, params.data.content.as_deref(), related)
            {
                tracing::warn!(
                    pack = %params.pack,
                    slug = %knowledge_doc.slug,
                    error = %error,
                    "Failed to cache updated knowledge document locally"
                );
            }
        }

        Ok(KnowledgeDocMutationResult { knowledge_doc })
    }

    async fn delete_knowledge_doc(&self, params: KnowledgeDocDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_knowledge_doc(&params.pack, &params.slug)
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let routine = self.cached_routine(&params.slug).await?;
        Ok(RoutineGetResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn create_routine(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult> {
        let routine_id = Uuid::new_v4();
        let created = self
            .platform_client
            .create_routine_document(&params.data, Some(routine_id))
            .await?;
        let local_routine = local_routine_from_document(&created);
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
        let _ = self.cached_routine(&params.slug).await?;
        let updated = self
            .platform_client
            .update_routine_document(&params.slug, &params.data)
            .await?;
        let local_routine = local_routine_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        Ok(RoutineMutationResult { routine: updated })
    }

    async fn delete_routine(&self, params: RoutineDeleteParams) -> Result<DeleteResult> {
        let existing = self.cached_routine(&params.slug).await?;
        self.platform_client
            .delete_routine_document(&params.slug)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Routine, &existing.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let model = self.cached_model(&params.model).await?;
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
            name: created.summary.name.clone(),
            slug: nenjo::manifest::model_manifest_slug(
                &created.summary.model_provider,
                &created.summary.model,
            ),
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
        let existing = self.cached_model(&params.model).await?;
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
            .update_model_document(&params.model, &merged)
            .await?;
        let local_model = ModelManifest {
            name: updated.summary.name.clone(),
            slug: nenjo::manifest::model_manifest_slug(
                &updated.summary.model_provider,
                &updated.summary.model,
            ),
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
        let existing = self.cached_model(&params.model).await?;
        self.platform_client
            .delete_model_document(&params.model)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Model, &existing.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let council = self.cached_council(&params.council).await?;
        Ok(CouncilGetResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn create_council(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult> {
        let leader_agent = params.data.leader_agent.clone();
        let mut members = Vec::with_capacity(params.data.members.len());
        for member in &params.data.members {
            members.push(CouncilCreateMemberApiBody {
                agent: member.agent.clone(),
                priority: member.priority,
                config: member.config.clone(),
            });
        }
        let body = CouncilCreateApiBody {
            name: params.data.name,
            description: params.data.description,
            leader_agent: params.data.leader_agent,
            delegation_strategy: params.data.delegation_strategy,
            config: params.data.config,
            members,
        };
        let mut created = self.platform_client.create_council_document(&body).await?;
        created.summary.leader_agent = leader_agent;
        let local_council = local_council_from_document(&created);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: created })
    }

    async fn update_council(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult> {
        let existing = self.cached_council(&params.council).await?;
        let merged = CouncilUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: params.data.description,
            delegation_strategy: params
                .data
                .delegation_strategy
                .or(Some(existing.delegation_strategy)),
            config: params.data.config,
        };
        let mut updated = self
            .platform_client
            .update_council_document(&params.council, &merged)
            .await?;
        updated.summary.leader_agent = existing.leader_agent;
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
        let existing = self.cached_council(&params.council).await?;
        let member = CouncilCreateMemberApiBody {
            agent: params.data.agent,
            priority: params.data.priority,
            config: params.data.config,
        };
        let mut updated = self
            .platform_client
            .add_council_member_document(&params.council, &member)
            .await?;
        updated.summary.leader_agent = existing.leader_agent;
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
        let existing = self.cached_council(&params.council).await?;
        let mut updated = self
            .platform_client
            .update_council_member_document(&params.council, &params.agent, &params.data)
            .await?;
        updated.summary.leader_agent = existing.leader_agent;
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
        let existing = self.cached_council(&params.council).await?;
        let mut updated = self
            .platform_client
            .remove_council_member_document(&params.council, &params.agent)
            .await?;
        updated.summary.leader_agent = existing.leader_agent;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn delete_council(&self, params: CouncilDeleteParams) -> Result<DeleteResult> {
        let existing = self.cached_council(&params.council).await?;
        self.platform_client
            .delete_council_document(&params.council)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Council, &existing.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let context_block = self.cached_context_block(&params.context_block).await?;
        Ok(ContextBlockGetResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn get_context_block_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult> {
        let context_block = self.cached_context_block(&params.context_block).await?;
        Ok(ContextBlockContentGetResult {
            context_block: ContextBlockContentDocument::from(context_block),
        })
    }

    async fn create_context_block(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult> {
        let context_block_id = Uuid::new_v4();
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                context_block_id,
                SensitiveContentKind::ContextBlockContent.encrypted_object_type(),
                &serde_json::json!(params.data.template.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_context_block_document(&params.data, Some(context_block_id), encrypted_payload)
            .await?;
        let local_context_block = ContextBlockManifest {
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            description: created.summary.description.clone(),
            template: params.data.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        let created_slug =
            nenjo::manifest::context_block_slug(&created.summary.path, &created.summary.name);
        self.record_platform_object_id(
            PlatformResourceKind::ContextBlock,
            &created_slug,
            context_block_id,
        )?;
        Ok(ContextBlockMutationResult {
            context_block: created,
        })
    }

    async fn update_context_block(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult> {
        let existing = self.cached_context_block(&params.context_block).await?;
        let updated = self
            .platform_client
            .update_context_block_document(&params.context_block, &params.data)
            .await?;
        let local_context_block = ContextBlockManifest {
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            description: updated.summary.description.clone(),
            template: existing.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        let updated_slug =
            nenjo::manifest::context_block_slug(&updated.summary.path, &updated.summary.name);
        self.move_platform_object_id(
            PlatformResourceKind::ContextBlock,
            &params.context_block,
            &updated_slug,
        )?;
        Ok(ContextBlockMutationResult {
            context_block: updated,
        })
    }

    async fn update_context_block_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult> {
        let existing = self.cached_context_block(&params.context_block).await?;
        let template = params.template.unwrap_or_else(|| existing.template.clone());
        let context_block_object_id =
            self.platform_object_id(PlatformResourceKind::ContextBlock, &params.context_block)?;
        let updated = self
            .platform_client
            .update_context_block_content_document(
                &params.context_block,
                &template,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_org_id().await?,
                        context_block_object_id,
                        SensitiveContentKind::ContextBlockContent.encrypted_object_type(),
                        &serde_json::json!(template.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_context_block = ContextBlockManifest {
            name: existing.name.clone(),
            path: existing.path.clone(),
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
        let existing = self.cached_context_block(&params.context_block).await?;
        self.platform_client
            .delete_context_block_document(&params.context_block)
            .await?;
        self.local_store
            .delete_resource(
                ManifestResourceKind::ContextBlock,
                &existing.manifest_slug(),
            )
            .await?;
        self.remove_platform_object_id(
            PlatformResourceKind::ContextBlock,
            &existing.manifest_slug(),
        )?;
        Ok(DeleteResult { deleted: true })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::{TempDir, tempdir};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use nenjo::manifest::ProjectManifest;
    use nenjo::manifest::local::LocalManifestStore;
    use nenjo::{ManifestResource, ManifestWriter};

    use super::*;

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
    }

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
        let library_dir = workspace_dir.join("library").join(pack_slug);
        std::fs::create_dir_all(library_dir.join("docs"))?;

        let store = Arc::new(LocalManifestStore::new(manifests_dir));
        store
            .upsert_resource(&ManifestResource::Project(ProjectManifest {
                name: "Graph Eval".to_string(),
                slug: Slug::derive(pack_slug),
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
            "version": "1",
            "schema_version": 1,
            "root_uri": format!("library://{pack_slug}/"),
            "synced_at": "2026-01-01T00:00:00Z",
            "docs": [
                {
                    "id": "overview",
                    "selector": overview_path,
                    "source_path": "docs/overview.md",
                    "title": "Overview",
                    "summary": "Library overview",
                    "kind": "guide",
                    "tags": ["domain:library"],
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
                    "selector": routine_path,
                    "source_path": "docs/routine.md",
                    "title": "Routine",
                    "summary": "Routine design",
                    "kind": "guide",
                    "tags": ["resource:routine"],
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
                    "selector": gate_path,
                    "source_path": "docs/gate.md",
                    "title": "Gate",
                    "summary": "Gate design",
                    "kind": "reference",
                    "tags": ["resource:gate"],
                    "related": []
                },
                {
                    "id": "unrelated",
                    "selector": unrelated_path,
                    "source_path": "docs/unrelated.md",
                    "title": "Unrelated",
                    "summary": "Unrelated document",
                    "kind": "reference",
                    "tags": ["domain:other"],
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

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<RecordedRequest> {
        let mut buffer = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                anyhow::bail!("connection closed before request headers completed");
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };

        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let request_line = headers
            .lines()
            .next()
            .ok_or_else(|| anyhow!("missing request line"))?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);

        let body_start = header_end;
        while buffer.len() < body_start + content_length {
            let mut chunk = vec![0_u8; body_start + content_length - buffer.len()];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }

        Ok(RecordedRequest {
            method,
            path,
            body: String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string(),
        })
    }

    fn response(status: &str, body: serde_json::Value) -> String {
        let body = body.to_string();
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn spawn_knowledge_update_server(
        pack_id: Uuid,
        doc_id: Uuid,
        target_doc_id: Uuid,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/api/v1/knowledge") => response(
                        "200 OK",
                        json!([{
                            "id": pack_id,
                            "slug": "test-pack",
                            "name": "Test Pack",
                            "description": null,
                            "selector": null,
                            "version": null
                        }]),
                    ),
                    ("GET", "/api/v1/knowledge/test-pack/items") => response(
                        "200 OK",
                        json!([
                            {
                                "id": doc_id,
                                "pack_id": pack_id,
                                "slug": "guide",
                                "filename": "guide.md",
                                "path": "docs/guide.md",
                                "title": "Guide",
                                "kind": "guide",
                                "summary": "Guide",
                                "tags": ["core"],
                                "content_type": "text/plain",
                                "updated_at": "2026-05-23T00:00:00Z"
                            },
                            {
                                "id": target_doc_id,
                                "pack_id": pack_id,
                                "slug": "target",
                                "filename": "target.md",
                                "path": "docs/target.md",
                                "title": "Target",
                                "kind": "guide",
                                "summary": "Target",
                                "tags": [],
                                "content_type": "text/plain",
                                "updated_at": "2026-05-23T00:00:00Z"
                            }
                        ]),
                    ),
                    ("PUT", "/api/v1/knowledge/test-pack/items/guide/content") => response(
                        "200 OK",
                        json!({
                            "id": doc_id,
                            "pack_id": pack_id,
                            "slug": "guide",
                            "filename": "guide.md",
                            "path": "docs/guide.md",
                            "title": "Guide",
                            "kind": "guide",
                            "summary": "Updated guide",
                            "tags": ["core"],
                            "content_type": "text/plain",
                            "updated_at": "2026-05-23T00:00:00Z"
                        }),
                    ),
                    ("PATCH", "/api/v1/knowledge/test-pack/items/guide") => response(
                        "200 OK",
                        json!({
                            "id": doc_id,
                            "pack_id": pack_id,
                            "slug": "guide",
                            "filename": "guide.md",
                            "path": "docs/guide.md",
                            "title": "Guide",
                            "kind": "guide",
                            "summary": "Updated guide",
                            "tags": ["core"],
                            "content_type": "text/markdown",
                            "updated_at": "2026-05-23T00:01:00Z"
                        }),
                    ),
                    ("GET", "/api/v1/knowledge/test-pack/items/guide/edges") => {
                        response("200 OK", json!([]))
                    }
                    ("POST", "/api/v1/knowledge/test-pack/items/guide/edges") => response(
                        "201 Created",
                        json!({
                            "id": Uuid::new_v4(),
                            "org_id": Uuid::new_v4(),
                            "source_item_id": doc_id,
                            "source_doc": "guide",
                            "target_item_id": target_doc_id,
                            "target_doc": "target",
                            "edge_type": "references",
                            "note": "see target",
                            "created_at": "2026-05-23T00:02:00Z",
                            "updated_at": "2026-05-23T00:02:00Z"
                        }),
                    ),
                    _ => response("404 Not Found", json!({ "error": "not found" })),
                };
                stream.write_all(body.as_bytes()).await?;
                requests.push(request);
            }
            Ok(requests)
        });
        Ok((base_url, handle))
    }

    async fn spawn_knowledge_create_server(
        pack_id: Uuid,
        doc_id: Uuid,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/api/v1/knowledge/test-pack/items") => response(
                        "201 Created",
                        json!({
                            "id": doc_id,
                            "pack_id": pack_id,
                            "slug": "ownership-lifetimes-a1b2c3d4",
                            "filename": "ownership-lifetimes.md",
                            "path": "rust/ownership",
                            "title": "Ownership & Lifetimes",
                            "kind": "guide",
                            "summary": "Ownership and lifetime guidance",
                            "tags": ["rust", "ownership"],
                            "content_type": "text/markdown",
                            "updated_at": "2026-05-23T00:00:00Z"
                        }),
                    ),
                    (
                        "GET",
                        "/api/v1/knowledge/test-pack/items/ownership-lifetimes-a1b2c3d4/edges",
                    ) => response("200 OK", json!([])),
                    _ => response("404 Not Found", json!({ "error": "not found" })),
                };
                stream.write_all(body.as_bytes()).await?;
                requests.push(request);
            }
            Ok(requests)
        });
        Ok((base_url, handle))
    }

    #[tokio::test]
    async fn library_knowledge_uses_slug_selector() {
        let (backend, _project_id, pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let backend = backend.with_current_library_slug(Some(pack_slug.clone()));

        let packs = backend.list_knowledge_packs().await.unwrap();
        let packs = packs.as_array().expect("packs array");
        assert!(
            packs
                .iter()
                .any(|pack| pack["pack"] == format!("lib:{pack_slug}"))
        );

        let value = backend
            .read_knowledge_doc(json!({
                "pack": format!("lib:{pack_slug}"),
                "selector": "routine"
            }))
            .await
            .unwrap();

        assert_eq!(value["content"], "# routine.md\n");
        assert_eq!(
            value["document"]["selector"],
            format!("library://{pack_slug}/docs/routine.md")
        );
    }

    #[tokio::test]
    async fn library_knowledge_neighbors_expose_outgoing_edges_with_metadata() {
        let (backend, _project_id, pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let routine_path = format!("library://{pack_slug}/docs/routine.md");
        let gate_path = format!("library://{pack_slug}/docs/gate.md");

        let value = backend
            .list_knowledge_neighbors(json!({
                "pack": format!("lib:{pack_slug}"),
                "selector": "routine"
            }))
            .await
            .unwrap();
        assert_eq!(value["document"]["selector"], routine_path);
        let edges = value["edges"].as_array().expect("edges array");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["type"], "depends_on");
        assert_eq!(edges[0]["target"]["selector"], gate_path);
        assert_eq!(edges[0]["target"]["title"], "Gate");
        assert!(edges[0].get("note").is_none());

        let filtered = backend
            .list_knowledge_neighbors(json!({
                "pack": format!("lib:{pack_slug}"),
                "selector": routine_path,
                "edge_type": "depends_on"
            }))
            .await
            .unwrap();
        let filtered = filtered["edges"].as_array().expect("filtered edges array");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["target"]["selector"], gate_path);
        assert_eq!(filtered[0]["type"], "depends_on");
    }

    #[tokio::test]
    async fn create_knowledge_doc_without_doc_returns_generated_slug() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let pack_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let (base_url, server) = spawn_knowledge_create_server(pack_id, doc_id)
            .await
            .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, NoopSensitivePayloadEncoder)
            .with_cached_org_id(Some(Uuid::new_v4()));

        let result = backend
            .create_knowledge_doc(KnowledgeDocCreateParams {
                data: KnowledgeDocCreateDocument {
                    pack: Slug::parse("test-pack").unwrap(),
                    filename: "ownership-lifetimes.md".into(),
                    content: "# Ownership & Lifetimes".into(),
                    content_type: Some("text/markdown".into()),
                    path: Some("rust/ownership".into()),
                    title: Some("Ownership & Lifetimes".into()),
                    kind: Some("guide".into()),
                    summary: Some("Ownership and lifetime guidance".into()),
                    tags: vec!["rust".into(), "ownership".into()],
                    related: Vec::new(),
                },
            })
            .await
            .unwrap();

        assert_eq!(result.knowledge_doc.pack.as_str(), "test-pack");
        assert_eq!(
            result.knowledge_doc.slug.as_str(),
            "ownership-lifetimes-a1b2c3d4"
        );
        assert_eq!(
            result.knowledge_doc.title.as_deref(),
            Some("Ownership & Lifetimes")
        );

        let requests = server.await.unwrap().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| (request.method.as_str(), request.path.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("POST", "/api/v1/knowledge/test-pack/items".to_string()),
                (
                    "GET",
                    "/api/v1/knowledge/test-pack/items/ownership-lifetimes-a1b2c3d4/edges"
                        .to_string()
                )
            ]
        );
        assert!(!requests[0].body.contains("name=\"slug\""));
    }

    #[tokio::test]
    async fn update_knowledge_doc_with_content_metadata_and_related_calls_all_slug_paths() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let pack_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let target_doc_id = Uuid::new_v4();
        let (base_url, server) = spawn_knowledge_update_server(pack_id, doc_id, target_doc_id)
            .await
            .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, NoopSensitivePayloadEncoder)
            .with_cached_org_id(Some(Uuid::new_v4()));

        let result = backend
            .update_knowledge_doc(KnowledgeDocUpdateParams {
                pack: Slug::parse("test-pack").unwrap(),
                slug: Slug::parse("guide").unwrap(),
                data: KnowledgeDocUpdateDocument {
                    filename: Some("guide.md".into()),
                    content: Some("updated content".into()),
                    path: Some(Some("docs/guide.md".into())),
                    title: Some(Some("Guide".into())),
                    kind: Some(Some("guide".into())),
                    summary: Some(Some("Updated guide".into())),
                    tags: Some(vec!["core".into()]),
                    related: Some(vec![KnowledgeDocRelatedDocument {
                        target_doc: Slug::parse("target").unwrap(),
                        edge_type: "references".into(),
                        note: Some("see target".into()),
                    }]),
                },
            })
            .await
            .unwrap();

        assert_eq!(result.knowledge_doc.pack.as_str(), "test-pack");
        assert_eq!(result.knowledge_doc.slug.as_str(), "guide");
        assert_eq!(result.knowledge_doc.path.as_deref(), Some("docs/guide.md"));
        assert_eq!(result.knowledge_doc.title.as_deref(), Some("Guide"));
        assert_eq!(result.knowledge_doc.tags, vec!["core"]);

        let requests = server.await.unwrap().unwrap();
        let expected_paths = [
            "/api/v1/knowledge/test-pack/items".to_string(),
            "/api/v1/knowledge/test-pack/items/guide/content".to_string(),
            "/api/v1/knowledge/test-pack/items/guide".to_string(),
            "/api/v1/knowledge/test-pack/items/guide/edges".to_string(),
            "/api/v1/knowledge/test-pack/items/guide/edges".to_string(),
        ];
        assert_eq!(
            requests
                .iter()
                .map(|request| (request.method.as_str(), request.path.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("GET", expected_paths[0].clone()),
                ("PUT", expected_paths[1].clone()),
                ("PATCH", expected_paths[2].clone()),
                ("GET", expected_paths[3].clone()),
                ("POST", expected_paths[4].clone())
            ]
        );
        assert!(requests[1].body.contains("updated content"));
        assert!(requests[2].body.contains("\"filename\":\"guide.md\""));
        assert!(!requests[2].body.contains("updated content"));
        assert!(!requests[2].body.contains("related"));
        assert!(requests[4].body.contains("target"));
        assert!(requests[4].body.contains("references"));
    }

    #[tokio::test]
    async fn local_manifest_org_id_uses_cached_bootstrap_org_id() {
        let (backend, _project_id, _pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let org_id = Uuid::new_v4();
        let backend = backend.with_cached_org_id(Some(org_id));

        assert_eq!(backend.local_manifest_org_id().await.unwrap(), org_id);
    }
}
