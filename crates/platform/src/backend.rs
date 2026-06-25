//! Platform-backed manifest backend implementations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, CommandManifest, ContextBlockManifest, CouncilManifest,
    DomainManifest, HasManifestSlug, Manifest, ManifestResource, ManifestResourceKind,
    ModelManifest, ProjectManifest, RoutineCronTaskManifest, RoutineManifest, RoutineTrigger,
    context_block_slug, domain_slug,
};
use nenjo::{ManifestReader, ManifestWriter, Slug};
use uuid::Uuid;

use crate::client::{
    AgentHeartbeatConfigureApiBody, CouncilCreateApiBody, CouncilCreateMemberApiBody,
    KnowledgeDocEdgeReplaceItem, KnowledgeDocEdgeResponse, PlatformManifestClient,
};
use crate::library_knowledge::{
    LibraryKnowledgePackCacheEntry, ReplaceDocumentEdges, ensure_library_knowledge_pack_cache,
    library_knowledge_doc_relative_path, remove_library_knowledge_entry,
    upsert_library_knowledge_entry_with_edges, write_library_document_content,
};
use crate::manifest_contract::{
    AgentRecord, DomainPromptRecord, KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord,
};
use crate::manifest_kinds::{ContentScope, SensitiveContentKind};
use crate::manifest_mcp::*;
use crate::policy::ManifestAccessPolicy;
use crate::prompt_merge::merge_prompt_config;
use crate::resource_ids::{PlatformResourceIdStore, PlatformResourceKind};

fn string_to_manifest_path(path: String) -> Option<String> {
    if path.is_empty() { None } else { Some(path) }
}

fn knowledge_edge_record_from_response(
    edge: KnowledgeDocEdgeResponse,
) -> KnowledgeDocumentEdgeRecord {
    edge
}

fn local_agent_from_record(agent: AgentRecord) -> AgentManifest {
    agent.to_manifest(agent.resolved_prompt_config())
}

fn local_ability_from_document(ability: AbilityDocument) -> AbilityManifest {
    AbilityManifest {
        name: ability.summary.name,
        path: string_to_manifest_path(ability.summary.path),
        description: ability.summary.description,
        activation_condition: ability.activation_condition,
        prompt_config: ability.prompt_config,
        platform_scopes: ability.platform_scopes,
        mcp_servers: ability.mcp_servers,
        script_tools: ability.script_tools,
        media: Vec::new(),
        source_type: "native".to_string(),
        read_only: false,
        metadata: serde_json::json!({}),
    }
}

