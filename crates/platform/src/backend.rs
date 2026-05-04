//! Platform-backed manifest backend implementations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    ManifestResource, ManifestResourceKind, ModelManifest, ProjectManifest, PromptConfig,
    RoutineManifest,
};
use nenjo::{ManifestReader, ManifestWriter};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client::PlatformManifestClient;
use crate::manifest_contract::ManifestKind;
use crate::manifest_mcp::*;
use crate::policy::ManifestAccessPolicy;

fn merge_json_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) => merge_json_patch(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

fn merge_prompt_config(current: &PromptConfig, patch: serde_json::Value) -> Result<PromptConfig> {
    let mut value = serde_json::to_value(current)?;
    merge_json_patch(&mut value, patch);
    Ok(serde_json::from_value(value)?)
}

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
        }
    }

    /// Attach a scope-based access policy used to filter reads and validate writes.
    pub fn with_access_policy(mut self, access_policy: ManifestAccessPolicy) -> Self {
        self.access_policy = Some(access_policy);
        self
    }

    /// Attach the worker workspace root used for local-first project document reads.
    pub fn with_workspace_dir(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    /// Attach the org id cached from worker bootstrap metadata.
    pub fn with_cached_org_id(mut self, org_id: Option<Uuid>) -> Self {
        self.cached_org_id = org_id.filter(|id| !id.is_nil());
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
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(hydrated.clone()))
            .await?;
        Ok(hydrated)
    }

    fn workspace_dir(&self) -> Result<&Path> {
        self.workspace_dir
            .as_deref()
            .ok_or_else(|| anyhow!("project document tools require a configured workspace_dir"))
    }

    async fn project_workspace_dir(&self, project_id: Uuid) -> Result<PathBuf> {
        let project = self
            .local_store
            .get_project(project_id)
            .await?
            .ok_or_else(|| anyhow!("project {project_id} is not cached locally"))?;
        Ok(self.workspace_dir()?.join(project.slug))
    }

    async fn list_local_project_documents(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectKnowledgeDocManifest>> {
        let project_dir = self.project_workspace_dir(project_id).await?;
        // Project document tools read from the worker's local knowledge cache rather than
        // hydrating documents directly from the platform on demand.
        let manifest = load_project_knowledge_manifest(&project_dir).ok_or_else(|| {
            anyhow!("project documents are not cached locally for project {project_id}")
        })?;
        Ok(manifest.docs)
    }

    async fn read_local_project_document(
        &self,
        project_id: Uuid,
        source_path: &str,
        fallback_filename: Option<&str>,
    ) -> Result<String> {
        let project_dir = self.project_workspace_dir(project_id).await?;
        let path = project_dir.join(source_path);
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(content),
            Err(primary_error) => {
                if let Some(filename) = fallback_filename {
                    let fallback_path = project_dir.join("docs").join(filename);
                    if fallback_path != path {
                        return Ok(
                            std::fs::read_to_string(&fallback_path).map_err(|_| primary_error)?
                        );
                    }
                }
                Err(anyhow::Error::from(primary_error)).with_context(|| {
                    format!("failed to read local project document {}", path.display())
                })
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectKnowledgeManifest {
    docs: Vec<ProjectKnowledgeDocManifest>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectKnowledgeDocManifest {
    id: String,
    virtual_path: String,
    source_path: String,
    title: String,
    summary: String,
    description: Option<String>,
    kind: String,
    authority: String,
    status: String,
    tags: Vec<String>,
    aliases: Vec<String>,
    keywords: Vec<String>,
    related: Vec<ProjectKnowledgeDocEdge>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectKnowledgeDocEdge {
    #[serde(rename = "type", alias = "edge_type")]
    edge_type: String,
    target: String,
    description: Option<String>,
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
    target: String,
    edges: Vec<ProjectDocNeighborEdge>,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectDocNeighborEdge {
    edge_type: String,
    source: String,
    target: String,
    note: Option<String>,
}

fn load_project_knowledge_manifest(project_dir: &Path) -> Option<ProjectKnowledgeManifest> {
    let path = project_dir.join("knowledge_manifest.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn project_doc_relative_path(doc: &ProjectKnowledgeDocManifest) -> String {
    doc.virtual_path
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| path.to_string())
        .unwrap_or_else(|| doc.virtual_path.clone())
}

fn project_doc_filename(doc: &ProjectKnowledgeDocManifest) -> String {
    let relative = project_doc_relative_path(doc);
    relative
        .rsplit('/')
        .next()
        .map(ToString::to_string)
        .unwrap_or(relative)
}

fn project_doc_path(doc: &ProjectKnowledgeDocManifest) -> Option<String> {
    project_doc_relative_path(doc)
        .rsplit_once('/')
        .map(|(path, _)| path.to_string())
}

fn normalize_project_lookup(project_id: Uuid, value: &str) -> String {
    let trimmed = value.trim().trim_matches('/').to_string();
    if let Some(stripped) = trimmed.strip_prefix(&format!("project://{project_id}/")) {
        stripped.trim_matches('/').to_string()
    } else {
        trimmed
    }
}

fn project_doc_manifest(doc: &ProjectKnowledgeDocManifest) -> ProjectDocManifest {
    ProjectDocManifest {
        id: doc.id.clone(),
        project_id: doc
            .virtual_path
            .split_once("project://")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(project_id, _)| project_id.to_string())
            .unwrap_or_default(),
        virtual_path: doc.virtual_path.clone(),
        filename: project_doc_filename(doc),
        path: project_doc_path(doc),
        title: doc.title.clone(),
        summary: doc.summary.clone(),
        description: doc.description.clone(),
        kind: doc.kind.clone(),
        authority: doc.authority.clone(),
        status: doc.status.clone(),
        tags: doc.tags.clone(),
    }
}

fn filter_project_docs(
    docs: Vec<ProjectKnowledgeDocManifest>,
    filter: &ProjectDocFilterArgs,
) -> Vec<ProjectKnowledgeDocManifest> {
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
                && !doc.kind.eq_ignore_ascii_case(kind)
            {
                return false;
            }
            if let Some(authority) = filter.authority.as_ref()
                && !doc.authority.eq_ignore_ascii_case(authority)
            {
                return false;
            }
            if let Some(status) = filter.status.as_ref()
                && !doc.status.eq_ignore_ascii_case(status)
            {
                return false;
            }
            if let Some(path_prefix) = filter.path_prefix.as_ref() {
                let prefix = path_prefix.trim_matches('/');
                let relative = project_doc_relative_path(doc);
                if !relative.starts_with(prefix) && !doc.virtual_path.starts_with(path_prefix) {
                    return false;
                }
            }
            true
        })
        .collect()
}

async fn lookup_project_doc<L, E>(
    backend: &PlatformManifestBackend<L, E>,
    project_id: Uuid,
    id_or_path: &str,
) -> Result<ProjectKnowledgeDocManifest>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder,
{
    let docs = backend.list_local_project_documents(project_id).await?;
    let normalized = normalize_project_lookup(project_id, id_or_path);
    docs.into_iter()
        .find(|doc| {
            doc.id == id_or_path
                || doc.virtual_path == id_or_path
                || project_doc_relative_path(doc) == normalized
                || project_doc_filename(doc) == normalized
        })
        .ok_or_else(|| {
            anyhow!(
                "unknown project doc '{}'; use a document id, relative path, filename, or project://{project_id}/ path",
                id_or_path
            )
        })
}

fn project_doc_score(
    doc: &ProjectKnowledgeDocManifest,
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
    let virtual_path = doc.virtual_path.to_lowercase();
    let title = doc.title.to_lowercase();
    let summary = doc.summary.to_lowercase();
    let kind = doc.kind.to_lowercase();
    let authority = doc.authority.to_lowercase();
    let status = doc.status.to_lowercase();
    let tags = doc
        .tags
        .iter()
        .map(|tag| tag.to_lowercase())
        .collect::<Vec<_>>();
    let aliases = doc
        .aliases
        .iter()
        .map(|alias| alias.to_lowercase())
        .collect::<Vec<_>>();
    let keywords = doc
        .keywords
        .iter()
        .map(|keyword| keyword.to_lowercase())
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
    if aliases.iter().any(|alias| alias.contains(&query)) {
        score += 4;
        matched.push("aliases".to_string());
    }
    if keywords.iter().any(|keyword| keyword.contains(&query)) {
        score += 3;
        matched.push("keywords".to_string());
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

async fn project_doc_neighbors<L, E>(
    backend: &PlatformManifestBackend<L, E>,
    project_id: Uuid,
    doc: &ProjectKnowledgeDocManifest,
    edge_type: Option<&str>,
) -> Result<Vec<ProjectDocNeighbor>>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder,
{
    let docs = backend.list_local_project_documents(project_id).await?;
    let docs_by_virtual_path = docs
        .iter()
        .map(|candidate| (candidate.virtual_path.clone(), candidate))
        .collect::<std::collections::HashMap<_, _>>();
    let mut neighbors = std::collections::BTreeMap::<String, ProjectDocNeighbor>::new();

    for edge in &doc.related {
        if let Some(filter) = edge_type
            && !edge.edge_type.eq_ignore_ascii_case(filter)
        {
            continue;
        }
        if let Some(target) = docs_by_virtual_path.get(&edge.target) {
            push_project_neighbor_edge(
                &mut neighbors,
                target.virtual_path.clone(),
                ProjectDocNeighborEdge {
                    edge_type: edge.edge_type.clone(),
                    source: doc.virtual_path.clone(),
                    target: target.virtual_path.clone(),
                    note: edge.description.clone(),
                },
            );
        }
    }

    for candidate in &docs {
        for edge in &candidate.related {
            if edge.target != doc.virtual_path {
                continue;
            }
            if let Some(filter) = edge_type
                && !edge.edge_type.eq_ignore_ascii_case(filter)
            {
                continue;
            }
            push_project_neighbor_edge(
                &mut neighbors,
                candidate.virtual_path.clone(),
                ProjectDocNeighborEdge {
                    edge_type: edge.edge_type.clone(),
                    source: candidate.virtual_path.clone(),
                    target: doc.virtual_path.clone(),
                    note: edge.description.clone(),
                },
            );
        }
    }

    Ok(neighbors.into_values().collect())
}

fn push_project_neighbor_edge(
    neighbors: &mut std::collections::BTreeMap<String, ProjectDocNeighbor>,
    neighbor_target: String,
    edge: ProjectDocNeighborEdge,
) {
    let neighbor = neighbors
        .entry(neighbor_target.clone())
        .or_insert_with(|| ProjectDocNeighbor {
            target: neighbor_target,
            edges: Vec::new(),
        });
    if !neighbor.edges.iter().any(|existing| {
        existing.edge_type == edge.edge_type
            && existing.source == edge.source
            && existing.target == edge.target
            && existing.note == edge.note
    }) {
        neighbor.edges.push(edge);
        neighbor.edges.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| left.edge_type.cmp(&right.edge_type))
                .then_with(|| left.note.cmp(&right.note))
        });
    }
}

async fn search_project_docs<L, E>(
    backend: &PlatformManifestBackend<L, E>,
    project_id: Uuid,
    docs: Vec<ProjectKnowledgeDocManifest>,
    query: &str,
    filter: &ProjectDocFilterArgs,
    include_content: bool,
) -> Result<Vec<ProjectDocSearchHit>>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder,
{
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
            if !neighbors
                .iter()
                .any(|neighbor| neighbor.target == doc.virtual_path)
            {
                continue;
            }
        }

        let content = backend
            .read_local_project_document(
                project_id,
                &doc.source_path,
                Some(&project_doc_filename(&doc)),
            )
            .await
            .ok();

        let Some((score, matched)) = project_doc_score(&doc, query, content.as_deref()) else {
            continue;
        };

        hits.push(ProjectDocSearchHit {
            id: doc.id.clone(),
            virtual_path: doc.virtual_path.clone(),
            title: doc.title.clone(),
            summary: doc.summary.clone(),
            kind: doc.kind.clone(),
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
    docs: &[ProjectKnowledgeDocManifest],
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
                path: doc.virtual_path.clone(),
                title: doc.title.clone(),
                kind: doc.kind.clone(),
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

    async fn list_project_documents(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ProjectDocListArgs =
            serde_json::from_value(params).context("invalid list_project_documents args")?;
        let docs = self.list_local_project_documents(args.project_id).await?;
        let docs = filter_project_docs(docs, &args.filter);
        serde_json::to_value(
            docs.iter()
                .map(project_doc_manifest)
                .collect::<Vec<ProjectDocManifest>>(),
        )
        .map_err(Into::into)
    }

    async fn read_project_document_manifest(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: ProjectDocLookupArgs = serde_json::from_value(params)
            .context("invalid read_project_document_manifest args")?;
        let manifest = lookup_project_doc(self, args.project_id, &args.id_or_path).await?;
        serde_json::to_value(project_doc_manifest(&manifest)).map_err(Into::into)
    }

    async fn read_project_document(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ProjectDocLookupArgs =
            serde_json::from_value(params).context("invalid read_project_document args")?;
        let manifest = lookup_project_doc(self, args.project_id, &args.id_or_path).await?;
        let content = self
            .read_local_project_document(
                args.project_id,
                &manifest.source_path,
                Some(&project_doc_filename(&manifest)),
            )
            .await?;
        serde_json::to_value(ProjectDocRead {
            manifest: project_doc_manifest(&manifest),
            content,
        })
        .map_err(Into::into)
    }

    async fn search_project_documents(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: ProjectDocSearchArgs =
            serde_json::from_value(params).context("invalid search_project_documents args")?;
        let docs = self.list_local_project_documents(args.project_id).await?;
        let hits =
            search_project_docs(self, args.project_id, docs, &args.query, &args.filter, true)
                .await?;
        serde_json::to_value(hits).map_err(Into::into)
    }

    async fn search_project_document_paths(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: ProjectDocSearchArgs =
            serde_json::from_value(params).context("invalid search_project_document_paths args")?;
        let docs = self.list_local_project_documents(args.project_id).await?;
        let hits = search_project_docs(
            self,
            args.project_id,
            docs,
            &args.query,
            &args.filter,
            false,
        )
        .await?;
        serde_json::to_value(hits).map_err(Into::into)
    }

    async fn list_project_document_tree(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: ProjectDocTreeArgs =
            serde_json::from_value(params).context("invalid list_project_document_tree args")?;
        let docs = self.list_local_project_documents(args.project_id).await?;
        serde_json::to_value(project_doc_tree(
            args.project_id,
            &docs,
            args.prefix.as_deref(),
        ))
        .map_err(Into::into)
    }

    async fn list_project_document_neighbors(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let args: ProjectDocNeighborArgs = serde_json::from_value(params)
            .context("invalid list_project_document_neighbors args")?;
        let manifest = lookup_project_doc(self, args.project_id, &args.id_or_path).await?;
        let neighbors =
            project_doc_neighbors(self, args.project_id, &manifest, args.edge_type.as_deref())
                .await?;
        serde_json::to_value(neighbors).map_err(Into::into)
    }

    async fn create_project_document(
        &self,
        params: ProjectDocumentCreateParams,
    ) -> Result<ProjectDocumentMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                Uuid::new_v4(),
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
                &serde_json::Value::String(params.data.description.clone()),
            )
            .await?;
        let project_document = self
            .platform_client
            .create_project_file_document(&params.data, encrypted_payload)
            .await?;
        Ok(ProjectDocumentMutationResult { project_document })
    }

    async fn update_project_document_content(
        &self,
        params: ProjectDocumentContentUpdateParams,
    ) -> Result<ProjectDocumentContentMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_org_id().await?,
                params.document_id,
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
                &serde_json::Value::String(params.description.clone()),
            )
            .await?;
        let project_document = self
            .platform_client
            .update_project_document_content(
                params.project_id,
                params.document_id,
                &params.description,
                encrypted_payload,
            )
            .await?;
        Ok(ProjectDocumentContentMutationResult { project_document })
    }

    async fn delete_project_document(
        &self,
        params: ProjectDocumentDeleteParams,
    ) -> Result<DeleteResult> {
        self.platform_client
            .delete_project_file_document(params.project_id, params.document_id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.document_id,
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

    async fn project_backend_fixture() -> Result<(
        PlatformManifestBackend<LocalManifestStore, NoopSensitivePayloadEncoder>,
        Uuid,
        TempDir,
    )> {
        let temp = tempdir()?;
        let manifests_dir = temp.path().join("manifests");
        let workspace_dir = temp.path().join("workspace");
        let project_id = Uuid::new_v4();
        let project_slug = "graph-eval";
        let project_dir = workspace_dir.join(project_slug);
        std::fs::create_dir_all(project_dir.join("docs"))?;

        let store = Arc::new(LocalManifestStore::new(manifests_dir));
        store
            .upsert_resource(&ManifestResource::Project(ProjectManifest {
                id: project_id,
                name: "Graph Eval".to_string(),
                slug: project_slug.to_string(),
                description: None,
                settings: json!({}),
            }))
            .await?;

        let overview_path = format!("project://{project_id}/docs/overview.md");
        let routine_path = format!("project://{project_id}/docs/routine.md");
        let gate_path = format!("project://{project_id}/docs/gate.md");
        let unrelated_path = format!("project://{project_id}/docs/unrelated.md");
        let manifest = json!({
            "pack_id": format!("project-{project_id}"),
            "pack_version": "1",
            "schema_version": 1,
            "root_uri": format!("project://{project_id}/"),
            "synced_at": "2026-01-01T00:00:00Z",
            "docs": [
                {
                    "id": "overview",
                    "virtual_path": overview_path,
                    "source_path": "docs/overview.md",
                    "title": "Overview",
                    "summary": "Project overview",
                    "description": null,
                    "kind": "guide",
                    "authority": "canonical",
                    "status": "stable",
                    "tags": ["domain:project"],
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
            project_dir.join("knowledge_manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        for filename in ["overview.md", "routine.md", "gate.md", "unrelated.md"] {
            std::fs::write(
                project_dir.join("docs").join(filename),
                format!("# {filename}\n"),
            )?;
        }

        let client = PlatformManifestClient::new("http://localhost:9", "test")?;
        let backend = PlatformManifestBackend::new(store, client, NoopSensitivePayloadEncoder)
            .with_workspace_dir(workspace_dir);

        Ok((backend, project_id, temp))
    }

    #[tokio::test]
    async fn project_document_neighbors_expose_outgoing_and_incoming_edges() {
        let (backend, project_id, _temp) = project_backend_fixture().await.unwrap();
        let routine_path = format!("project://{project_id}/docs/routine.md");
        let overview_path = format!("project://{project_id}/docs/overview.md");
        let gate_path = format!("project://{project_id}/docs/gate.md");

        let value = backend
            .list_project_document_neighbors(json!({
                "project_id": project_id,
                "id_or_path": "routine"
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
            .list_project_document_neighbors(json!({
                "project_id": project_id,
                "id_or_path": routine_path,
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
        let (backend, _project_id, _temp) = project_backend_fixture().await.unwrap();
        let org_id = Uuid::new_v4();
        let backend = backend.with_cached_org_id(Some(org_id));

        assert_eq!(backend.local_manifest_org_id().await.unwrap(), org_id);
    }
}
