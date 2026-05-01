//! Document sync — download project documents to the local workspace.
//!
//! At bootstrap, fetches document metadata for each project via the v1 API,
//! diffs against a local `_manifest.json`, and downloads new/changed docs.
//! Deleted docs are removed locally. Network errors are soft-fail (logged);
//! filesystem errors are hard-fail.

use anyhow::{Context, Result};
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
// Manifest
// ---------------------------------------------------------------------------

/// On-disk manifest tracking which documents have been synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentManifest {
    pub project_id: Uuid,
    pub synced_at: String,
    pub documents: Vec<ManifestEntry>,
}

/// A single document entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub id: Uuid,
    pub filename: String,
    pub path: Option<String>,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub authority: Option<String>,
    pub summary: Option<String>,
    pub status: Option<String>,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    pub updated_at: String,
}

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

fn manifest_entry_relative_path(entry: &ManifestEntry) -> String {
    match entry.path.as_deref().map(|path| path.trim_matches('/')) {
        Some(path) if !path.is_empty() => format!("{path}/{}", entry.filename),
        _ => entry.filename.clone(),
    }
}

fn document_relative_path(doc: &DocumentSyncMeta) -> String {
    match doc.path.as_deref().map(|path| path.trim_matches('/')) {
        Some(path) if !path.is_empty() => format!("{path}/{}", doc.filename),
        _ => doc.filename.clone(),
    }
}

