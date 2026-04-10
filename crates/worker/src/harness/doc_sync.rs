//! Document sync — download project documents to the local workspace.
//!
//! At bootstrap, fetches document metadata for each project via the v1 API,
//! diffs against a local `_manifest.json`, and downloads new/changed docs.
//! Deleted docs are removed locally. Network errors are soft-fail (logged);
//! filesystem errors are hard-fail.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};
use uuid::Uuid;

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
    /// Local filenames to delete (no longer present remotely).
    pub to_delete: Vec<String>,
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

    let to_delete: Vec<String> = manifest
        .map(|m| {
            m.documents
                .iter()
                .filter(|e| !remote_ids.contains(&e.id))
                .map(|e| e.filename.clone())
                .collect()
        })
        .unwrap_or_default();

    SyncDiff {
        to_download,
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
    projects: &[nenjo::manifest::ProjectManifest],
) -> Result<()> {
    for project in projects.iter().filter(|p| !p.is_system) {
        let project_dir = workspace_dir.join(&project.slug);
        std::fs::create_dir_all(&project_dir).with_context(|| {
            format!(
                "Failed to create project directory: {}",
                project_dir.display()
            )
        })?;

        if let Err(e) = sync_project(api, &project_dir, project.id).await {
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
pub async fn sync_project(api: &NenjoClient, project_dir: &Path, project_id: Uuid) -> Result<()> {
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

    if diff.to_download.is_empty() && diff.to_delete.is_empty() {
        debug!(project_id = %project_id, "Documents up to date");
        return Ok(());
    }

    info!(
        project_id = %project_id,
        downloads = diff.to_download.len(),
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
            Ok(response_text) => {
                // The content endpoint returns JSON { content, filename, ... }
                // Extract the actual content string
                let content = serde_json::from_str::<serde_json::Value>(&response_text)
                    .ok()
                    .and_then(|v| v.get("content").and_then(|c| c.as_str()).map(String::from))
                    .unwrap_or(response_text);

                let target = docs_dir.join(&doc.filename);
                let tmp = docs_dir.join(format!(".{}.tmp", &doc.filename));

                std::fs::write(&tmp, content.as_bytes())
                    .with_context(|| format!("Failed to write {}", tmp.display()))?;
                std::fs::rename(&tmp, &target).with_context(|| {
                    format!("Failed to rename {} → {}", tmp.display(), target.display())
                })?;

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
                size_bytes: d.size_bytes,
                updated_at: d.updated_at.clone(),
            })
            .collect(),
    };

    write_manifest(project_dir, &new_manifest)?;

    Ok(())
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
            content_type: "text/markdown".into(),
            size_bytes: size,
            updated_at: updated.into(),
        }
    }

    fn entry(id: u128, filename: &str, updated: &str, size: i64) -> ManifestEntry {
        ManifestEntry {
            id: Uuid::from_u128(id),
            filename: filename.into(),
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
        assert_eq!(diff.to_download[0].filename, "new.md");
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