fn local_domain_from_document(domain: DomainDocument) -> DomainManifest {
    DomainManifest {
        name: domain.summary.name,
        path: domain.summary.path,
        description: domain.summary.description,
        command: domain.command,
        platform_scopes: domain.platform_scopes,
        abilities: domain.abilities,
        mcp_servers: domain.mcp_servers,
        script_tools: domain.script_tools,
        media: Vec::new(),
        prompt_config: domain.prompt_config,
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

fn routine_encrypted_payload_object_id(payload: Option<&serde_json::Value>) -> Option<Uuid> {
    payload?
        .get("object_id")?
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn command_matches_ref(command: &CommandManifest, command_ref: &str) -> bool {
    command.name == command_ref
        || command.command == command_ref
        || command.command.trim_start_matches('/') == command_ref
}

fn ability_matches_ref(ability: &AbilityManifest, ability_ref: &Slug) -> bool {
    ability.manifest_slug() == *ability_ref || Slug::derive(&ability.name) == *ability_ref
}

fn configure_name_slug(name: Option<&String>) -> Option<Slug> {
    name.map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(Slug::derive)
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

    /// Encode a payload with an explicit account-content scope.
    async fn encode_payload_with_scope(
        &self,
        scope: ContentScope,
        account_id: Uuid,
        object_id: Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let _ = scope;
        self.encode_payload(account_id, object_id, object_type, payload)
            .await
    }

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
    read_only_manifest: Option<Arc<Manifest>>,
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
            read_only_manifest: None,
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

    /// Attach read-only runtime resources, such as package-installed manifests,
    /// that should be queryable but never used as the mutable write source.
    pub fn with_read_only_manifest(mut self, manifest: Arc<Manifest>) -> Self {
        self.read_only_manifest = Some(manifest);
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

    fn read_only_ability(&self, ability_ref: &Slug) -> Option<AbilityManifest> {
        self.read_only_manifest
            .as_ref()?
            .abilities
            .iter()
            .find(|ability| ability_matches_ref(ability, ability_ref))
            .cloned()
    }

    fn allow_domain(&self, domain: &DomainManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_domain(domain))
            .unwrap_or(true)
    }

    async fn decode_domain_prompt_record(
        &self,
        mut record: DomainPromptRecord,
    ) -> Result<DomainPromptRecord> {
        let Some(payload) = record.encrypted_payload.as_ref() else {
            return Ok(record);
        };
        if payload.object_type != SensitiveContentKind::DomainPrompt.encrypted_object_type() {
            bail!(
                "domain prompt record carried unsupported encrypted payload type {}",
                payload.object_type
            );
        }

        let Some(decoded) = self
            .sensitive_payload_encoder
            .decode_payload(&serde_json::to_value(payload)?)
            .await?
        else {
            return Ok(record);
        };

        record.prompt_config = Some(
            serde_json::from_value(decoded)
                .context("failed to decode encrypted domain prompt payload")?,
        );
        record.encrypted_payload = None;
        Ok(record)
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

    fn optional_platform_object_id(
        &self,
        kind: PlatformResourceKind,
        slug: &Slug,
    ) -> Result<Option<Uuid>> {
        let Some(store) = self.resource_ids.as_ref() else {
            return Ok(None);
        };
        store.get(kind, slug)
    }

    fn ensure_platform_object_id(&self, kind: PlatformResourceKind, slug: &Slug) -> Result<Uuid> {
        if let Some(id) = self.optional_platform_object_id(kind, slug)? {
            return Ok(id);
        }
        let id = Uuid::new_v4();
        self.record_platform_object_id(kind, slug, id)?;
        Ok(id)
    }

    async fn ensure_routine_object_id(&self, routine: &Slug) -> Result<Uuid> {
        if let Some(id) =
            self.optional_platform_object_id(PlatformResourceKind::Routine, routine)?
        {
            return Ok(id);
        }

        if let Some(record) = self
            .platform_client
            .get_routine_record_optional(routine)
            .await?
        {
            let id = routine_encrypted_payload_object_id(record.encrypted_payload.as_ref())
                .unwrap_or(record.id);
            if id != record.id {
                return Err(anyhow!(
                    "routine {} encrypted_payload object_id {} does not match platform id {}",
                    routine,
                    id,
                    record.id
                ));
            }
            self.record_platform_object_id(
                PlatformResourceKind::Routine,
                &Slug::derive(&record.slug),
                id,
            )?;
            if Slug::derive(&record.slug) != *routine {
                self.record_platform_object_id(PlatformResourceKind::Routine, routine, id)?;
            }
            return Ok(id);
        }

        self.ensure_platform_object_id(PlatformResourceKind::Routine, routine)
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
        if let Some(store) = self.resource_ids.as_ref()
            && let Some(id) = store.get(kind, old_slug)?
        {
            store.upsert(kind, new_slug, id)?;
            if old_slug != new_slug {
                store.remove(kind, old_slug)?;
            }
        }
        Ok(())
    }

    fn knowledge_document_object_id(&self, pack: &Slug, doc: &Slug) -> Result<Option<Uuid>> {
        let Some(store) = self.resource_ids.as_ref() else {
            return Ok(None);
        };
        store.get_knowledge_document(pack, doc)
    }

    fn record_knowledge_document_id(&self, pack: &Slug, doc: &Slug, id: Uuid) -> Result<()> {
        if let Some(store) = self.resource_ids.as_ref() {
            store.upsert_knowledge_document(pack, doc, id)?;
        }
        Ok(())
    }

    fn remove_knowledge_document_id(&self, pack: &Slug, doc: &Slug) -> Result<()> {
        if let Some(store) = self.resource_ids.as_ref() {
            store.remove_knowledge_document(pack, doc)?;
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
    ) -> Result<Vec<KnowledgeDocumentEdgeRecord>> {
        let related = related
            .iter()
            .map(|edge| KnowledgeDocEdgeReplaceItem {
                target_doc: edge.target_doc.as_str(),
                edge_type: edge.edge_type.as_str(),
                note: edge.note.as_deref(),
            })
            .collect::<Vec<_>>();
        let edges = self
            .platform_client
            .replace_knowledge_doc_edges(pack, doc, &related)
            .await?;
        Ok(edges
            .into_iter()
            .map(knowledge_edge_record_from_response)
            .collect())
    }

    async fn cached_or_remote_ability(&self, ability_ref: &Slug) -> Result<AbilityManifest> {
        let local = self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .find(|ability| ability_matches_ref(ability, ability_ref));
        if let Some(ability) = local {
            return Ok(ability);
        }

        if let Some(ability) = self.read_only_ability(ability_ref) {
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
            prompt_config: remote.prompt_config,
            platform_scopes: remote.platform_scopes,
            mcp_servers: remote.mcp_servers,
            script_tools: remote.script_tools,
            media: Vec::new(),
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
            .find(|item| item.manifest_slug() == *agent || Slug::derive(&item.name) == *agent)
            .ok_or_else(|| anyhow!("agent not found in local manifest: {agent}"))
    }

    async fn cached_local_agent(&self, agent: &Slug) -> Result<Option<AgentManifest>> {
        Ok(self
            .local_store
            .list_agents()
            .await?
            .into_iter()
            .find(|item| item.manifest_slug() == *agent || Slug::derive(&item.name) == *agent))
    }

    async fn cached_routine(&self, routine: &Slug) -> Result<RoutineManifest> {
        self.local_store
            .list_routines()
            .await?
            .into_iter()
            .find(|item| item.manifest_slug() == *routine || Slug::derive(&item.name) == *routine)
            .ok_or_else(|| anyhow!("routine not found in local manifest: {routine}"))
    }

    async fn cached_local_routine(&self, routine: &Slug) -> Result<Option<RoutineManifest>> {
        Ok(self
            .local_store
            .list_routines()
            .await?
            .into_iter()
            .find(|item| item.manifest_slug() == *routine || Slug::derive(&item.name) == *routine))
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
            .find(|item| item.manifest_slug() == *model || Slug::derive(&item.name) == *model)
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

    async fn cached_local_context_block(
        &self,
        context_block: &Slug,
    ) -> Result<Option<ContextBlockManifest>> {
        Ok(self
            .local_store
            .list_context_blocks()
            .await?
            .into_iter()
            .find(|item| item.slug() == *context_block))
    }

    async fn cached_or_remote_domain(&self, domain_ref: &Slug) -> Result<DomainManifest> {
        let local = self
            .local_store
            .list_domains()
            .await?
            .into_iter()
            .find(|item| item.slug() == *domain_ref);
        if let Some(domain) = local {
            return Ok(domain);
        }

        let Some(remote) = self.platform_client.fetch_domain_record(domain_ref).await? else {
            return Err(anyhow!(
                "domain not found in local manifest: {}",
                domain_ref
            ));
        };

        let remote = self
            .decode_domain_prompt_record(remote)
            .await?
            .to_document();
        let hydrated = local_domain_from_document(remote);
        self.local_store
            .upsert_resource(&ManifestResource::Domain(hydrated.clone()))
            .await?;
        Ok(hydrated)
    }

    async fn cached_local_domain(&self, domain_ref: &Slug) -> Result<Option<DomainManifest>> {
        Ok(self
            .local_store
            .list_domains()
            .await?
            .into_iter()
            .find(|item| {
                item.manifest_slug() == *domain_ref || Slug::derive(&item.name) == *domain_ref
            }))
    }

    async fn cached_local_ability(&self, ability_ref: &Slug) -> Result<Option<AbilityManifest>> {
        Ok(self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .find(|ability| ability_matches_ref(ability, ability_ref)))
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

    async fn cache_knowledge_pack(&self, pack: &KnowledgePackDocument) -> Result<()> {
        let library_root = self.library_root()?;
        ensure_library_knowledge_pack_cache(
            self.local_store.as_ref(),
            &library_root,
            LibraryKnowledgePackCacheEntry {
                slug: pack.slug.clone(),
                name: Some(pack.name.clone()),
                description: Some(pack.description.clone()),
                selector: pack.selector.clone(),
                version: pack.version.clone(),
                read_only: Some(pack.read_only),
                metadata: Some(serde_json::json!({})),
            },
        )
        .await?;
        Ok(())
    }

    async fn cache_knowledge_doc(
        &self,
        doc: &KnowledgeDocSummary,
        doc_id: Uuid,
        content: Option<&str>,
        edges: &[KnowledgeDocumentEdgeRecord],
        replace_edges: ReplaceDocumentEdges,
    ) -> Result<()> {
        let pack_slug = doc.pack.as_str();
        let library_root = self.library_root()?;
        ensure_library_knowledge_pack_cache(
            self.local_store.as_ref(),
            &library_root,
            LibraryKnowledgePackCacheEntry::from_slug(doc.pack.clone()),
        )
        .await?;
        let pack_dir = library_root.join(pack_slug);
        let now = chrono::Utc::now();
        let record = KnowledgeDocumentRecord {
            id: doc_id,
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
            edges: edges.to_vec(),
        };
        upsert_library_knowledge_entry_with_edges(&pack_dir, pack_slug, &record, replace_edges)?;
        if let Some(content) = content {
            write_library_document_content(
                &pack_dir,
                &record.library_doc_relative_path(),
                content,
            )?;
        }
        Ok(())
    }

    fn remove_cached_knowledge_doc(&self, pack: &Slug, doc: &Slug) -> Result<()> {
        let pack_dir = self.library_root()?.join(pack.as_str());
        let existing = library_knowledge_doc_relative_path(&pack_dir, doc);
        remove_library_knowledge_entry(&pack_dir, doc)?;
        if let Some(relative_path) = existing {
            let path = pack_dir.join("docs").join(relative_path);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("Failed to delete {}", path.display()))?;
            }
        }
        Ok(())
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

    async fn configure_agent(&self, params: AgentConfigureParams) -> Result<AgentConfigureResult> {
        let mut data = params.data;
        if data.agent.is_none()
            && let Some(agent) = configure_name_slug(
                data.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref()),
            )
            && let Some(existing) = self.cached_local_agent(&agent).await?
        {
            data.agent = Some(existing.manifest_slug());
        }

        let existing_agent = if let Some(agent) = data.agent.as_ref() {
            match self.cached_local_agent(agent).await? {
                Some(existing) => {
                    if !self.allow_agent(&existing) {
                        return Err(anyhow!("agent not found in local manifest: {}", agent));
                    }
                    Some(existing)
                }
                None => None,
            }
        } else {
            None
        };

        let old_slug = data.agent.clone();
        let heartbeat_patch = data.heartbeat.take();
        let heartbeat_instructions = heartbeat_patch
            .as_ref()
            .and_then(|heartbeat| heartbeat.instructions.clone());
        let mut resolved_prompt_config = None;
        if let Some(prompt_config) = data.prompt_config.as_ref() {
            let agent_object_id = match data.agent.as_ref() {
                Some(agent) => self.platform_object_id(PlatformResourceKind::Agent, agent)?,
                None => {
                    let id = data.id.unwrap_or_else(Uuid::new_v4);
                    data.id = Some(id);
                    id
                }
            };
            let base_prompt_config = existing_agent
                .as_ref()
                .map(|agent| agent.prompt_config.clone())
                .unwrap_or_default();
            let merged_prompt_config =
                merge_prompt_config(&base_prompt_config, prompt_config.clone())?;
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    agent_object_id,
                    SensitiveContentKind::AgentPrompt.encrypted_object_type(),
                    &serde_json::to_value(&merged_prompt_config)?,
                )
                .await?
                .ok_or_else(|| anyhow!("agent prompt encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
            data.prompt_config = None;
            resolved_prompt_config = Some(merged_prompt_config);
        }
        let mut configured = self.platform_client.configure_agent_record(&data).await?;
        let new_slug = Slug::derive(&configured.slug);

        if let Some(heartbeat) = heartbeat_patch {
            let interval = heartbeat
                .interval
                .as_deref()
                .or_else(|| {
                    existing_agent
                        .as_ref()
                        .and_then(|agent| agent.heartbeat.as_ref())
                        .map(|heartbeat| heartbeat.interval.as_str())
                })
                .ok_or_else(|| {
                    anyhow!(
                        "heartbeat.interval is required when configuring heartbeat for agent {}",
                        new_slug
                    )
                })?;
            let encrypted_payload = if let Some(instructions) = heartbeat.instructions.as_ref() {
                Some(
                    self.sensitive_payload_encoder
                        .encode_payload(
                            self.local_manifest_org_id().await?,
                            configured.id,
                            SensitiveContentKind::HeartbeatInstructions.encrypted_object_type(),
                            &serde_json::json!({ "instructions": instructions }),
                        )
                        .await?
                        .ok_or_else(|| {
                            anyhow!("agent heartbeat instructions encryption produced no payload")
                        })?,
                )
            } else {
                None
            };
            let body = AgentHeartbeatConfigureApiBody {
                interval,
                metadata: heartbeat.metadata.as_ref(),
                encrypted_payload: encrypted_payload.as_ref(),
            };
            configured = self
                .platform_client
                .upsert_agent_heartbeat_record(&new_slug, &body)
                .await?;
        }

        let mut local_agent = local_agent_from_record(configured.clone());
        if let Some(prompt_config) = resolved_prompt_config {
            local_agent.prompt_config = prompt_config;
        } else if configured.prompt_config.is_none()
            && let Some(existing_agent) = existing_agent.as_ref()
        {
            local_agent.prompt_config = existing_agent.prompt_config.clone();
        }
        if let Some(heartbeat) = local_agent.heartbeat.as_mut()
            && heartbeat.instructions.is_none()
        {
            heartbeat.instructions = heartbeat_instructions.or_else(|| {
                existing_agent
                    .as_ref()
                    .and_then(|agent| agent.heartbeat.as_ref())
                    .and_then(|heartbeat| heartbeat.instructions.clone())
            });
        }
        let agent_document = AgentDocument::from(local_agent.clone());
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;

        if let Some(old_slug) = old_slug.as_ref() {
            self.move_platform_object_id(PlatformResourceKind::Agent, old_slug, &new_slug)?;
        }
        self.record_platform_object_id(PlatformResourceKind::Agent, &new_slug, configured.id)?;

        Ok(AgentConfigureResult {
            agent: agent_document,
            warnings: Vec::new(),
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
        let mut abilities = self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .filter(|ability| self.allow_ability(ability))
            .collect::<Vec<_>>();
        if let Some(manifest) = self.read_only_manifest.as_ref() {
            for ability in &manifest.abilities {
                if self.allow_ability(ability)
                    && !abilities
                        .iter()
                        .any(|existing| existing.manifest_slug() == ability.manifest_slug())
                {
                    abilities.push(ability.clone());
                }
            }
        }
        let abilities = abilities
            .into_iter()
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

    async fn configure_ability(
        &self,
        params: AbilityConfigureParams,
    ) -> Result<AbilityConfigureResult> {
        let mut data = params.data;
        if data.ability.is_none()
            && let Some(ability) = configure_name_slug(
                data.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref()),
            )
            && let Some(existing) = self.cached_local_ability(&ability).await?
            && self.allow_ability(&existing)
        {
            data.ability = Some(existing.manifest_slug());
        }

        let existing_ability = if let Some(ability) = data.ability.as_ref() {
            match self.cached_local_ability(ability).await? {
                Some(existing) => {
                    if !self.allow_ability(&existing) {
                        return Err(anyhow!("ability not found in local manifest: {}", ability));
                    }
                    Some(existing)
                }
                None => None,
            }
        } else {
            None
        };

        let old_slug = data.ability.clone();
        if data.ability.is_none() {
            data.id = Some(Uuid::new_v4());
        }
        let submitted_prompt_config = data.prompt_config.clone();
        if let Some(prompt_config) = data.prompt_config.as_ref() {
            let ability_object_id = match data.ability.as_ref() {
                Some(ability) => self.platform_object_id(PlatformResourceKind::Ability, ability)?,
                None => data
                    .id
                    .expect("new ability id should be assigned before encoding"),
            };
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    ability_object_id,
                    SensitiveContentKind::AbilityPrompt.encrypted_object_type(),
                    &serde_json::json!(prompt_config),
                )
                .await?
                .ok_or_else(|| anyhow!("ability prompt encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
            data.prompt_config = None;
        }

        let configured = self
            .platform_client
            .configure_ability_document(&data)
            .await?;
        let new_slug = Slug::derive(&configured.summary.name);
        let mut local_ability = local_ability_from_document(configured);
        if let Some(prompt_config) = submitted_prompt_config {
            local_ability.prompt_config = prompt_config;
        } else if let Some(existing_ability) = existing_ability.as_ref() {
            local_ability.prompt_config = existing_ability.prompt_config.clone();
        }
        if let Some(existing_ability) = existing_ability {
            local_ability.source_type = existing_ability.source_type;
            local_ability.read_only = existing_ability.read_only;
            local_ability.metadata = existing_ability.metadata;
        }
        let ability_document = AbilityDocument::from(local_ability.clone());
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;

        if let Some(old_slug) = old_slug.as_ref() {
            self.move_platform_object_id(PlatformResourceKind::Ability, old_slug, &new_slug)?;
        } else if let Some(id) = data.id {
            self.record_platform_object_id(PlatformResourceKind::Ability, &new_slug, id)?;
        }

        Ok(AbilityConfigureResult {
            ability: ability_document,
            warnings: Vec::new(),
        })
    }
}

#[async_trait]
impl<L, E> CommandManifestBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn list_commands(&self) -> Result<CommandsListResult> {
        let commands = self
            .local_store
            .load_manifest()
            .await?
            .commands
            .into_iter()
            .map(CommandSummary::from)
            .collect();
        Ok(CommandsListResult { commands })
    }

    async fn get_command(&self, params: CommandsGetParams) -> Result<CommandGetResult> {
        let command_ref = params.command;
        let command = self
            .local_store
            .load_manifest()
            .await?
            .commands
            .into_iter()
            .find(|command| command_matches_ref(command, &command_ref))
            .ok_or_else(|| anyhow!("command not found in local manifest: {command_ref}"))?;

        Ok(CommandGetResult { command })
    }

    async fn configure_command(
        &self,
        params: CommandConfigureParams,
    ) -> Result<CommandConfigureResult> {
        let existing_command = if let Some(command_ref) = params.data.command_ref.as_deref() {
            let manifest = self.local_store.load_manifest().await?;
            Some(
                manifest
                    .commands
                    .into_iter()
                    .find(|command| command_matches_ref(command, command_ref))
                    .ok_or_else(|| anyhow!("command not found in local manifest: {command_ref}"))?,
            )
        } else {
            None
        };

        if let Some(existing) = existing_command.as_ref()
            && (existing.read_only || existing.source_type != "native")
        {
            return Err(anyhow!("package-managed commands cannot be edited locally"));
        }

        let old_slug = existing_command
            .as_ref()
            .map(|command| command.manifest_slug());
        let mut data = params.data;
        if data.command_ref.is_none() && data.content.is_none() {
            return Err(anyhow!("content is required when creating a command"));
        }
        if let Some(slug) = old_slug.as_ref() {
            data.id = Some(self.platform_object_id(PlatformResourceKind::Command, slug)?);
        }

        let submitted_content = data.content.clone();
        if let Some(content) = data.content.as_ref() {
            let command_object_id = match data.id {
                Some(id) => id,
                None => {
                    let id = Uuid::new_v4();
                    data.id = Some(id);
                    id
                }
            };
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    command_object_id,
                    SensitiveContentKind::CommandContent.encrypted_object_type(),
                    &serde_json::json!(content),
                )
                .await?
                .ok_or_else(|| anyhow!("command content encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
            data.content = None;
        }

        let (configured_id, mut local_command) = self
            .platform_client
            .configure_command_document(&data)
            .await?;
        if let Some(content) = submitted_content {
            local_command.content = content;
        } else if let Some(existing) = existing_command {
            local_command.content = existing.content;
        }
        local_command.source_type = "native".to_string();
        local_command.read_only = false;

        self.local_store
            .upsert_resource(&ManifestResource::Command(local_command.clone()))
            .await?;

        if let Some(old_slug) = old_slug.as_ref() {
            self.move_platform_object_id(
                PlatformResourceKind::Command,
                old_slug,
                &local_command.manifest_slug(),
            )?;
        }
        self.record_platform_object_id(
            PlatformResourceKind::Command,
            &local_command.manifest_slug(),
            configured_id,
        )?;

        Ok(CommandConfigureResult {
            command: local_command,
            warnings: Vec::new(),
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
        let domain = self.cached_or_remote_domain(&params.domain).await?;
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

    async fn configure_domain(
        &self,
        params: DomainConfigureParams,
    ) -> Result<DomainConfigureResult> {
        let mut data = params.data;
        if data.domain.is_none()
            && let Some(metadata) = data.metadata.as_ref()
            && let Some(name) = metadata
                .name
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
        {
            let path = metadata.path.as_deref().unwrap_or_default();
            let domain = domain_slug(path, name);
            if let Some(existing) = self.cached_local_domain(&domain).await?
                && self.allow_domain(&existing)
            {
                data.domain = Some(existing.manifest_slug());
            }
        }

        let existing_domain = if let Some(domain) = data.domain.as_ref() {
            match self.cached_local_domain(domain).await? {
                Some(existing) => {
                    if !self.allow_domain(&existing) {
                        return Err(anyhow!("domain not found in local manifest: {}", domain));
                    }
                    Some(existing)
                }
                None => None,
            }
        } else {
            None
        };

        let old_slug = data.domain.clone();
        if data.domain.is_none() {
            data.id = Some(Uuid::new_v4());
        }
        let submitted_prompt_config = data.prompt_config.clone();
        if let Some(prompt_config) = data.prompt_config.as_ref() {
            let domain_object_id = match data.domain.as_ref() {
                Some(domain) => self.platform_object_id(PlatformResourceKind::Domain, domain)?,
                None => data
                    .id
                    .expect("new domain id should be assigned before encoding"),
            };
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    domain_object_id,
                    SensitiveContentKind::DomainPrompt.encrypted_object_type(),
                    &serde_json::json!(prompt_config),
                )
                .await?
                .ok_or_else(|| anyhow!("domain prompt encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
            data.prompt_config = None;
        }

        let configured = self
            .decode_domain_prompt_record(self.platform_client.configure_domain_record(&data).await?)
            .await?
            .to_document();
        let new_slug = configured.summary.slug.clone();
        let mut local_domain = local_domain_from_document(configured);
        if let Some(prompt_config) = submitted_prompt_config {
            local_domain.prompt_config = prompt_config;
        } else if let Some(existing_domain) = existing_domain {
            local_domain.prompt_config = existing_domain.prompt_config;
        }
        let domain_document = DomainDocument::from(local_domain.clone());
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;

        if let Some(old_slug) = old_slug.as_ref() {
            self.move_platform_object_id(PlatformResourceKind::Domain, old_slug, &new_slug)?;
        } else if let Some(id) = data.id {
            self.record_platform_object_id(PlatformResourceKind::Domain, &new_slug, id)?;
        }

        Ok(DomainConfigureResult {
            domain: domain_document,
            warnings: Vec::new(),
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
        if let Err(error) = self.cache_knowledge_pack(&knowledge_pack).await {
            bail!(
                "knowledge pack '{}' was created on the platform, but local cache registration failed: {error}",
                knowledge_pack.slug
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
        if let Err(error) = self.cache_knowledge_pack(&knowledge_pack).await {
            bail!(
                "knowledge pack '{}' was updated on the platform, but local cache registration failed: {error}",
                knowledge_pack.slug
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
        let edges = if data.related.is_empty() {
            Vec::new()
        } else {
            self.replace_knowledge_doc_related(&data.pack, &knowledge_doc.slug, &data.related)
                .await?
        };
        if let Err(error) = self
            .cache_knowledge_doc(
                &knowledge_doc,
                doc_id,
                Some(&data.content),
                &edges,
                ReplaceDocumentEdges::Yes,
            )
            .await
        {
            tracing::warn!(
                pack = %data.pack,
                slug = %knowledge_doc.slug,
                error = %error,
                "Failed to cache created knowledge document locally"
            );
        }
        self.record_knowledge_document_id(&data.pack, &knowledge_doc.slug, doc_id)?;
        Ok(KnowledgeDocMutationResult {
            knowledge_doc,
            edges,
        })
    }

    async fn update_knowledge_doc(
        &self,
        params: KnowledgeDocUpdateParams,
    ) -> Result<KnowledgeDocMutationResult> {
        let doc_id = match self.knowledge_document_object_id(&params.pack, &params.slug)? {
            Some(id) => id,
            None => {
                self.platform_client
                    .resolve_knowledge_doc_slug(&params.pack, &params.slug)
                    .await?
            }
        };
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

        let edges = if let Some(related) = params.data.related.as_deref() {
            self.replace_knowledge_doc_related(&params.pack, &params.slug, related)
                .await?
        } else {
            Vec::new()
        };
        let replace_edges = if params.data.related.is_some() {
            ReplaceDocumentEdges::Yes
        } else {
            ReplaceDocumentEdges::No
        };
        if let Err(error) = self
            .cache_knowledge_doc(
                &knowledge_doc,
                doc_id,
                params.data.content.as_deref(),
                &edges,
                replace_edges,
            )
            .await
        {
            tracing::warn!(
                pack = %params.pack,
                slug = %knowledge_doc.slug,
                error = %error,
                "Failed to cache updated knowledge document locally"
            );
        }

        self.record_knowledge_document_id(&params.pack, &knowledge_doc.slug, doc_id)?;
        Ok(KnowledgeDocMutationResult {
            knowledge_doc,
            edges,
        })
    }

    async fn delete_knowledge_doc(&self, params: KnowledgeDocDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_knowledge_doc(&params.pack, &params.slug)
            .await?;
        if let Err(error) = self.remove_cached_knowledge_doc(&params.pack, &params.slug) {
            tracing::warn!(
                pack = %params.pack,
                slug = %params.slug,
                error = %error,
                "Failed to remove deleted knowledge document from local cache"
            );
        }
        self.remove_knowledge_document_id(&params.pack, &params.slug)?;
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

    async fn configure_routine(
        &self,
        params: RoutineConfigureParams,
    ) -> Result<RoutineConfigureResult> {
        let mut data = params.data;
        if data.routine.is_none()
            && data
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.name.as_ref())
                .is_none_or(|name| name.trim().is_empty())
        {
            return Err(anyhow!("metadata.name is required when creating a routine"));
        }

        if data.routine.is_none()
            && let Some(routine) = configure_name_slug(
                data.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref()),
            )
            && let Some(existing) = self.cached_local_routine(&routine).await?
        {
            data.routine = Some(existing.manifest_slug());
        }

        let existing_routine = if let Some(existing_routine) = data.routine.as_ref() {
            self.cached_local_routine(existing_routine).await?
        } else {
            None
        };

        if data.id.is_none() {
            let routine_id = match data.routine.as_ref() {
                Some(routine) => self.ensure_routine_object_id(routine).await?,
                None => {
                    let routine = configure_name_slug(
                        data.metadata
                            .as_ref()
                            .and_then(|metadata| metadata.name.as_ref()),
                    )
                    .ok_or_else(|| anyhow!("metadata.name is required when creating a routine"))?;
                    self.ensure_platform_object_id(PlatformResourceKind::Routine, &routine)?
                }
            };
            data.id = Some(routine_id);
        }

        if data.cron_task.is_some()
            && data
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.trigger)
                .is_none()
            && existing_routine
                .as_ref()
                .map(|routine| routine.trigger)
                .is_none()
        {
            data.metadata
                .get_or_insert_with(RoutineConfigureMetadata::default)
                .trigger = Some(RoutineTrigger::Cron);
        }

        let mut submitted_step_instructions = Vec::new();
        if let Some(graph) = data.graph.as_mut() {
            let org_id = self.local_manifest_org_id().await?;
            for step in &mut graph.steps {
                let instructions = step
                    .config
                    .instructions()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let Some(instructions) = instructions else {
                    continue;
                };
                let step_id = step.id.unwrap_or_else(Uuid::new_v4);
                step.id = Some(step_id);
                submitted_step_instructions.push((step.slug.clone(), instructions.clone()));
                step.config.clear_instructions();
                let encrypted_payload = self
                    .sensitive_payload_encoder
                    .encode_payload(
                        org_id,
                        step_id,
                        SensitiveContentKind::RoutineStepInstructions.encrypted_object_type(),
                        &serde_json::json!({ "instructions": instructions }),
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow!("routine step instructions encryption produced no payload")
                    })?;
                step.encrypted_payload = Some(encrypted_payload);
            }
        }

        let mut submitted_cron_task = None;
        if let Some(cron_task) = data.cron_task.take() {
            if data.encrypted_payload.is_some() {
                return Err(anyhow!(
                    "routine cron_task and encrypted_payload cannot both be provided"
                ));
            }
            let trigger = data
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.trigger)
                .or_else(|| existing_routine.as_ref().map(|routine| routine.trigger));
            if !matches!(trigger, Some(RoutineTrigger::Cron)) {
                return Err(anyhow!("routine cron_task requires a cron routine trigger"));
            }
            let routine_object_id = match data.routine.as_ref() {
                Some(routine) => self
                    .optional_platform_object_id(PlatformResourceKind::Routine, routine)?
                    .or(data.id)
                    .ok_or_else(|| anyhow!("routine configure id was not initialized"))?,
                None => data
                    .id
                    .ok_or_else(|| anyhow!("routine configure id was not initialized"))?,
            };
            submitted_cron_task = Some(RoutineCronTaskManifest {
                title: cron_task.title.clone(),
                description: cron_task.description.clone(),
                acceptance_criteria: cron_task.acceptance_criteria.clone(),
            });
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    routine_object_id,
                    SensitiveContentKind::RoutineCronTask.encrypted_object_type(),
                    &serde_json::to_value(&cron_task)?,
                )
                .await?
                .ok_or_else(|| anyhow!("routine cron task encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
        }
        let configured = self.platform_client.configure_routine_record(&data).await?;
        let mut routine_document = configured.to_document();
        for (step_slug, instructions) in submitted_step_instructions {
            if let Some(step) = routine_document
                .steps
                .iter_mut()
                .find(|step| step.slug == step_slug)
                && let Some(config) = step.config.as_object_mut()
            {
                config.insert(
                    "instructions".to_string(),
                    serde_json::Value::String(instructions),
                );
            }
        }
        if routine_document.metadata.cron_task.is_none() {
            routine_document.metadata.cron_task = submitted_cron_task.or_else(|| {
                existing_routine
                    .as_ref()
                    .and_then(|routine| routine.metadata.cron_task.clone())
            });
        }
        let local_routine = local_routine_from_document(&routine_document);
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        let new_slug = Slug::derive(&configured.slug);
        self.record_platform_object_id(PlatformResourceKind::Routine, &new_slug, configured.id)?;
        Ok(RoutineConfigureResult {
            routine: routine_document,
            warnings: Vec::new(),
        })
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
            native_tools: created.native_tools.clone(),
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
            native_tools: params
                .data
                .native_tools
                .or_else(|| Some(existing.native_tools.clone())),
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
            native_tools: updated.native_tools.clone(),
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

    async fn configure_context_block(
        &self,
        params: ContextBlockConfigureParams,
    ) -> Result<ContextBlockConfigureResult> {
        let mut data = params.data;
        if data.context_block.is_none()
            && let Some(metadata) = data.metadata.as_ref()
            && let Some(name) = metadata
                .name
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
        {
            let path = metadata.path.as_deref().unwrap_or_default();
            let context_block = context_block_slug(path, name);
            if let Some(existing) = self.cached_local_context_block(&context_block).await? {
                data.context_block = Some(existing.manifest_slug());
            }
        }

        let existing_context_block = if let Some(context_block) = data.context_block.as_ref() {
            self.cached_local_context_block(context_block).await?
        } else {
            None
        };

        let old_slug = data.context_block.clone();
        if data.context_block.is_none() {
            data.id = Some(Uuid::new_v4());
        }
        let submitted_template = data.template.clone();
        let fallback_template = existing_context_block
            .as_ref()
            .map(|item| item.template.clone())
            .unwrap_or_default();
        if data.template.is_some() {
            let context_block_object_id = match data.context_block.as_ref() {
                Some(context_block) => {
                    self.platform_object_id(PlatformResourceKind::ContextBlock, context_block)?
                }
                None => data
                    .id
                    .expect("new context block id should be assigned before encoding"),
            };
            let encrypted_payload = self
                .sensitive_payload_encoder
                .encode_payload(
                    self.local_manifest_org_id().await?,
                    context_block_object_id,
                    SensitiveContentKind::ContextBlockContent.encrypted_object_type(),
                    &serde_json::json!(submitted_template.clone().unwrap_or_default()),
                )
                .await?
                .ok_or_else(|| anyhow!("context block template encryption produced no payload"))?;
            data.encrypted_payload = Some(encrypted_payload);
            data.template = None;
        }

        let configured = self
            .platform_client
            .configure_context_block_document(&data)
            .await?;
        let new_slug = configured.summary.slug.clone();
        let template = submitted_template.unwrap_or_else(|| {
            if configured.template.is_empty() {
                fallback_template
            } else {
                configured.template.clone()
            }
        });
        let local_context_block = ContextBlockManifest {
            name: configured.summary.name.clone(),
            path: configured.summary.path.clone(),
            description: configured.summary.description.clone(),
            template,
        };
        let context_block_document = ContextBlockDocument::from(local_context_block.clone());
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;

        if let Some(old_slug) = old_slug.as_ref() {
            self.move_platform_object_id(PlatformResourceKind::ContextBlock, old_slug, &new_slug)?;
        } else if let Some(id) = data.id {
            self.record_platform_object_id(PlatformResourceKind::ContextBlock, &new_slug, id)?;
        }

        Ok(ContextBlockConfigureResult {
            context_block: context_block_document,
            warnings: Vec::new(),
        })
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

    #[derive(Debug, Clone, Default)]
    struct TestSensitivePayloadEncoder;

    #[async_trait::async_trait]
    impl SensitivePayloadEncoder for TestSensitivePayloadEncoder {
        async fn encode_payload(
            &self,
            _account_id: Uuid,
            object_id: Uuid,
            object_type: &str,
            _payload: &serde_json::Value,
        ) -> Result<Option<serde_json::Value>> {
            Ok(Some(json!({
                "object_id": object_id,
                "object_type": object_type,
                "ciphertext": "encrypted-test-payload",
                "encryption_scope": "org"
            })))
        }

        async fn decode_payload(
            &self,
            payload: &serde_json::Value,
        ) -> Result<Option<serde_json::Value>> {
            match payload
                .get("object_type")
                .and_then(serde_json::Value::as_str)
            {
                Some("manifest.domain.prompt") => Ok(Some(json!({
                    "developer_prompt_addon": "DECODED_DOMAIN_PROMPT_123"
                }))),
                _ => Ok(None),
            }
        }
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
            library_dir.join(crate::library_knowledge::LIBRARY_KNOWLEDGE_MANIFEST_FILENAME),
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

    async fn spawn_single_request_server(
        expected_method: &'static str,
        expected_path: &'static str,
        status: &'static str,
        response_body: serde_json::Value,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            let (mut stream, _) = listener.accept().await?;
            let request = read_request(&mut stream).await?;
            let body = match (request.method.as_str(), request.path.as_str()) {
                (method, path) if method == expected_method && path == expected_path => {
                    response(status, response_body)
                }
                _ => response("404 Not Found", json!({ "error": "not found" })),
            };
            stream.write_all(body.as_bytes()).await?;
            requests.push(request);
            Ok(requests)
        });
        Ok((base_url, handle))
    }

    async fn spawn_request_sequence_server(
        expected: Vec<(&'static str, &'static str, &'static str, serde_json::Value)>,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (expected_method, expected_path, status, response_body) in expected {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    (method, path) if method == expected_method && path == expected_path => {
                        response(status, response_body)
                    }
                    _ => response("404 Not Found", json!({ "error": "not found" })),
                };
                stream.write_all(body.as_bytes()).await?;
                requests.push(request);
            }
            Ok(requests)
        });
        Ok((base_url, handle))
    }

    async fn spawn_knowledge_update_server(
        pack_id: Uuid,
        doc_id: Uuid,
        target_doc_id: Uuid,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let org_id = Uuid::new_v4();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..4 {
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
                                "org_id": org_id,
                                "pack_id": pack_id,
                                "slug": "guide",
                                "filename": "guide.md",
                                "path": "docs/guide.md",
                                "title": "Guide",
                                "kind": "guide",
                                "summary": "Guide",
                                "tags": ["core"],
                                "content_type": "text/plain",
                                "created_at": "2026-05-23T00:00:00Z",
                                "updated_at": "2026-05-23T00:00:00Z"
                            },
                            {
                                "id": target_doc_id,
                                "org_id": org_id,
                                "pack_id": pack_id,
                                "slug": "target",
                                "filename": "target.md",
                                "path": "docs/target.md",
                                "title": "Target",
                                "kind": "guide",
                                "summary": "Target",
                                "tags": [],
                                "content_type": "text/plain",
                                "created_at": "2026-05-23T00:00:00Z",
                                "updated_at": "2026-05-23T00:00:00Z"
                            }
                        ]),
                    ),
                    ("PUT", "/api/v1/knowledge/test-pack/items/guide/content") => response(
                        "200 OK",
                        json!({
                            "id": doc_id,
                            "org_id": org_id,
                            "pack_id": pack_id,
                            "slug": "guide",
                            "filename": "guide.md",
                            "path": "docs/guide.md",
                            "title": "Guide",
                            "kind": "guide",
                            "summary": "Updated guide",
                            "tags": ["core"],
                            "content_type": "text/plain",
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:00:00Z"
                        }),
                    ),
                    ("PATCH", "/api/v1/knowledge/test-pack/items/guide") => response(
                        "200 OK",
                        json!({
                            "id": doc_id,
                            "org_id": org_id,
                            "pack_id": pack_id,
                            "slug": "guide",
                            "filename": "guide.md",
                            "path": "docs/guide.md",
                            "title": "Guide",
                            "kind": "guide",
                            "summary": "Updated guide",
                            "tags": ["core"],
                            "content_type": "text/markdown",
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:01:00Z"
                        }),
                    ),
                    ("PUT", "/api/v1/knowledge/test-pack/items/guide/edges") => response(
                        "200 OK",
                        json!([{
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
                        }]),
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

    #[tokio::test]
    async fn configure_agent_sends_only_encrypted_prompt_payload() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/agents/configure",
            "201 Created",
            json!({
                "id": agent_id,
                "org_id": org_id,
                "slug": "reviewer",
                "name": "Reviewer",
                "description": null,
                "color": "#0EA5E9",
                "model": null,
                "model_id": null,
                "model_name": null,
                "domains": [],
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "abilities": [],
                "prompt_locked": false,
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "encrypted_payload": {
                    "account_id": org_id,
                    "encryption_scope": "org",
                    "object_id": agent_id,
                    "object_type": "manifest.agent.prompt",
                    "algorithm": "AES-256-GCM",
                    "key_version": 1,
                    "nonce": "nonce",
                    "ciphertext": "encrypted-test-payload"
                },
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let prompt_patch = json!({
            "developer_prompt": "Sensitive agent prompt",
            "templates": { "chat": "Sensitive chat template" }
        });
        let configured = backend
            .configure_agent(AgentConfigureParams {
                data: AgentConfigureDocument {
                    metadata: Some(AgentConfigureMetadata {
                        name: Some("Reviewer".to_string()),
                        description: None,
                        color: None,
                        model: None,
                    }),
                    prompt_config: Some(prompt_patch),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            configured.agent.prompt_config.developer_prompt,
            "Sensitive agent prompt"
        );
        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert!(body.get("prompt_config").is_none());
        assert!(body.get("encrypted_payload").is_some());
        assert!(!requests[0].body.contains("Sensitive agent prompt"));
        assert!(!requests[0].body.contains("Sensitive chat template"));
    }

    #[tokio::test]
    async fn configure_agent_encrypts_heartbeat_instructions_and_preserves_manifest_plaintext() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/api/v1/agents/configure") => response(
                        "201 Created",
                        json!({
                            "id": agent_id,
                            "org_id": org_id,
                            "slug": "reviewer",
                            "name": "Reviewer",
                            "description": null,
                            "color": null,
                            "model": null,
                            "model_id": null,
                            "model_name": null,
                            "domains": [],
                            "platform_scopes": [],
                            "mcp_servers": [],
                            "script_tools": [],
                            "abilities": [],
                            "prompt_locked": false,
                            "source_type": "native",
                            "read_only": false,
                            "metadata": {},
                            "created_by": null,
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:00:00Z"
                        }),
                    ),
                    ("PUT", "/api/v1/agents/reviewer/heartbeat") => response(
                        "200 OK",
                        json!({
                            "id": agent_id,
                            "org_id": org_id,
                            "slug": "reviewer",
                            "name": "Reviewer",
                            "description": null,
                            "color": null,
                            "model": null,
                            "model_id": null,
                            "model_name": null,
                            "domains": [],
                            "platform_scopes": [],
                            "mcp_servers": [],
                            "script_tools": [],
                            "abilities": [],
                            "prompt_locked": false,
                            "heartbeat": {
                                "agent": "reviewer",
                                "interval": "5m",
                                "is_active": false,
                                "last_run_at": null,
                                "next_run_at": null,
                                "metadata": { "timezone": "America/Chicago" }
                            },
                            "source_type": "native",
                            "read_only": false,
                            "metadata": {},
                            "created_by": null,
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:00:00Z"
                        }),
                    ),
                    _ => response("404 Not Found", json!({ "error": "not found" })),
                };
                stream.write_all(body.as_bytes()).await?;
                requests.push(request);
            }
            Ok::<_, anyhow::Error>(requests)
        });
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: AgentConfigureDocument {
                    metadata: Some(AgentConfigureMetadata {
                        name: Some("Reviewer".to_string()),
                        description: None,
                        color: None,
                        model: None,
                    }),
                    heartbeat: Some(AgentHeartbeatConfigureDocument {
                        interval: Some("5m".to_string()),
                        metadata: Some(json!({ "timezone": "America/Chicago" })),
                        instructions: Some("Sensitive heartbeat instructions".to_string()),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result
                .agent
                .heartbeat
                .as_ref()
                .and_then(|heartbeat| heartbeat.instructions.as_deref()),
            Some("Sensitive heartbeat instructions")
        );
        let requests = server.await.unwrap().unwrap();
        let configure_body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        let heartbeat_body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert!(configure_body.get("heartbeat").is_none());
        assert!(heartbeat_body.get("encrypted_payload").is_some());
        assert_eq!(
            heartbeat_body["encrypted_payload"]["object_type"],
            SensitiveContentKind::HeartbeatInstructions.encrypted_object_type()
        );
        assert!(
            !requests[0]
                .body
                .contains("Sensitive heartbeat instructions")
        );
        assert!(
            !requests[1]
                .body
                .contains("Sensitive heartbeat instructions")
        );
    }

    #[tokio::test]
    async fn configure_ability_sends_only_encrypted_prompt_payload() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let ability_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/abilities/configure",
            "201 Created",
            json!({
                "id": ability_id,
                "org_id": org_id,
                "slug": "review-code",
                "name": "review_code",
                "path": "",
                "description": null,
                "activation_condition": "",
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "encrypted_payload": {
                    "account_id": org_id,
                    "encryption_scope": "org",
                    "object_id": ability_id,
                    "object_type": "manifest.ability.prompt",
                    "algorithm": "AES-256-GCM",
                    "key_version": 1,
                    "nonce": "nonce",
                    "ciphertext": "encrypted-test-payload"
                },
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let configured = backend
            .configure_ability(AbilityConfigureParams {
                data: AbilityConfigureDocument {
                    metadata: Some(AbilityConfigureMetadata {
                        name: Some("review_code".to_string()),
                        path: None,
                        description: None,
                        activation_condition: None,
                    }),
                    prompt_config: Some(nenjo::manifest::AbilityPromptConfig {
                        developer_prompt: "Sensitive ability prompt".to_string(),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            configured.ability.prompt_config.developer_prompt,
            "Sensitive ability prompt"
        );
        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert!(body.get("prompt_config").is_none());
        assert!(body.get("encrypted_payload").is_some());
        assert!(!requests[0].body.contains("Sensitive ability prompt"));
    }

    #[tokio::test]
    async fn configure_context_block_sends_only_encrypted_template_payload() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let block_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/context-blocks/configure",
            "201 Created",
            json!({
                "id": block_id,
                "org_id": org_id,
                "slug": "repo-guidance",
                "name": "repo_guidance",
                "path": "",
                "description": null,
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let configured = backend
            .configure_context_block(ContextBlockConfigureParams {
                data: ContextBlockConfigureDocument {
                    metadata: Some(ContextBlockConfigureMetadata {
                        name: Some("repo_guidance".to_string()),
                        path: None,
                        description: None,
                    }),
                    template: Some("Sensitive context block template".to_string()),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            configured.context_block.template,
            "Sensitive context block template"
        );
        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert!(body.get("template").is_none());
        assert!(body.get("encrypted_payload").is_some());
        assert!(
            !requests[0]
                .body
                .contains("Sensitive context block template")
        );
    }

    #[tokio::test]
    async fn configure_ability_readback_preserves_submitted_prompt() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let ability_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/abilities/configure",
            "201 Created",
            json!({
                "id": ability_id,
                "org_id": org_id,
                "slug": "mcp-payload-smoke-ability",
                "name": "mcp_payload_smoke_ability",
                "path": "",
                "description": null,
                "activation_condition": "",
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "encrypted_payload": {
                    "account_id": org_id,
                    "encryption_scope": "org",
                    "object_id": ability_id,
                    "object_type": "manifest.ability.prompt",
                    "algorithm": "AES-256-GCM",
                    "key_version": 1,
                    "nonce": "nonce",
                    "ciphertext": "encrypted-test-payload"
                },
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        backend
            .configure_ability(AbilityConfigureParams {
                data: AbilityConfigureDocument {
                    metadata: Some(AbilityConfigureMetadata {
                        name: Some("mcp_payload_smoke_ability".to_string()),
                        path: None,
                        description: None,
                        activation_condition: None,
                    }),
                    prompt_config: Some(nenjo::manifest::AbilityPromptConfig {
                        developer_prompt: "SMOKE_ABILITY_PROMPT_123".to_string(),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        let _ = server.await.unwrap().unwrap();

        let result = backend
            .get_ability(AbilitiesGetParams {
                ability: Slug::derive("mcp_payload_smoke_ability"),
            })
            .await
            .unwrap();
        assert_eq!(
            result.ability.prompt_config.developer_prompt,
            "SMOKE_ABILITY_PROMPT_123"
        );
    }

    #[tokio::test]
    async fn configure_domain_readback_preserves_submitted_prompt() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let domain_id = Uuid::new_v4();
        let (base_url, server) = spawn_domain_configure_server(org_id, domain_id)
            .await
            .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let configured = backend
            .configure_domain(DomainConfigureParams {
                data: DomainConfigureDocument {
                    metadata: Some(DomainConfigureMetadata {
                        name: Some("Build Domain".to_string()),
                        path: None,
                        description: None,
                        command: Some("#build-domain".to_string()),
                    }),
                    prompt_config: Some(nenjo::manifest::DomainPromptConfig {
                        developer_prompt_addon: Some("SMOKE_DOMAIN_PROMPT_123".to_string()),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let result = backend
            .get_domain(DomainsGetParams {
                domain: configured.domain.summary.slug.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            result
                .domain
                .prompt_config
                .developer_prompt_addon
                .as_deref(),
            Some("SMOKE_DOMAIN_PROMPT_123")
        );
        let _ = server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn configure_context_block_readback_preserves_submitted_template() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let block_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/context-blocks/configure",
            "201 Created",
            json!({
                "id": block_id,
                "org_id": org_id,
                "slug": "smoke-mcp-payload-smoke-context",
                "name": "mcp_payload_smoke_context",
                "path": "smoke",
                "description": null,
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        backend
            .configure_context_block(ContextBlockConfigureParams {
                data: ContextBlockConfigureDocument {
                    metadata: Some(ContextBlockConfigureMetadata {
                        name: Some("mcp_payload_smoke_context".to_string()),
                        path: Some("smoke".to_string()),
                        description: None,
                    }),
                    template: Some("SMOKE_CONTEXT_TEMPLATE_123".to_string()),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        let _ = server.await.unwrap().unwrap();

        let result = backend
            .get_context_block(ContextBlocksGetParams {
                context_block: Slug::derive("smoke-mcp_payload_smoke_context"),
            })
            .await
            .unwrap();
        assert_eq!(result.context_block.template, "SMOKE_CONTEXT_TEMPLATE_123");
    }

    #[tokio::test]
    async fn configure_routine_encrypts_cron_task_and_preserves_manifest_plaintext() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let resource_ids = Arc::new(PlatformResourceIdStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let (base_url, server) = spawn_request_sequence_server(vec![
            (
                "GET",
                "/api/v1/routines/nightly-review",
                "404 Not Found",
                json!({ "error": "not found" }),
            ),
            (
                "POST",
                "/api/v1/routines/configure",
                "201 Created",
                json!({
                    "id": routine_id,
                    "org_id": org_id,
                    "project_id": null,
                    "slug": "nightly-review",
                    "name": "Nightly Review",
                    "description": null,
                    "trigger": "cron",
                    "is_active": false,
                    "is_default": false,
                    "max_retries": 0,
                    "step_count": 0,
                    "metadata": {
                        "schedule": "0 0 * * *",
                        "entry_steps": []
                    },
                    "steps": [],
                    "edges": [],
                    "created_by": null,
                    "created_at": "2026-05-23T00:00:00Z",
                    "updated_at": "2026-05-23T00:00:00Z"
                }),
            ),
        ])
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_resource_id_store(resource_ids.clone())
            .with_cached_org_id(Some(org_id));

        let result = backend
            .configure_routine(RoutineConfigureParams {
                data: RoutineConfigureDocument {
                    routine: Some(Slug::derive("nightly-review")),
                    metadata: Some(RoutineConfigureMetadata {
                        name: Some("Nightly Review".to_string()),
                        description: None,
                        project_id: None,
                        trigger: None,
                        is_active: None,
                        max_retries: None,
                    }),
                    runtime_metadata: Some(json!({
                        "schedule": "0 0 * * *",
                        "entry_steps": []
                    })),
                    cron_task: Some(RoutineCronTaskConfigureDocument {
                        title: "Sensitive nightly task".to_string(),
                        description: Some("Sensitive task description".to_string()),
                        acceptance_criteria: Some("Sensitive acceptance criteria".to_string()),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let cron_task = result
            .routine
            .metadata
            .cron_task
            .as_ref()
            .expect("cron task should be preserved in local manifest");
        assert_eq!(cron_task.title, "Sensitive nightly task");
        assert_eq!(
            cron_task.description.as_deref(),
            Some("Sensitive task description")
        );
        assert_eq!(
            cron_task.acceptance_criteria.as_deref(),
            Some("Sensitive acceptance criteria")
        );
        let requests = server.await.unwrap().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/routines/nightly-review");
        let body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        let request_routine_id = body["id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("routine id should be generated before configure");
        assert_eq!(body["routine"], "nightly-review");
        assert_eq!(body["metadata"]["trigger"], "cron");
        assert!(body.get("cron_task").is_none());
        assert!(body.get("encrypted_payload").is_some());
        assert_eq!(
            body["encrypted_payload"]["object_id"],
            request_routine_id.to_string()
        );
        assert_eq!(
            body["encrypted_payload"]["object_type"],
            SensitiveContentKind::RoutineCronTask.encrypted_object_type()
        );
        assert!(!requests[1].body.contains("Sensitive nightly task"));
        assert!(!requests[1].body.contains("Sensitive task description"));
        assert!(!requests[1].body.contains("Sensitive acceptance criteria"));
        assert_eq!(
            resource_ids
                .get(
                    PlatformResourceKind::Routine,
                    &Slug::derive("nightly-review")
                )
                .unwrap(),
            Some(routine_id)
        );
    }

    #[tokio::test]
    async fn configure_routine_recovers_missing_sidecar_id_from_existing_encrypted_payload() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let resource_ids = Arc::new(PlatformResourceIdStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let (base_url, server) = spawn_request_sequence_server(vec![
            (
                "GET",
                "/api/v1/routines/nightly-review",
                "200 OK",
                json!({
                    "id": routine_id,
                    "org_id": org_id,
                    "project_id": null,
                    "slug": "nightly-review",
                    "name": "Nightly Review",
                    "description": null,
                    "trigger": "cron",
                    "is_active": false,
                    "is_default": false,
                    "max_retries": 0,
                    "step_count": 0,
                    "metadata": {
                        "schedule": "0 0 * * *",
                        "entry_steps": []
                    },
                    "encrypted_payload": {
                        "object_id": routine_id,
                        "object_type": "routine.cron_task",
                        "ciphertext": "existing-encrypted-payload",
                        "encryption_scope": "org"
                    },
                    "steps": [],
                    "edges": [],
                    "created_by": null,
                    "created_at": "2026-05-23T00:00:00Z",
                    "updated_at": "2026-05-23T00:00:00Z"
                }),
            ),
            (
                "POST",
                "/api/v1/routines/configure",
                "200 OK",
                json!({
                    "id": routine_id,
                    "org_id": org_id,
                    "project_id": null,
                    "slug": "nightly-review",
                    "name": "Nightly Review",
                    "description": null,
                    "trigger": "cron",
                    "is_active": false,
                    "is_default": false,
                    "max_retries": 0,
                    "step_count": 0,
                    "metadata": {
                        "schedule": "0 0 * * *",
                        "entry_steps": []
                    },
                    "steps": [],
                    "edges": [],
                    "created_by": null,
                    "created_at": "2026-05-23T00:00:00Z",
                    "updated_at": "2026-05-23T00:00:00Z"
                }),
            ),
        ])
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_resource_id_store(resource_ids.clone())
            .with_cached_org_id(Some(org_id));

        backend
            .configure_routine(RoutineConfigureParams {
                data: RoutineConfigureDocument {
                    routine: Some(Slug::derive("nightly-review")),
                    metadata: Some(RoutineConfigureMetadata {
                        name: Some("Nightly Review".to_string()),
                        description: None,
                        project_id: None,
                        trigger: None,
                        is_active: None,
                        max_retries: None,
                    }),
                    runtime_metadata: Some(json!({
                        "schedule": "0 0 * * *",
                        "entry_steps": []
                    })),
                    cron_task: Some(RoutineCronTaskConfigureDocument {
                        title: "Updated nightly task".to_string(),
                        description: None,
                        acceptance_criteria: None,
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let requests = server.await.unwrap().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        let body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["id"], routine_id.to_string());
        assert_eq!(
            body["encrypted_payload"]["object_id"],
            routine_id.to_string()
        );
        assert_eq!(
            resource_ids
                .get(
                    PlatformResourceKind::Routine,
                    &Slug::derive("nightly-review")
                )
                .unwrap(),
            Some(routine_id)
        );
    }

    #[tokio::test]
    async fn configure_routine_with_uncached_explicit_ref_calls_platform() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let (base_url, server) = spawn_request_sequence_server(vec![
            (
                "GET",
                "/api/v1/routines/brand-design-workflow",
                "404 Not Found",
                json!({ "error": "not found" }),
            ),
            (
                "POST",
                "/api/v1/routines/configure",
                "201 Created",
                json!({
                    "id": routine_id,
                    "org_id": org_id,
                    "project_id": null,
                    "slug": "brand-design-workflow",
                    "name": "Brand Design Workflow",
                    "description": "Updated workflow",
                    "trigger": "task",
                    "is_active": true,
                    "is_default": false,
                    "max_retries": 2,
                    "step_count": 0,
                    "metadata": {
                        "entry_steps": []
                    },
                    "steps": [],
                    "edges": [],
                    "created_by": null,
                    "created_at": "2026-05-23T00:00:00Z",
                    "updated_at": "2026-05-23T00:00:00Z"
                }),
            ),
        ])
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let result = backend
            .configure_routine(RoutineConfigureParams {
                data: RoutineConfigureDocument {
                    routine: Some(Slug::derive("brand-design-workflow")),
                    metadata: Some(RoutineConfigureMetadata {
                        name: Some("Brand Design Workflow".to_string()),
                        description: Some(Some("Updated workflow".to_string())),
                        project_id: None,
                        trigger: Some(RoutineTrigger::Task),
                        is_active: Some(true),
                        max_retries: Some(2),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.routine.summary.slug,
            Slug::derive("brand-design-workflow")
        );
        let requests = server.await.unwrap().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        let body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["routine"], "brand-design-workflow");
        assert_eq!(body["metadata"]["name"], "Brand Design Workflow");
    }

    #[tokio::test]
    async fn configure_agent_with_uncached_explicit_ref_calls_platform() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/agents/configure",
            "200 OK",
            json!({
                "id": agent_id,
                "org_id": org_id,
                "slug": "brand-designer",
                "name": "Brand Designer",
                "description": "Updated agent",
                "color": null,
                "model": null,
                "model_id": null,
                "model_name": null,
                "domains": [],
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "abilities": [],
                "prompt_locked": false,
                "heartbeat": null,
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        backend
            .configure_agent(AgentConfigureParams {
                data: AgentConfigureDocument {
                    agent: Some(Slug::derive("brand-designer")),
                    metadata: Some(AgentConfigureMetadata {
                        name: Some("Brand Designer".to_string()),
                        description: Some(Some("Updated agent".to_string())),
                        color: None,
                        model: None,
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["agent"], "brand-designer");
    }

    #[tokio::test]
    async fn configure_ability_with_uncached_explicit_ref_calls_platform() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let ability_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/abilities/configure",
            "200 OK",
            json!({
                "id": ability_id,
                "org_id": org_id,
                "slug": "brand_review",
                "name": "brand_review",
                "path": "",
                "description": "Updated ability",
                "activation_condition": "",
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        backend
            .configure_ability(AbilityConfigureParams {
                data: AbilityConfigureDocument {
                    ability: Some(Slug::derive("brand_review")),
                    metadata: Some(AbilityConfigureMetadata {
                        name: Some("brand_review".to_string()),
                        path: None,
                        description: Some(Some("Updated ability".to_string())),
                        activation_condition: None,
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["ability"], "brand_review");
    }

    #[tokio::test]
    async fn configure_domain_with_uncached_explicit_ref_calls_platform() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let domain_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/domains/configure",
            "200 OK",
            json!({
                "id": domain_id,
                "org_id": org_id,
                "slug": "brand-domain",
                "name": "Brand Domain",
                "path": "",
                "description": "Updated domain",
                "command": "#brand",
                "platform_scopes": [],
                "abilities": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        backend
            .configure_domain(DomainConfigureParams {
                data: DomainConfigureDocument {
                    domain: Some(Slug::derive("brand-domain")),
                    metadata: Some(DomainConfigureMetadata {
                        name: Some("Brand Domain".to_string()),
                        path: None,
                        description: Some(Some("Updated domain".to_string())),
                        command: None,
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["domain"], "brand-domain");
    }

    #[tokio::test]
    async fn configure_context_block_with_uncached_explicit_ref_calls_platform() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let block_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "POST",
            "/api/v1/context-blocks/configure",
            "200 OK",
            json!({
                "id": block_id,
                "org_id": org_id,
                "slug": "brand-guidance",
                "name": "Brand Guidance",
                "path": "",
                "description": "Updated context",
                "template": "Existing template",
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z"
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let result = backend
            .configure_context_block(ContextBlockConfigureParams {
                data: ContextBlockConfigureDocument {
                    context_block: Some(Slug::derive("brand-guidance")),
                    metadata: Some(ContextBlockConfigureMetadata {
                        name: Some("Brand Guidance".to_string()),
                        path: None,
                        description: Some(Some("Updated context".to_string())),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.context_block.template, "Existing template");
        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["context_block"], "brand-guidance");
    }

    async fn spawn_knowledge_create_server(
        pack_id: Uuid,
        doc_id: Uuid,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let org_id = Uuid::new_v4();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..1 {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/api/v1/knowledge/test-pack/items") => response(
                        "201 Created",
                        json!({
                            "id": doc_id,
                            "org_id": org_id,
                            "pack_id": pack_id,
                            "slug": "ownership-lifetimes-a1b2c3d4",
                            "filename": "ownership-lifetimes.md",
                            "path": "rust/ownership",
                            "title": "Ownership & Lifetimes",
                            "kind": "guide",
                            "summary": "Ownership and lifetime guidance",
                            "tags": ["rust", "ownership"],
                            "content_type": "text/markdown",
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:00:00Z"
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

    async fn spawn_domain_configure_server(
        org_id: Uuid,
        domain_id: Uuid,
    ) -> Result<(
        String,
        tokio::task::JoinHandle<Result<Vec<RecordedRequest>>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let base_url = format!("http://{address}");
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..1 {
                let (mut stream, _) = listener.accept().await?;
                let request = read_request(&mut stream).await?;
                let body = match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/api/v1/domains/configure") => response(
                        "201 Created",
                        json!({
                            "id": domain_id,
                            "org_id": org_id,
                            "slug": "build-domain",
                            "name": "Build Domain",
                            "path": "",
                            "description": null,
                            "command": "#build-domain",
                            "platform_scopes": [],
                            "abilities": [],
                            "mcp_servers": [],
                            "script_tools": [],
                            "source_type": "native",
                            "read_only": false,
                            "metadata": {},
                            "created_by": null,
                            "created_at": "2026-05-23T00:00:00Z",
                            "updated_at": "2026-05-23T00:00:00Z",
                            "encrypted_payload": {
                                "account_id": org_id,
                                "encryption_scope": "org",
                                "object_id": domain_id,
                                "object_type": "manifest.domain.prompt",
                                "algorithm": "AES-256-GCM",
                                "key_version": 1,
                                "nonce": "nonce",
                                "ciphertext": "encrypted-test-payload",
                            }
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

    #[tokio::test]
    async fn configure_domain_preserves_submitted_prompt_for_followup_get() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let domain_id = Uuid::new_v4();
        let (base_url, server) = spawn_domain_configure_server(org_id, domain_id)
            .await
            .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let prompt_config = nenjo::manifest::DomainPromptConfig {
            developer_prompt_addon: Some("Build domain instructions".to_string()),
        };
        let configured = backend
            .configure_domain(DomainConfigureParams {
                data: DomainConfigureDocument {
                    id: None,
                    domain: None,
                    metadata: Some(DomainConfigureMetadata {
                        name: Some("Build Domain".to_string()),
                        path: None,
                        description: None,
                        command: Some("#build-domain".to_string()),
                    }),
                    prompt_config: Some(prompt_config.clone()),
                    encrypted_payload: None,
                    assignments: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(
            configured.domain.prompt_config.developer_prompt_addon,
            prompt_config.developer_prompt_addon
        );

        let fetched = backend
            .get_domain(DomainsGetParams {
                domain: configured.domain.summary.slug.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            fetched.domain.prompt_config.developer_prompt_addon,
            prompt_config.developer_prompt_addon
        );

        let requests = server.await.unwrap().unwrap();
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert!(body.get("prompt_config").is_none());
        assert!(body.get("encrypted_payload").is_some());
    }

    #[tokio::test]
    async fn get_domain_decodes_remote_encrypted_prompt_into_local_cache() {
        let temp = tempdir().unwrap();
        let store = Arc::new(LocalManifestStore::new(temp.path().join("manifests")));
        let org_id = Uuid::new_v4();
        let domain_id = Uuid::new_v4();
        let (base_url, server) = spawn_single_request_server(
            "GET",
            "/api/v1/domains/build-domain",
            "200 OK",
            json!({
                "id": domain_id,
                "org_id": org_id,
                "slug": "build-domain",
                "name": "Build Domain",
                "path": "",
                "description": null,
                "command": "#build-domain",
                "platform_scopes": [],
                "abilities": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_by": null,
                "created_at": "2026-05-23T00:00:00Z",
                "updated_at": "2026-05-23T00:00:00Z",
                "prompt_config": null,
                "encrypted_payload": {
                    "account_id": org_id,
                    "encryption_scope": "org",
                    "object_id": domain_id,
                    "object_type": "manifest.domain.prompt",
                    "algorithm": "AES-256-GCM",
                    "key_version": 1,
                    "nonce": "nonce",
                    "ciphertext": "encrypted-test-payload"
                }
            }),
        )
        .await
        .unwrap();
        let client = PlatformManifestClient::new(base_url, "test").unwrap();
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
            .with_cached_org_id(Some(org_id));

        let fetched = backend
            .get_domain(DomainsGetParams {
                domain: Slug::derive("build-domain"),
            })
            .await
            .unwrap();
        assert_eq!(
            fetched
                .domain
                .prompt_config
                .developer_prompt_addon
                .as_deref(),
            Some("DECODED_DOMAIN_PROMPT_123")
        );

        let cached = backend
            .local_store
            .list_domains()
            .await
            .unwrap()
            .into_iter()
            .any(|domain| {
                domain.prompt_config.developer_prompt_addon.as_deref()
                    == Some("DECODED_DOMAIN_PROMPT_123")
            });
        assert!(cached);
        let _ = server.await.unwrap().unwrap();
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
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
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
            vec![("POST", "/api/v1/knowledge/test-pack/items".to_string())]
        );
        assert!(!requests[0].body.contains("name=\"slug\""));
        assert!(requests[0].body.contains("name=\"file\""));
        assert!(
            requests[0]
                .body
                .contains("filename=\"ownership-lifetimes.md\"")
        );
        assert!(!requests[0].body.contains("# Ownership & Lifetimes"));
        assert!(requests[0].body.contains("encrypted-test-payload"));
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
        let backend = PlatformManifestBackend::new(store, client, TestSensitivePayloadEncoder)
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
                        target_doc: "target".to_string(),
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
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].target_doc, "target");

        let requests = server.await.unwrap().unwrap();
        let expected_paths = [
            "/api/v1/knowledge/test-pack/items".to_string(),
            "/api/v1/knowledge/test-pack/items/guide/content".to_string(),
            "/api/v1/knowledge/test-pack/items/guide".to_string(),
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
                ("PUT", expected_paths[3].clone()),
            ]
        );
        assert!(!requests[1].body.contains("updated content"));
        assert!(requests[1].body.contains("encrypted-test-payload"));
        assert!(requests[2].body.contains("\"filename\":\"guide.md\""));
        assert!(!requests[2].body.contains("updated content"));
        assert!(!requests[2].body.contains("related"));
        assert!(requests[3].body.contains("target"));
        assert!(requests[3].body.contains("references"));
    }

    #[tokio::test]
    async fn local_manifest_org_id_uses_cached_bootstrap_org_id() {
        let (backend, _project_id, _pack_slug, _temp) = library_backend_fixture().await.unwrap();
        let org_id = Uuid::new_v4();
        let backend = backend.with_cached_org_id(Some(org_id));

        assert_eq!(backend.local_manifest_org_id().await.unwrap(), org_id);
    }
}
