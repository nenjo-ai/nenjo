//! Document sync — download project documents to the local workspace.
//!
//! At bootstrap, fetches document metadata for each project via the v1 API,
//! diffs against the local project `knowledge_manifest.json`, and downloads new/changed docs.
//! Deleted docs are removed locally. Network errors are soft-fail (logged);
//! filesystem errors are hard-fail.

use anyhow::{Context, Result};
use nenjo::knowledge::{
    KnowledgeDocAuthority, KnowledgeDocEdge, KnowledgeDocEdgeType, KnowledgeDocKind,
    KnowledgeDocManifest, KnowledgeDocStatus, KnowledgePackManifest,
};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use crate::harness::api_client::{DocumentSyncEdge, DocumentSyncMeta, NenjoClient};

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Result of comparing remote docs against the local manifest.
#[derive(Debug)]
pub struct SyncDiff {
    /// Documents to download (new or updated).
    pub to_download: Vec<DocumentSyncMeta>,
    /// Local files to rename when only the filename changed.
    pub to_rename: Vec<FileRename>,
    /// Local filenames to delete (no longer present remotely).
    pub to_delete: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    pub from: String,
    pub to: String,
}

fn document_relative_path(doc: &DocumentSyncMeta) -> String {
    match doc.path.as_deref().map(|path| path.trim_matches('/')) {
        Some(path) if !path.is_empty() => format!("{path}/{}", doc.filename),
        _ => doc.filename.clone(),
    }
}

fn knowledge_doc_relative_path(doc: &KnowledgeDocManifest) -> String {
    doc.source_path
        .strip_prefix("docs/")
        .unwrap_or(&doc.source_path)
        .trim_matches('/')
        .to_string()
}

fn knowledge_doc_id(doc: &KnowledgeDocManifest) -> Option<Uuid> {
    Uuid::parse_str(&doc.id).ok()
}

/// Compare remote document list against a local manifest.
///
/// A document is considered changed if its `updated_at` timestamp differs.
pub fn compute_diff(
    manifest: Option<&ProjectKnowledgePackManifest>,
    remote: &[DocumentSyncMeta],
) -> SyncDiff {
    let local_map: HashMap<Uuid, &KnowledgeDocManifest> = manifest
        .map(|m| {
            m.docs
                .iter()
                .filter_map(|doc| knowledge_doc_id(doc).map(|id| (id, doc)))
                .collect()
        })
        .unwrap_or_default();

    let remote_ids: std::collections::HashSet<Uuid> = remote.iter().map(|d| d.id).collect();

    let to_download: Vec<DocumentSyncMeta> = remote
        .iter()
        .filter(|doc| {
            match local_map.get(&doc.id) {
                Some(entry) => entry.updated_at != doc.updated_at,
                None => true, // new document
            }
        })
        .cloned()
        .collect();

    let rename_download_ids: std::collections::HashSet<Uuid> = remote
        .iter()
        .filter_map(|doc| {
            local_map
                .get(&doc.id)
                .filter(|entry| {
                    knowledge_doc_relative_path(entry) != document_relative_path(doc)
                        && entry.updated_at != doc.updated_at
                })
                .map(|_| doc.id)
        })
        .collect();

    let to_rename: Vec<FileRename> = remote
        .iter()
        .filter_map(|doc| {
            let entry = local_map.get(&doc.id)?;
            let from = knowledge_doc_relative_path(entry);
            let to = document_relative_path(doc);
            if from == to || rename_download_ids.contains(&doc.id) {
                return None;
            }
            Some(FileRename { from, to })
        })
        .collect();

    let to_delete: Vec<String> = manifest
        .map(|m| {
            m.docs
                .iter()
                .filter_map(|entry| {
                    let id = knowledge_doc_id(entry)?;
                    (!remote_ids.contains(&id) || rename_download_ids.contains(&id))
                        .then(|| knowledge_doc_relative_path(entry))
                })
                .collect()
        })
        .unwrap_or_default();

    SyncDiff {
        to_download,
        to_rename,
        to_delete,
    }
}

// ---------------------------------------------------------------------------
// Manifest I/O
// ---------------------------------------------------------------------------

const KNOWLEDGE_MANIFEST_FILENAME: &str = "knowledge_manifest.json";

