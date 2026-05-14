//! Library knowledge sync — download library items to the local workspace.
//!
//! At bootstrap, fetches library item metadata via the v1 API, diffs against
//! the local library `manifest.json`, and downloads new/changed items.
//! Deleted items are removed locally. Network errors are soft-fail (logged);
//! filesystem errors are hard-fail.

use anyhow::{Context, Result};
use nenjo_events::EncryptedPayload;
use nenjo_knowledge::KnowledgeDocManifest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::api_client::{DocumentSyncEdge, DocumentSyncMeta, NenjoClient};
use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use nenjo_platform::library_knowledge::{
    LibraryKnowledgePackManifest, build_library_knowledge_manifest, library_item_relative_path,
    library_knowledge_item_relative_path, load_library_knowledge_manifest,
    remove_library_knowledge_entry, upsert_library_knowledge_entry,
    write_library_knowledge_manifest,
};

pub fn manifest_library_dir(
    manifest: &nenjo::Manifest,
    workspace_dir: &Path,
    project_id: Uuid,
) -> PathBuf {
    workspace_dir
        .join("library")
        .join(library_pack_slug(manifest, project_id))
}

pub fn remove_manifest_document(
    manifest: &nenjo::Manifest,
    workspace_dir: &Path,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<()> {
    let library_dir = manifest_library_dir(manifest, workspace_dir, project_id);
    let existing = library_knowledge_item_relative_path(&library_dir, document_id);
    remove_library_knowledge_entry(&library_dir, document_id)?;
    if let Some(filename) = existing {
        delete_document_file(&library_dir, &filename)?;
    } else {
        debug!(%project_id, %document_id, "Deleted library knowledge item was not present in local knowledge manifest");
    }
    Ok(())
}

fn library_pack_slug(manifest: &nenjo::Manifest, project_id: Uuid) -> String {
    manifest
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.slug.clone())
        .unwrap_or_else(|| project_id.to_string())
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Result of comparing remote library items against the local manifest.
#[derive(Debug)]
pub struct SyncDiff {
    /// Library items to download (new or updated).
    pub to_download: Vec<DocumentSyncMeta>,
    /// Local files to rename when only the filename changed.
    pub to_rename: Vec<FileRename>,
    /// Local library item files to delete (no longer present remotely).
    pub to_delete: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    pub from: String,
    pub to: String,
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

/// Compare remote library item list against a local manifest.
///
/// A library item is considered changed if its `updated_at` timestamp differs.
pub fn compute_diff(
    manifest: Option<&LibraryKnowledgePackManifest>,
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
                None => true, // new library item
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
                    knowledge_doc_relative_path(entry) != library_item_relative_path(doc)
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
            let to = library_item_relative_path(doc);
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
// Sync
// ---------------------------------------------------------------------------

/// Sync library knowledge packs into the workspace library.
///
/// Network/API errors are logged and skipped (soft-fail).
/// Filesystem errors are propagated (hard-fail).
pub async fn sync_all(
    api: &NenjoClient,
    workspace_dir: &Path,
    state_dir: &Path,
    _projects: &[nenjo::manifest::ProjectManifest],
) -> Result<()> {
    let library_dir = workspace_dir.join("library");
    std::fs::create_dir_all(&library_dir).with_context(|| {
        format!(
            "Failed to create library directory: {}",
            library_dir.display()
        )
    })?;

    let packs = match api.list_knowledge_packs().await {
        Ok(packs) => packs,
        Err(e) => {
            warn!(
                error = %e,
                "Failed to list knowledge packs — skipping knowledge sync"
            );
            return Ok(());
        }
    };

    for pack in packs {
        let pack_dir = library_dir.join(&pack.slug);
        if let Err(e) = sync_pack(api, &pack_dir, pack.id, state_dir).await {
            warn!(
                pack_id = %pack.id,
                pack_slug = %pack.slug,
                error = %e,
                "Knowledge sync failed for pack — continuing"
            );
        }
    }

    Ok(())
}

/// Sync knowledge items for a single pack.
pub async fn sync_pack(
    api: &NenjoClient,
    pack_dir: &Path,
    pack_id: Uuid,
    state_dir: &Path,
) -> Result<()> {
    let pack_slug = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("default")
        .to_string();

    let remote_docs = match api.list_knowledge_items(pack_id).await {
        Ok(docs) => docs,
        Err(e) => {
            warn!(
                pack_id = %pack_id,
                error = %e,
                "Failed to list knowledge items — skipping sync"
            );
            return Ok(());
        }
    };

    let mut edges_by_doc: HashMap<Uuid, Vec<DocumentSyncEdge>> = HashMap::new();
    for doc in &remote_docs {
        match api.list_knowledge_item_edges(pack_id, doc.id).await {
            Ok(edges) => {
                edges_by_doc.insert(doc.id, edges);
            }
            Err(e) => {
                warn!(
                    pack_id = %pack_id,
                    doc_id = %doc.id,
                    error = %e,
                    "Failed to list knowledge item edges — continuing with empty edge set"
                );
                edges_by_doc.insert(doc.id, Vec::new());
            }
        }
    }

    let manifest = load_library_knowledge_manifest(pack_dir);
    let diff = compute_diff(manifest.as_ref(), &remote_docs);

    if diff.to_download.is_empty() && diff.to_delete.is_empty() && diff.to_rename.is_empty() {
        debug!(pack_id = %pack_id, "Knowledge items up to date");
        return Ok(());
    }

    info!(
        pack_id = %pack_id,
        pack_slug = %pack_slug,
        downloads = diff.to_download.len(),
        renames = diff.to_rename.len(),
        deletes = diff.to_delete.len(),
        "Syncing knowledge pack"
    );

    // Track which library items were successfully downloaded.
    let mut failed_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    // Ensure the local docs directory exists inside the library pack.
    let docs_dir = pack_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;

    // Download new/changed library items.
    for doc in &diff.to_download {
        match api.get_knowledge_item_content(pack_id, doc.id).await {
            Ok(response) => {
                let content = resolve_document_content(
                    state_dir,
                    &response.encrypted_payload,
                    response.content,
                )
                .await
                .with_context(|| {
                    format!("Failed to resolve content for library item {}", doc.id)
                })?;

                write_document_content(pack_dir, &library_item_relative_path(doc), &content)?;
                info!(doc_id = %doc.id, filename = %doc.filename, "Downloaded knowledge item");
            }
            Err(e) => {
                warn!(
                    doc_id = %doc.id,
                    filename = %doc.filename,
                    error = %e,
                    "Failed to download knowledge item — skipping"
                );
                failed_ids.insert(doc.id);
            }
        }
    }

    for rename in &diff.to_rename {
        rename_document_file(pack_dir, &rename.from, &rename.to)?;
        debug!(from = %rename.from, to = %rename.to, "Renamed library item file");
    }

    // Delete removed library items.
    for relative_path in &diff.to_delete {
        let path = docs_dir.join(relative_path);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete {}", path.display()))?;
            debug!(path = %relative_path, "Deleted removed library item file");
        }
    }

    let synced_docs = remote_docs
        .iter()
        .filter(|doc| !failed_ids.contains(&doc.id))
        .cloned()
        .collect::<Vec<_>>();
    write_library_knowledge_manifest(
        pack_dir,
        &build_library_knowledge_manifest(pack_id, &pack_slug, &synced_docs, &edges_by_doc),
    )?;

    Ok(())
}

pub fn write_document_content(pack_dir: &Path, relative_path: &str, content: &str) -> Result<()> {
    let docs_dir = pack_dir.join("docs");
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

pub fn delete_document_file(pack_dir: &Path, relative_path: &str) -> Result<()> {
    let path = pack_dir.join("docs").join(relative_path);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete {}", path.display()))?;
    }
    Ok(())
}

pub fn rename_document_file(pack_dir: &Path, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }
    let docs_dir = pack_dir.join("docs");
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

fn reconcile_document_file_location(pack_dir: &Path, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }

    let docs_dir = pack_dir.join("docs");
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

    rename_document_file(pack_dir, from, to)
}

pub async fn sync_document(
    api: &NenjoClient,
    pack_dir: &Path,
    pack_id: Uuid,
    document_id: Uuid,
    state_dir: &Path,
    metadata: Option<&DocumentSyncMeta>,
) -> Result<()> {
    let pack_slug = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("default")
        .to_string();
    let response = api.get_knowledge_item_content(pack_id, document_id).await?;
    let content =
        resolve_document_content(state_dir, &response.encrypted_payload, response.content).await?;

    let resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_knowledge_items(pack_id)
            .await?
            .into_iter()
            .find(|doc| doc.id == document_id)
            .ok_or_else(|| {
                anyhow::anyhow!("knowledge item not found in metadata list: {document_id}")
            })?
    };
    write_document_content(
        pack_dir,
        &library_item_relative_path(&resolved_meta),
        &content,
    )?;

    let edges = api
        .list_knowledge_item_edges(pack_id, document_id)
        .await
        .unwrap_or_default();

    let mut resolved_meta = resolved_meta;
    resolved_meta.size_bytes = resolved_meta.size_bytes.max(response.size_bytes);
    upsert_library_knowledge_entry(pack_dir, pack_id, &pack_slug, &resolved_meta, &edges)?;
    Ok(())
}

