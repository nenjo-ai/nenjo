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

use crate::crypto::decrypt_text;
use crate::crypto::provider::WorkerAuthProvider;
use crate::harness::api_client::{DocumentSyncMeta, NenjoClient};

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
                    entry.filename != doc.filename && entry.updated_at != doc.updated_at
                })
                .map(|_| doc.id)
        })
        .collect();

    let to_rename: Vec<FileRename> = remote
        .iter()
        .filter_map(|doc| {
            let entry = local_map.get(&doc.id)?;
            if entry.filename == doc.filename || rename_download_ids.contains(&doc.id) {
                return None;
            }
            Some(FileRename {
                from: entry.filename.clone(),
                to: doc.filename.clone(),
            })
        })
        .collect();

    let to_delete: Vec<String> = manifest
        .map(|m| {
            m.documents
                .iter()
                .filter(|e| !remote_ids.contains(&e.id) || rename_download_ids.contains(&e.id))
                .map(|e| e.filename.clone())
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

/// Load the manifest from `project_dir/_manifest.json`, returning `None` if
/// the file doesn't exist or can't be parsed.
pub fn load_manifest(project_dir: &Path) -> Option<DocumentManifest> {
    let path = project_dir.join(MANIFEST_FILENAME);
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

                write_document_content(project_dir, &doc.filename, &content)?;
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
    for filename in &diff.to_delete {
        let path = docs_dir.join(filename);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete {}", path.display()))?;
            debug!(filename = %filename, "Deleted removed document");
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

    Ok(())
}

pub fn write_document_content(project_dir: &Path, filename: &str, content: &str) -> Result<()> {
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;

    let target = docs_dir.join(filename);
    let tmp = docs_dir.join(format!(".{}.tmp", filename));

    std::fs::write(&tmp, content.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
}

pub fn delete_document_file(project_dir: &Path, filename: &str) -> Result<()> {
    let path = project_dir.join("docs").join(filename);
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
    let filename = manifest.documents.remove(pos).filename;
    manifest.synced_at = chrono::Utc::now().to_rfc3339();
    write_manifest(project_dir, &manifest)?;
    Ok(Some(filename))
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
    write_document_content(project_dir, &response.filename, &content)?;

    let entry = ManifestEntry {
        id: document_id,
        filename: response.filename,
        path: metadata.and_then(|doc| doc.path.clone()),
        title: metadata.and_then(|doc| doc.title.clone()),
        kind: metadata.and_then(|doc| doc.kind.clone()),
        authority: metadata.and_then(|doc| doc.authority.clone()),
        summary: metadata.and_then(|doc| doc.summary.clone()),
        status: metadata.and_then(|doc| doc.status.clone()),
        tags: metadata.map(|doc| doc.tags.clone()).unwrap_or_default(),
        size_bytes: metadata
            .map(|doc| doc.size_bytes)
            .unwrap_or(response.size_bytes),
        updated_at: metadata
            .map(|doc| doc.updated_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    };
    upsert_manifest_entry(project_dir, project_id, entry)?;
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
    let ack = auth_provider
        .load_ack()
        .await?
        .context("worker has no enrolled ACK for document decrypt")?;
    let plaintext = decrypt_text(&ack, encrypted_payload)?;
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
}