/// Local project knowledge manifest stored as `knowledge_manifest.json`.
///
/// This is the single source of truth for project document sync state and
/// knowledge metadata. Do not add a second project document manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledgePackManifest {
    pub pack_id: String,
    pub pack_version: String,
    pub schema_version: u32,
    pub root_uri: String,
    pub synced_at: String,
    pub docs: Vec<KnowledgeDocManifest>,
}

impl KnowledgePackManifest for ProjectKnowledgePackManifest {
    fn pack_id(&self) -> &str {
        &self.pack_id
    }

    fn pack_version(&self) -> &str {
        &self.pack_version
    }

    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn root_uri(&self) -> &str {
        &self.root_uri
    }

    fn content_hash(&self) -> &str {
        ""
    }

    fn docs(&self) -> &[KnowledgeDocManifest] {
        &self.docs
    }
}

impl ProjectKnowledgePackManifest {
    fn new(project_id: Uuid) -> Self {
        Self {
            pack_id: format!("project-{project_id}"),
            pack_version: "1".to_string(),
            schema_version: 1,
            root_uri: format!("project://{project_id}/"),
            synced_at: chrono::Utc::now().to_rfc3339(),
            docs: Vec::new(),
        }
    }

    fn touch(&mut self) {
        self.synced_at = chrono::Utc::now().to_rfc3339();
    }

    fn remove_document(&mut self, document_id: Uuid) -> bool {
        let doc_id = document_id.to_string();
        let removed_virtual_paths: std::collections::HashSet<String> = self
            .docs
            .iter()
            .filter(|doc| doc.id == doc_id)
            .map(|doc| doc.virtual_path.clone())
            .collect();
        let original_len = self.docs.len();
        self.docs.retain(|doc| doc.id != doc_id);
        if self.docs.len() == original_len {
            return false;
        }
        for doc in &mut self.docs {
            doc.related
                .retain(|edge| !removed_virtual_paths.contains(&edge.target));
        }
        self.touch();
        true
    }

    fn doc_by_id(&self, document_id: Uuid) -> Option<&KnowledgeDocManifest> {
        let doc_id = document_id.to_string();
        self.docs.iter().find(|doc| doc.id == doc_id)
    }

    fn upsert_from_sync_meta(
        &mut self,
        project_id: Uuid,
        metadata: &DocumentSyncMeta,
        edges: &[DocumentSyncEdge],
    ) {
        let virtual_path = project_doc_virtual_path(project_id, metadata);
        let next = project_knowledge_doc(project_id, metadata, edges, |target_id| {
            self.docs
                .iter()
                .find(|doc| doc.id == target_id.to_string())
                .map(|doc| doc.virtual_path.clone())
        });
        if let Some(pos) = self.docs.iter().position(|doc| doc.id == next.id) {
            self.docs[pos] = next;
        } else {
            self.docs.push(next);
        }
        for doc in &mut self.docs {
            doc.related.retain(|edge| edge.target != virtual_path);
        }
        for edge in edges {
            if edge.target_document_id == metadata.id
                && let Some(source) = self
                    .docs
                    .iter_mut()
                    .find(|doc| doc.id == edge.source_document_id.to_string())
            {
                let target = virtual_path.clone();
                if !source.related.iter().any(|existing| {
                    existing.edge_type.as_str() == edge.edge_type && existing.target == target
                }) {
                    source.related.push(KnowledgeDocEdge {
                        edge_type: parse_doc_edge_type(&edge.edge_type),
                        target,
                        description: edge.note.clone(),
                    });
                }
            }
        }
        self.docs
            .sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
        self.touch();
    }
}