pub async fn sync_document_metadata(
    api: &NenjoClient,
    pack_dir: &Path,
    pack_id: Uuid,
    document_id: Uuid,
    metadata: Option<&DocumentSyncMeta>,
) -> Result<()> {
    let pack_slug = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("default")
        .to_string();
    let resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_knowledge_items(pack_id)
            .await?
            .into_iter()
            .find(|doc| doc.id == document_id)
            .ok_or_else(|| {
                anyhow::anyhow!("knowledge item not found in metadata list: {document_id}")
            })?
    };

    let edges = api
        .list_knowledge_item_edges(pack_id, document_id)
        .await
        .unwrap_or_default();

    if let Some(existing) = library_knowledge_item_relative_path(pack_dir, document_id) {
        reconcile_document_file_location(
            pack_dir,
            &existing,
            &library_item_relative_path(&resolved_meta),
        )?;
    }

    upsert_library_knowledge_entry(pack_dir, pack_id, &pack_slug, &resolved_meta, &edges)?;
    Ok(())
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
            pack_id: Uuid::from_u128(7),
            slug: "test".into(),
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

    fn manifest(docs: Vec<DocumentSyncMeta>) -> LibraryKnowledgePackManifest {
        build_library_knowledge_manifest(Uuid::nil(), "test", &docs, &Default::default())
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
    fn library_knowledge_manifest_does_not_derive_aliases() {
        let mut doc = meta(1, "random.md", "2026-02-22", 512);
        doc.path = Some("domain/path".into());
        doc.title = Some("Random".into());
        doc.summary = Some("Just a test document".into());
        doc.aliases = vec!["Random concept".into()];
        doc.keywords = vec!["randomness".into()];

        let manifest = build_library_knowledge_manifest(
            Uuid::from_u128(7),
            "test",
            &[doc],
            &Default::default(),
        );

        assert_eq!(manifest.docs.len(), 1);
        assert_eq!(manifest.docs[0].aliases, ["Random concept"]);
        assert_eq!(manifest.docs[0].keywords, ["randomness"]);
        assert_eq!(manifest.docs[0].summary, "Just a test document");
        assert_eq!(
            manifest.docs[0].virtual_path,
            "library://test/domain/path/random.md"
        );
    }

    #[test]
    fn library_item_metadata_persists_to_knowledge_manifest() {
        let dir = tempdir().unwrap();
        let pack_id = Uuid::from_u128(7);
        let mut doc = meta(1, "random.md", "2026-02-22", 512);
        doc.path = Some("domain".into());
        doc.title = Some("Random".into());
        doc.kind = Some("guide".into());
        doc.authority = Some("draft".into());
        doc.summary = Some("Just a test document".into());
        doc.status = Some("draft".into());
        doc.tags = vec!["library".into()];
        doc.aliases = vec!["Random concept".into()];
        doc.keywords = vec!["randomness".into()];

        upsert_library_knowledge_entry(dir.path(), pack_id, "test", &doc, &[]).unwrap();

        let knowledge = load_library_knowledge_manifest(dir.path()).unwrap();
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