/// Compare remote document list against a local manifest.
///
/// A document is considered changed if its `updated_at` timestamp differs.
pub fn compute_diff(manifest: Option<&DocumentManifest>, remote: &[DocumentSyncMeta]) -> SyncDiff {
    let local_map: HashMap<Uuid, &ManifestEntry> = manifest
        .map(|m| m.documents.iter().map(|e| (e.id, e)).collect())
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
                    manifest_entry_relative_path(entry) != document_relative_path(doc)
                        && entry.updated_at != doc.updated_at
                })
                .map(|_| doc.id)
        })
        .collect();

    let to_rename: Vec<FileRename> = remote
        .iter()
        .filter_map(|doc| {
            let entry = local_map.get(&doc.id)?;
            let from = manifest_entry_relative_path(entry);
            let to = document_relative_path(doc);
            if from == to || rename_download_ids.contains(&doc.id) {
                return None;
            }
            Some(FileRename { from, to })
        })
        .collect();

    let to_delete: Vec<String> = manifest
        .map(|m| {
            m.documents
                .iter()
                .filter(|e| !remote_ids.contains(&e.id) || rename_download_ids.contains(&e.id))
                .map(manifest_entry_relative_path)
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

const MANIFEST_FILENAME: &str = "_manifest.json";
// Local-first project document tools consume this cache artifact instead of calling the
// platform for every read/search operation.
const KNOWLEDGE_MANIFEST_FILENAME: &str = "knowledge_manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledgeManifest {
    pub pack_id: String,
    pub pack_version: String,
    pub schema_version: u32,
    pub root_uri: String,
    pub synced_at: String,
    pub docs: Vec<ProjectKnowledgeDocManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledgeDocManifest {
    pub id: String,
    pub virtual_path: String,
    pub source_path: String,
    pub title: String,
    pub summary: String,
    pub description: Option<String>,
    pub kind: String,
    pub authority: String,
    pub status: String,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub related: Vec<ProjectKnowledgeDocEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledgeDocEdge {
    #[serde(rename = "type", alias = "edge_type")]
    pub edge_type: String,
    pub target: String,
    pub description: Option<String>,
}

/// Load the manifest from `project_dir/_manifest.json`, returning `None` if
/// the file doesn't exist or can't be parsed.
pub fn load_manifest(project_dir: &Path) -> Option<DocumentManifest> {
    let path = project_dir.join(MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn load_project_knowledge_manifest(project_dir: &Path) -> Option<ProjectKnowledgeManifest> {
    let path = project_dir.join(KNOWLEDGE_MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write the manifest atomically (tmp + rename).
fn write_manifest(project_dir: &Path, manifest: &DocumentManifest) -> Result<()> {
    let target = project_dir.join(MANIFEST_FILENAME);
    let tmp = project_dir.join(format!(".{MANIFEST_FILENAME}.tmp"));

    let json =
        serde_json::to_string_pretty(manifest).context("Failed to serialize document manifest")?;

    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;

    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;

    Ok(())
}

fn write_project_knowledge_manifest(
    project_dir: &Path,
    manifest: &ProjectKnowledgeManifest,
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

    let manifest = load_manifest(project_dir);
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

    // Write updated manifest — only include docs that were successfully synced
    let now = chrono::Utc::now().to_rfc3339();
    let new_manifest = DocumentManifest {
        project_id,
        synced_at: now,
        documents: remote_docs
            .iter()
            .filter(|d| !failed_ids.contains(&d.id))
            .map(|d| ManifestEntry {
                id: d.id,
                filename: d.filename.clone(),
                path: d.path.clone(),
                title: d.title.clone(),
                kind: d.kind.clone(),
                authority: d.authority.clone(),
                summary: d.summary.clone(),
                status: d.status.clone(),
                tags: d.tags.clone(),
                size_bytes: d.size_bytes,
                updated_at: d.updated_at.clone(),
            })
            .collect(),
    };

    write_manifest(project_dir, &new_manifest)?;
    write_project_knowledge_manifest(
        project_dir,
        &build_project_knowledge_manifest(project_id, &remote_docs, &edges_by_doc),
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
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
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

fn upsert_manifest_entry(project_dir: &Path, project_id: Uuid, entry: ManifestEntry) -> Result<()> {
    let mut manifest = load_manifest(project_dir).unwrap_or(DocumentManifest {
        project_id,
        synced_at: chrono::Utc::now().to_rfc3339(),
        documents: Vec::new(),
    });
    manifest.project_id = project_id;
    manifest.synced_at = chrono::Utc::now().to_rfc3339();
    if let Some(pos) = manifest.documents.iter().position(|doc| doc.id == entry.id) {
        manifest.documents[pos] = entry;
    } else {
        manifest.documents.push(entry);
    }
    write_manifest(project_dir, &manifest)
}

pub fn upsert_document_metadata(
    project_dir: &Path,
    project_id: Uuid,
    metadata: &DocumentSyncMeta,
) -> Result<()> {
    let entry = ManifestEntry {
        id: metadata.id,
        filename: metadata.filename.clone(),
        path: metadata.path.clone(),
        title: metadata.title.clone(),
        kind: metadata.kind.clone(),
        authority: metadata.authority.clone(),
        summary: metadata.summary.clone(),
        status: metadata.status.clone(),
        tags: metadata.tags.clone(),
        size_bytes: metadata.size_bytes,
        updated_at: metadata.updated_at.clone(),
    };
    upsert_manifest_entry(project_dir, project_id, entry)
}

pub fn remove_manifest_entry(project_dir: &Path, document_id: Uuid) -> Result<Option<String>> {
    let Some(mut manifest) = load_manifest(project_dir) else {
        return Ok(None);
    };
    let Some(pos) = manifest
        .documents
        .iter()
        .position(|doc| doc.id == document_id)
    else {
        return Ok(None);
    };
    let relative_path = manifest_entry_relative_path(&manifest.documents[pos]);
    manifest.documents.remove(pos);
    manifest.synced_at = chrono::Utc::now().to_rfc3339();
    write_manifest(project_dir, &manifest)?;
    Ok(Some(relative_path))
}

pub fn remove_project_knowledge_entry(project_dir: &Path, document_id: Uuid) -> Result<()> {
    let Some(mut manifest) = load_project_knowledge_manifest(project_dir) else {
        return Ok(());
    };
    let doc_id = document_id.to_string();
    let removed_virtual_paths: std::collections::HashSet<String> = manifest
        .docs
        .iter()
        .filter(|doc| doc.id == doc_id)
        .map(|doc| doc.virtual_path.clone())
        .collect();
    manifest.docs.retain(|doc| doc.id != doc_id);
    for doc in &mut manifest.docs {
        doc.related
            .retain(|edge| !removed_virtual_paths.contains(&edge.target));
    }
    write_project_knowledge_manifest(project_dir, &manifest)
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

    let entry = ManifestEntry {
        id: document_id,
        filename: resolved_meta.filename.clone(),
        path: resolved_meta.path.clone(),
        title: resolved_meta.title.clone(),
        kind: resolved_meta.kind.clone(),
        authority: resolved_meta.authority.clone(),
        summary: resolved_meta.summary.clone(),
        status: resolved_meta.status.clone(),
        tags: resolved_meta.tags.clone(),
        size_bytes: resolved_meta.size_bytes.max(response.size_bytes),
        updated_at: resolved_meta.updated_at.clone(),
    };
    upsert_manifest_entry(project_dir, project_id, entry)?;
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

    if let Some(existing) = load_manifest(project_dir)
        .and_then(|manifest| {
            manifest
                .documents
                .into_iter()
                .find(|entry| entry.id == document_id)
        })
        .map(|entry| manifest_entry_relative_path(&entry))
    {
        reconcile_document_file_location(
            project_dir,
            &existing,
            &document_relative_path(&resolved_meta),
        )?;
    }

    upsert_document_metadata(project_dir, project_id, &resolved_meta)?;
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
    let virtual_path = project_doc_virtual_path(project_id, metadata);
    let next = project_knowledge_doc(project_id, metadata, edges, |target_id| {
        manifest
            .docs
            .iter()
            .find(|doc| doc.id == target_id.to_string())
            .map(|doc| doc.virtual_path.clone())
    });
    if let Some(pos) = manifest.docs.iter().position(|doc| doc.id == next.id) {
        manifest.docs[pos] = next;
    } else {
        manifest.docs.push(next);
    }
    for doc in &mut manifest.docs {
        doc.related.retain(|edge| edge.target != virtual_path);
    }
    for edge in edges {
        if edge.target_document_id == metadata.id
            && let Some(source) = manifest
                .docs
                .iter_mut()
                .find(|doc| doc.id == edge.source_document_id.to_string())
        {
            let target = virtual_path.clone();
            if !source
                .related
                .iter()
                .any(|existing| existing.edge_type == edge.edge_type && existing.target == target)
            {
                source.related.push(ProjectKnowledgeDocEdge {
                    edge_type: edge.edge_type.clone(),
                    target,
                    description: edge.note.clone(),
                });
            }
        }
    }
    manifest
        .docs
        .sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
    manifest.synced_at = chrono::Utc::now().to_rfc3339();
    write_project_knowledge_manifest(project_dir, &manifest)
}

fn build_project_knowledge_manifest(
    project_id: Uuid,
    docs: &[DocumentSyncMeta],
    edges_by_doc: &HashMap<Uuid, Vec<DocumentSyncEdge>>,
) -> ProjectKnowledgeManifest {
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
    ProjectKnowledgeManifest {
        pack_id: format!("project-{project_id}"),
        pack_version: "1".to_string(),
        schema_version: 1,
        root_uri: format!("project://{project_id}/"),
        synced_at: chrono::Utc::now().to_rfc3339(),
        docs: entries,
    }
}

fn empty_knowledge_manifest(project_id: Uuid) -> ProjectKnowledgeManifest {
    ProjectKnowledgeManifest {
        pack_id: format!("project-{project_id}"),
        pack_version: "1".to_string(),
        schema_version: 1,
        root_uri: format!("project://{project_id}/"),
        synced_at: chrono::Utc::now().to_rfc3339(),
        docs: Vec::new(),
    }
}

fn project_knowledge_doc(
    project_id: Uuid,
    doc: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
    resolve_target: impl Fn(Uuid) -> Option<String>,
) -> ProjectKnowledgeDocManifest {
    let relative_path = project_doc_relative_path(doc);
    ProjectKnowledgeDocManifest {
        id: doc.id.to_string(),
        virtual_path: project_doc_virtual_path(project_id, doc),
        source_path: format!("docs/{relative_path}"),
        title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
        summary: doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Project document {relative_path}")),
        description: None,
        kind: doc.kind.clone().unwrap_or_else(|| "reference".to_string()),
        authority: doc
            .authority
            .clone()
            .unwrap_or_else(|| "reference".to_string()),
        status: doc.status.clone().unwrap_or_else(|| "stable".to_string()),
        tags: doc.tags.clone(),
        aliases: vec![doc.filename.clone(), relative_path.clone()],
        keywords: doc.tags.clone(),
        related: edges
            .iter()
            .filter(|edge| edge.source_document_id == doc.id)
            .filter_map(|edge| {
                resolve_target(edge.target_document_id).map(|target| ProjectKnowledgeDocEdge {
                    edge_type: edge.edge_type.clone(),
                    target,
                    description: edge.note.clone(),
                })
            })
            .collect(),
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
            content_type: "text/markdown".into(),
            size_bytes: size,
            updated_at: updated.into(),
        }
    }

    fn entry(id: u128, filename: &str, updated: &str, size: i64) -> ManifestEntry {
        ManifestEntry {
            id: Uuid::from_u128(id),
            filename: filename.into(),
            path: None,
            title: None,
            kind: None,
            authority: None,
            summary: None,
            status: None,
            tags: Vec::new(),
            size_bytes: size,
            updated_at: updated.into(),
        }
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
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "arch.md", "2026-01-01", 100)],
        };
        let remote = vec![meta(1, "arch.md", "2026-01-01", 100)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_updated_downloads() {
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "arch.md", "2026-01-01", 100)],
        };
        let remote = vec![meta(1, "arch.md", "2026-02-01", 150)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_deleted_remotely() {
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![
                entry(1, "arch.md", "2026-01-01", 100),
                entry(2, "old.md", "2026-01-01", 50),
            ],
        };
        let remote = vec![meta(1, "arch.md", "2026-01-01", 100)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_new_and_deleted() {
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "old.md", "2026-01-01", 100)],
        };
        let remote = vec![meta(2, "new.md", "2026-02-01", 200)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_download[0].filename, "new.md");
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_renamed_file_without_content_change_renames_only() {
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "old.md", "2026-01-01", 100)],
        };
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
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "old.md", "2026-01-01", 100)],
        };
        let remote = vec![meta(1, "new.md", "2026-02-01", 150)];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn manifest_roundtrip() {
        let manifest = DocumentManifest {
            project_id: Uuid::nil(),
            synced_at: "2026-01-01T00:00:00Z".into(),
            documents: vec![entry(1, "arch.md", "2026-01-01", 100)],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: DocumentManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.project_id, Uuid::nil());
        assert_eq!(parsed.documents.len(), 1);
        assert_eq!(parsed.documents[0].filename, "arch.md");
    }

    #[test]
    fn load_manifest_missing_file() {
        let dir = std::env::temp_dir().join("nenjo_test_missing_manifest");
        let _ = std::fs::create_dir_all(&dir);
        assert!(load_manifest(&dir).is_none());
    }

    #[test]
    fn write_and_load_manifest() {
        let dir = std::env::temp_dir().join("nenjo_test_write_manifest");
        let _ = std::fs::create_dir_all(&dir);

        let manifest = DocumentManifest {
            project_id: Uuid::from_u128(42),
            synced_at: "2026-02-22T12:00:00Z".into(),
            documents: vec![entry(1, "test.md", "2026-02-22", 512)],
        };

        write_manifest(&dir, &manifest).unwrap();
        let loaded = load_manifest(&dir).unwrap();
        assert_eq!(loaded.project_id, Uuid::from_u128(42));
        assert_eq!(loaded.documents[0].filename, "test.md");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
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