pub fn load_project_knowledge_manifest(project_dir: &Path) -> Option<ProjectKnowledgePackManifest> {
    let path = project_dir.join(KNOWLEDGE_MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_project_knowledge_manifest(
    project_dir: &Path,
    manifest: &ProjectKnowledgePackManifest,
) -> Result<()> {
    let target = project_dir.join(KNOWLEDGE_MANIFEST_FILENAME);
    let tmp = project_dir.join(format!(".{KNOWLEDGE_MANIFEST_FILENAME}.tmp"));
    let json = serde_json::to_string_pretty(manifest)
        .context("Failed to serialize project knowledge manifest")?;
    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Sync documents for all projects.
///
/// Network/API errors are logged and skipped (soft-fail).
/// Filesystem errors are propagated (hard-fail).
pub async fn sync_all(
    api: &NenjoClient,
    workspace_dir: &Path,
    state_dir: &Path,
    projects: &[nenjo::manifest::ProjectManifest],
) -> Result<()> {
    for project in projects {
        let project_dir = workspace_dir.join(&project.slug);
        std::fs::create_dir_all(&project_dir).with_context(|| {
            format!(
                "Failed to create project directory: {}",
                project_dir.display()
            )
        })?;

        if let Err(e) = sync_project(api, &project_dir, project.id, state_dir).await {
            warn!(
                project_id = %project.id,
                error = %e,
                "Document sync failed for project — continuing"
            );
        }
    }

    Ok(())
}

/// Sync documents for a single project.
pub async fn sync_project(
    api: &NenjoClient,
    project_dir: &Path,
    project_id: Uuid,
    state_dir: &Path,
) -> Result<()> {
    // Fetch remote doc list — soft-fail on network errors
    let remote_docs = match api.list_project_documents(project_id).await {
        Ok(docs) => docs,
        Err(e) => {
            warn!(
                project_id = %project_id,
                error = %e,
                "Failed to list project documents — skipping sync"
            );
            return Ok(());
        }
    };

    let mut edges_by_doc: HashMap<Uuid, Vec<DocumentSyncEdge>> = HashMap::new();
    for doc in &remote_docs {
        match api.list_project_document_edges(project_id, doc.id).await {
            Ok(edges) => {
                edges_by_doc.insert(doc.id, edges);
            }
            Err(e) => {
                warn!(
                    project_id = %project_id,
                    doc_id = %doc.id,
                    error = %e,
                    "Failed to list project document edges — continuing with empty edge set"
                );
                edges_by_doc.insert(doc.id, Vec::new());
            }
        }
    }

    let manifest = load_project_knowledge_manifest(project_dir);
    let diff = compute_diff(manifest.as_ref(), &remote_docs);

    if diff.to_download.is_empty() && diff.to_delete.is_empty() && diff.to_rename.is_empty() {
        debug!(project_id = %project_id, "Documents up to date");
        return Ok(());
    }

    info!(
        project_id = %project_id,
        downloads = diff.to_download.len(),
        renames = diff.to_rename.len(),
        deletes = diff.to_delete.len(),
        "Syncing project documents"
    );

    // Track which docs were successfully downloaded
    let mut failed_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    // Ensure docs directory exists
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;

    // Download new/changed documents
    for doc in &diff.to_download {
        match api.get_document_content(project_id, doc.id).await {
            Ok(response) => {
                let content = resolve_document_content(
                    state_dir,
                    &response.encrypted_payload,
                    response.content,
                )
                .await
                .with_context(|| format!("Failed to resolve content for document {}", doc.id))?;

                write_document_content(project_dir, &document_relative_path(doc), &content)?;
                info!(doc_id = %doc.id, filename = %doc.filename, "Downloaded document");
            }
            Err(e) => {
                warn!(
                    doc_id = %doc.id,
                    filename = %doc.filename,
                    error = %e,
                    "Failed to download document — skipping"
                );
                failed_ids.insert(doc.id);
            }
        }
    }

    for rename in &diff.to_rename {
        rename_document_file(project_dir, &rename.from, &rename.to)?;
        debug!(from = %rename.from, to = %rename.to, "Renamed document");
    }

    // Delete removed documents
    for relative_path in &diff.to_delete {
        let path = docs_dir.join(relative_path);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete {}", path.display()))?;
            debug!(path = %relative_path, "Deleted removed document");
        }
    }

    let synced_docs = remote_docs
        .iter()
        .filter(|doc| !failed_ids.contains(&doc.id))
        .cloned()
        .collect::<Vec<_>>();
    write_project_knowledge_manifest(
        project_dir,
        &build_project_knowledge_manifest(project_id, &synced_docs, &edges_by_doc),
    )?;

    Ok(())
}

pub fn write_document_content(
    project_dir: &Path,
    relative_path: &str,
    content: &str,
) -> Result<()> {
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;

    let target = docs_dir.join(relative_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create docs dir: {}", parent.display()))?;
    }
    let tmp = target.with_file_name(format!(
        ".{}.tmp",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
    ));

    std::fs::write(&tmp, content.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    remove_empty_directory_target(&target)?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
}

fn remove_empty_directory_target(target: &Path) -> Result<()> {
    if !target.is_dir() {
        return Ok(());
    }

    let mut entries = std::fs::read_dir(target)
        .with_context(|| format!("Failed to inspect target directory {}", target.display()))?;
    if entries.next().is_some() {
        anyhow::bail!(
            "Cannot replace non-empty directory {} with document file",
            target.display()
        );
    }
    std::fs::remove_dir(target)
        .with_context(|| format!("Failed to remove stale directory {}", target.display()))
}

pub fn delete_document_file(project_dir: &Path, relative_path: &str) -> Result<()> {
    let path = project_dir.join("docs").join(relative_path);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete {}", path.display()))?;
    }
    Ok(())
}

pub fn rename_document_file(project_dir: &Path, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;
    let from_path = docs_dir.join(from);
    if !from_path.exists() {
        return Ok(());
    }
    let to_path = docs_dir.join(to);
    if let Some(parent) = to_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create docs dir: {}", parent.display()))?;
    }
    remove_empty_directory_target(&to_path)?;
    std::fs::rename(&from_path, &to_path).with_context(|| {
        format!(
            "Failed to rename {} → {}",
            from_path.display(),
            to_path.display()
        )
    })?;
    Ok(())
}

fn reconcile_document_file_location(project_dir: &Path, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }

    let docs_dir = project_dir.join("docs");
    let from_path = docs_dir.join(from);
    if !from_path.exists() {
        return Ok(());
    }

    let to_path = docs_dir.join(to);
    if to_path.exists() {
        std::fs::remove_file(&from_path)
            .with_context(|| format!("Failed to delete stale {}", from_path.display()))?;
        return Ok(());
    }

    rename_document_file(project_dir, from, to)
}

pub fn remove_project_knowledge_entry(project_dir: &Path, document_id: Uuid) -> Result<()> {
    let Some(mut manifest) = load_project_knowledge_manifest(project_dir) else {
        return Ok(());
    };
    if manifest.remove_document(document_id) {
        write_project_knowledge_manifest(project_dir, &manifest)?;
    }
    Ok(())
}

pub fn project_knowledge_document_relative_path(
    project_dir: &Path,
    document_id: Uuid,
) -> Option<String> {
    load_project_knowledge_manifest(project_dir).and_then(|manifest| {
        manifest
            .doc_by_id(document_id)
            .map(knowledge_doc_relative_path)
    })
}

pub async fn sync_document(
    api: &NenjoClient,
    project_dir: &Path,
    project_id: Uuid,
    document_id: Uuid,
    state_dir: &Path,
    metadata: Option<&DocumentSyncMeta>,
) -> Result<()> {
    let response = api.get_document_content(project_id, document_id).await?;
    let content =
        resolve_document_content(state_dir, &response.encrypted_payload, response.content).await?;

    let resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_project_documents(project_id)
            .await?
            .into_iter()
            .find(|doc| doc.id == document_id)
            .ok_or_else(|| {
                anyhow::anyhow!("project document not found in metadata list: {document_id}")
            })?
    };
    write_document_content(
        project_dir,
        &document_relative_path(&resolved_meta),
        &content,
    )?;

    let edges = api
        .list_project_document_edges(project_id, document_id)
        .await
        .unwrap_or_default();

    let mut resolved_meta = resolved_meta;
    resolved_meta.size_bytes = resolved_meta.size_bytes.max(response.size_bytes);
    upsert_project_knowledge_entry(project_dir, project_id, &resolved_meta, &edges)?;
    Ok(())
}

pub async fn sync_document_metadata(
    api: &NenjoClient,
    project_dir: &Path,
    project_id: Uuid,
    document_id: Uuid,
    metadata: Option<&DocumentSyncMeta>,
) -> Result<()> {
    let resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_project_documents(project_id)
            .await?
            .into_iter()
            .find(|doc| doc.id == document_id)
            .ok_or_else(|| {
                anyhow::anyhow!("project document not found in metadata list: {document_id}")
            })?
    };

    let edges = api
        .list_project_document_edges(project_id, document_id)
        .await
        .unwrap_or_default();

    if let Some(existing) = project_knowledge_document_relative_path(project_dir, document_id) {
        reconcile_document_file_location(
            project_dir,
            &existing,
            &document_relative_path(&resolved_meta),
        )?;
    }

    upsert_project_knowledge_entry(project_dir, project_id, &resolved_meta, &edges)?;
    Ok(())
}

fn upsert_project_knowledge_entry(
    project_dir: &Path,
    project_id: Uuid,
    metadata: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
) -> Result<()> {
    let mut manifest = load_project_knowledge_manifest(project_dir)
        .unwrap_or_else(|| empty_knowledge_manifest(project_id));
    manifest.upsert_from_sync_meta(project_id, metadata, edges);
    write_project_knowledge_manifest(project_dir, &manifest)
}

fn build_project_knowledge_manifest(
    project_id: Uuid,
    docs: &[DocumentSyncMeta],
    edges_by_doc: &HashMap<Uuid, Vec<DocumentSyncEdge>>,
) -> ProjectKnowledgePackManifest {
    let virtual_paths_by_id: HashMap<Uuid, String> = docs
        .iter()
        .map(|doc| (doc.id, project_doc_virtual_path(project_id, doc)))
        .collect();
    let mut entries = docs
        .iter()
        .map(|doc| {
            let edges = edges_by_doc.get(&doc.id).map(Vec::as_slice).unwrap_or(&[]);
            project_knowledge_doc(project_id, doc, edges, |target_id| {
                virtual_paths_by_id.get(&target_id).cloned()
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
    ProjectKnowledgePackManifest {
        pack_id: format!("project-{project_id}"),
        pack_version: "1".to_string(),
        schema_version: 1,
        root_uri: format!("project://{project_id}/"),
        synced_at: chrono::Utc::now().to_rfc3339(),
        docs: entries,
    }
}

fn empty_knowledge_manifest(project_id: Uuid) -> ProjectKnowledgePackManifest {
    ProjectKnowledgePackManifest::new(project_id)
}

fn project_knowledge_doc(
    project_id: Uuid,
    doc: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
    resolve_target: impl Fn(Uuid) -> Option<String>,
) -> KnowledgeDocManifest {
    let relative_path = project_doc_relative_path(doc);
    KnowledgeDocManifest {
        id: doc.id.to_string(),
        virtual_path: project_doc_virtual_path(project_id, doc),
        source_path: format!("docs/{relative_path}"),
        title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
        summary: doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Project document {relative_path}")),
        description: None,
        kind: parse_doc_kind(doc.kind.as_deref()),
        authority: parse_doc_authority(doc.authority.as_deref()),
        status: parse_doc_status(doc.status.as_deref()),
        tags: doc.tags.clone(),
        aliases: doc.aliases.clone(),
        keywords: doc.keywords.clone(),
        related: edges
            .iter()
            .filter(|edge| edge.source_document_id == doc.id)
            .filter_map(|edge| {
                resolve_target(edge.target_document_id).map(|target| KnowledgeDocEdge {
                    edge_type: parse_doc_edge_type(&edge.edge_type),
                    target,
                    description: edge.note.clone(),
                })
            })
            .collect(),
        size_bytes: doc.size_bytes,
        updated_at: doc.updated_at.clone(),
    }
}

fn project_doc_virtual_path(project_id: Uuid, doc: &DocumentSyncMeta) -> String {
    let relative = project_doc_relative_path(doc);
    format!("project://{project_id}/{relative}")
}

fn project_doc_relative_path(doc: &DocumentSyncMeta) -> String {
    let mut path = doc.path.clone().unwrap_or_default();
    path = path.trim_matches('/').to_string();
    if path.is_empty() {
        doc.filename.clone()
    } else {
        format!("{path}/{}", doc.filename)
    }
}

fn parse_doc_kind(value: Option<&str>) -> KnowledgeDocKind {
    match value.unwrap_or("reference").trim() {
        "guide" => KnowledgeDocKind::Guide,
        "taxonomy" => KnowledgeDocKind::Taxonomy,
        "domain" => KnowledgeDocKind::Domain,
        "entity" => KnowledgeDocKind::Entity,
        "policy" => KnowledgeDocKind::Policy,
        _ => KnowledgeDocKind::Reference,
    }
}

fn parse_doc_authority(value: Option<&str>) -> KnowledgeDocAuthority {
    match value.unwrap_or("reference").trim() {
        "canonical" => KnowledgeDocAuthority::Canonical,
        "supporting" => KnowledgeDocAuthority::Supporting,
        "pattern" => KnowledgeDocAuthority::Pattern,
        "advisory" => KnowledgeDocAuthority::Advisory,
        "example" => KnowledgeDocAuthority::Example,
        "draft" => KnowledgeDocAuthority::Draft,
        "deprecated" => KnowledgeDocAuthority::Deprecated,
        _ => KnowledgeDocAuthority::Reference,
    }
}

fn parse_doc_status(value: Option<&str>) -> KnowledgeDocStatus {
    match value.unwrap_or("stable").trim() {
        "draft" => KnowledgeDocStatus::Draft,
        "deprecated" => KnowledgeDocStatus::Deprecated,
        _ => KnowledgeDocStatus::Stable,
    }
}

fn parse_doc_edge_type(value: &str) -> KnowledgeDocEdgeType {
    match value.trim() {
        "part_of" => KnowledgeDocEdgeType::PartOf,
        "defines" => KnowledgeDocEdgeType::Defines,
        "governs" => KnowledgeDocEdgeType::Governs,
        "classifies" => KnowledgeDocEdgeType::Classifies,
        "depends_on" => KnowledgeDocEdgeType::DependsOn,
        "extends" => KnowledgeDocEdgeType::Extends,
        "related_to" => KnowledgeDocEdgeType::RelatedTo,
        _ => KnowledgeDocEdgeType::References,
    }
}

pub async fn resolve_document_content(
    state_dir: &Path,
    encrypted_payload: &Option<EncryptedPayload>,
    content: Option<String>,
) -> Result<String> {
    if let Some(content) = content {
        return Ok(content);
    }

    let encrypted_payload = encrypted_payload
        .as_ref()
        .context("document content response did not include content or encrypted_payload")?;

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("failed to initialize worker auth provider")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, encrypted_payload).await?;
    let value: serde_json::Value = serde_json::from_str(&plaintext)
        .context("decrypted document content was not valid JSON")?;
    value
        .as_str()
        .map(str::to_string)
        .context("decrypted document content payload was not a string")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn meta(id: u128, filename: &str, updated: &str, size: i64) -> DocumentSyncMeta {
        DocumentSyncMeta {
            id: Uuid::from_u128(id),
            filename: filename.into(),
            path: None,
            title: None,
            kind: None,
            authority: None,
            summary: None,
            status: None,
            tags: Vec::new(),
            aliases: Vec::new(),
            keywords: Vec::new(),
            content_type: "text/markdown".into(),
            size_bytes: size,
            updated_at: updated.into(),
        }
    }

    fn manifest(docs: Vec<DocumentSyncMeta>) -> ProjectKnowledgePackManifest {
        build_project_knowledge_manifest(Uuid::nil(), &docs, &Default::default())
    }

    #[test]
    fn diff_no_manifest_downloads_all() {
        let remote = vec![
            meta(1, "arch.md", "2026-01-01", 100),
            meta(2, "req.md", "2026-01-01", 200),
        ];
        let diff = compute_diff(None, &remote);
        assert_eq!(diff.to_download.len(), 2);
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_unchanged_skips() {
        let manifest = manifest(vec![meta(1, "arch.md", "2026-01-01", 100)]);
        let remote = vec![meta(1, "arch.md", "2026-01-01", 100)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_updated_downloads() {
        let manifest = manifest(vec![meta(1, "arch.md", "2026-01-01", 100)]);
        let remote = vec![meta(1, "arch.md", "2026-02-01", 150)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_deleted_remotely() {
        let manifest = manifest(vec![
            meta(1, "arch.md", "2026-01-01", 100),
            meta(2, "old.md", "2026-01-01", 50),
        ]);
        let remote = vec![meta(1, "arch.md", "2026-01-01", 100)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_new_and_deleted() {
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01", 100)]);
        let remote = vec![meta(2, "new.md", "2026-02-01", 200)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_download[0].filename, "new.md");
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_renamed_file_without_content_change_renames_only() {
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01", 100)]);
        let remote = vec![meta(1, "new.md", "2026-01-01", 100)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert_eq!(
            diff.to_rename,
            vec![FileRename {
                from: "old.md".into(),
                to: "new.md".into(),
            }]
        );
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_renamed_and_updated_file_redownloads_and_deletes_old_name() {
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01", 100)]);
        let remote = vec![meta(1, "new.md", "2026-02-01", 150)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn write_document_content_replaces_empty_stale_directory_target() {
        let dir = tempdir().unwrap();
        let stale_dir = dir.path().join("docs/domain/random.md");
        std::fs::create_dir_all(&stale_dir).unwrap();

        write_document_content(dir.path(), "domain/random.md", "content").unwrap();

        let target = dir.path().join("docs/domain/random.md");
        assert!(target.is_file());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "content");
    }

    #[test]
    fn write_document_content_rejects_non_empty_stale_directory_target() {
        let dir = tempdir().unwrap();
        let stale_dir = dir.path().join("docs/domain/random.md");
        std::fs::create_dir_all(&stale_dir).unwrap();
        std::fs::write(stale_dir.join("nested.md"), "nested").unwrap();

        let error = write_document_content(dir.path(), "domain/random.md", "content")
            .expect_err("non-empty directory target should not be replaced");

        assert!(
            error
                .to_string()
                .contains("Cannot replace non-empty directory")
        );
    }

    #[test]
    fn rename_document_file_replaces_empty_stale_directory_target() {
        let dir = tempdir().unwrap();
        let docs_dir = dir.path().join("docs/domain");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("random.tmp"), "content").unwrap();
        std::fs::create_dir_all(docs_dir.join("random.md")).unwrap();

        rename_document_file(dir.path(), "domain/random.tmp", "domain/random.md").unwrap();

        let target = docs_dir.join("random.md");
        assert!(target.is_file());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "content");
    }

    #[test]
    fn project_knowledge_manifest_does_not_derive_aliases() {
        let project_id = Uuid::from_u128(7);
        let mut doc = meta(1, "random.md", "2026-02-22", 512);
        doc.path = Some("domain/path".into());
        doc.title = Some("Random".into());
        doc.summary = Some("Just a test document".into());
        doc.aliases = vec!["Random concept".into()];
        doc.keywords = vec!["randomness".into()];

        let manifest = build_project_knowledge_manifest(project_id, &[doc], &Default::default());

        assert_eq!(manifest.docs.len(), 1);
        assert_eq!(manifest.docs[0].aliases, ["Random concept"]);
        assert_eq!(manifest.docs[0].keywords, ["randomness"]);
        assert_eq!(manifest.docs[0].summary, "Just a test document");
        assert_eq!(
            manifest.docs[0].virtual_path,
            format!("project://{project_id}/domain/path/random.md")
        );
    }

    #[test]
    fn project_document_metadata_persists_to_knowledge_manifest() {
        let dir = tempdir().unwrap();
        let project_id = Uuid::from_u128(7);
        let mut doc = meta(1, "random.md", "2026-02-22", 512);
        doc.path = Some("domain".into());
        doc.title = Some("Random".into());
        doc.kind = Some("guide".into());
        doc.authority = Some("draft".into());
        doc.summary = Some("Just a test document".into());
        doc.status = Some("draft".into());
        doc.tags = vec!["project".into()];
        doc.aliases = vec!["Random concept".into()];
        doc.keywords = vec!["randomness".into()];

        upsert_project_knowledge_entry(dir.path(), project_id, &doc, &[]).unwrap();

        let knowledge = load_project_knowledge_manifest(dir.path()).unwrap();
        assert_eq!(knowledge.docs.len(), 1);
        assert_eq!(knowledge.docs[0].title, "Random");
        assert_eq!(knowledge.docs[0].summary, "Just a test document");
        assert_eq!(knowledge.docs[0].aliases, ["Random concept"]);
        assert_eq!(knowledge.docs[0].keywords, ["randomness"]);
        assert_eq!(knowledge.docs[0].size_bytes, 512);
        assert_eq!(knowledge.docs[0].updated_at, "2026-02-22");
    }

    #[test]
    fn reconcile_document_file_location_removes_stale_old_path_when_new_path_exists() {
        let dir = tempdir().unwrap();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(docs_dir.join("guides")).unwrap();
        std::fs::write(docs_dir.join("old.md"), "old").unwrap();
        std::fs::write(docs_dir.join("guides/new.md"), "new").unwrap();

        reconcile_document_file_location(dir.path(), "old.md", "guides/new.md").unwrap();

        assert!(!docs_dir.join("old.md").exists());
        assert_eq!(
            std::fs::read_to_string(docs_dir.join("guides/new.md")).unwrap(),
            "new"
        );
    }
}
