//! Library knowledge sync — download user-uploaded library documents locally.
//!
//! At bootstrap, fetches uploaded pack metadata via the v1 API, diffs against the
//! local library `manifest.json`, and downloads new/changed documents from object
//! storage. Package- and GitHub-backed knowledge stays in installed package
//! trees; it is not mirrored through platform S3. Network errors are soft-fail
//! (logged); filesystem errors are hard-fail.

use anyhow::{Context, Result};
use nenjo::Slug;
use nenjo::manifest::{KnowledgePackManifest, KnowledgePackSource};
use nenjo_events::EncryptedPayload;
use nenjo_knowledge::KnowledgeDocManifest;
use nenjo_platform::PlatformResourceIdStore;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::api_client::{ApiClient, KnowledgeDocumentRecord, KnowledgePackRecord};

use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use crate::handlers::manifest::knowledge::DocumentEdgesSource;
use nenjo_platform::library_knowledge::{
    LibraryKnowledgePackCacheEntry, LibraryKnowledgePackManifest, ReplaceDocumentEdges,
    build_library_knowledge_manifest, ensure_library_knowledge_pack_cache,
    library_knowledge_doc_relative_path, load_library_knowledge_manifest,
    manifest_doc_relative_path, remove_library_knowledge_entry, upsert_library_knowledge_entry,
    upsert_library_knowledge_entry_with_edges, write_library_knowledge_manifest,
};

pub fn remove_manifest_document_from_pack_dir(
    pack_dir: &Path,
    doc: &nenjo::Slug,
    metadata: Option<&KnowledgeDocumentRecord>,
) -> Result<()> {
    let existing = library_knowledge_doc_relative_path(pack_dir, doc)
        .or_else(|| metadata.map(|record| record.library_doc_relative_path()));
    remove_library_knowledge_entry(pack_dir, doc)?;
    if let Some(filename) = existing {
        delete_document_file(pack_dir, &filename)?;
    } else {
        debug!(%doc, "Deleted library knowledge document was not present in local knowledge manifest");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Result of comparing remote library documents against the local manifest.
#[derive(Debug)]
pub struct SyncDiff {
    /// Library items to download (new or updated).
    pub to_download: Vec<KnowledgeDocumentRecord>,
    /// Local files to rename when only the filename changed.
    pub to_rename: Vec<FileRename>,
    /// Local library document files to delete (no longer present remotely).
    pub to_delete: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    pub from: String,
    pub to: String,
}

fn knowledge_doc_slug(doc: &KnowledgeDocManifest) -> Option<nenjo::Slug> {
    nenjo::Slug::parse(&doc.id).ok()
}

/// Compare remote library document list against a local manifest.
///
/// A library document is considered changed if its `updated_at` timestamp differs.
pub fn compute_diff(
    manifest: Option<&LibraryKnowledgePackManifest>,
    remote: &[KnowledgeDocumentRecord],
) -> SyncDiff {
    let local_map: HashMap<nenjo::Slug, &KnowledgeDocManifest> = manifest
        .map(|m| {
            m.docs
                .iter()
                .filter_map(|doc| knowledge_doc_slug(doc).map(|slug| (slug, doc)))
                .collect()
        })
        .unwrap_or_default();

    let remote_docs: std::collections::HashSet<nenjo::Slug> = remote
        .iter()
        .map(|doc| nenjo::Slug::derive(&doc.slug))
        .collect();

    let to_download: Vec<KnowledgeDocumentRecord> = remote
        .iter()
        .filter(|doc| {
            let slug = nenjo::Slug::derive(&doc.slug);
            match local_map.get(&slug) {
                Some(entry) => entry.updated_at != doc.updated_at_rfc3339(),
                None => true, // new library document
            }
        })
        .cloned()
        .collect();

    let rename_download_docs: std::collections::HashSet<nenjo::Slug> = remote
        .iter()
        .filter_map(|doc| {
            let slug = nenjo::Slug::derive(&doc.slug);
            local_map
                .get(&slug)
                .filter(|entry| {
                    manifest_doc_relative_path(entry) != doc.library_doc_relative_path()
                        && entry.updated_at != doc.updated_at_rfc3339()
                })
                .map(|_| slug)
        })
        .collect();

    let to_rename: Vec<FileRename> = remote
        .iter()
        .filter_map(|doc| {
            let slug = nenjo::Slug::derive(&doc.slug);
            let entry = local_map.get(&slug)?;
            let from = manifest_doc_relative_path(entry);
            let to = doc.library_doc_relative_path();
            if from == to || rename_download_docs.contains(&slug) {
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
                    let doc = knowledge_doc_slug(entry)?;
                    (!remote_docs.contains(&doc) || rename_download_docs.contains(&doc))
                        .then(|| manifest_doc_relative_path(entry))
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
pub fn reconcile_knowledge_document_resource_ids(
    manifests_dir: &Path,
    pack_slug: &str,
    remote_docs: &[KnowledgeDocumentRecord],
    previous_manifest: Option<&LibraryKnowledgePackManifest>,
) -> Result<()> {
    let store = PlatformResourceIdStore::new(manifests_dir);
    let pack = Slug::derive(pack_slug);
    let remote_slugs: HashSet<Slug> = remote_docs
        .iter()
        .map(|doc| Slug::derive(&doc.slug))
        .collect();

    for doc in remote_docs {
        store.upsert_knowledge_document(&pack, &Slug::derive(&doc.slug), doc.id)?;
    }

    if let Some(manifest) = previous_manifest {
        for entry in &manifest.docs {
            let Ok(doc_slug) = Slug::parse(&entry.id) else {
                continue;
            };
            if !remote_slugs.contains(&doc_slug) {
                store.remove_knowledge_document(&pack, &doc_slug)?;
            }
        }
    }

    Ok(())
}

fn is_uploaded_library_pack(pack: &KnowledgePackRecord) -> bool {
    pack.source_type == "uploaded"
}

fn remote_library_pack_slugs(packs: &[KnowledgePackRecord]) -> HashSet<String> {
    packs
        .iter()
        .filter(|pack| is_uploaded_library_pack(pack))
        .map(|pack| pack.slug.clone())
        .collect()
}

async fn sync_library_knowledge_pack_manifests(
    manifests_dir: &Path,
    library_dir: &Path,
    remote_packs: &[KnowledgePackRecord],
) -> Result<()> {
    let remote_slugs = remote_library_pack_slugs(remote_packs);
    let store = nenjo::LocalManifestStore::new(manifests_dir);
    let mut manifest = nenjo::ManifestReader::load_manifest(&store).await?;
    manifest.knowledge_packs.retain(|pack| {
        pack.source_type != KnowledgePackSource::Library
            || remote_slugs.contains(pack.slug.as_str())
    });
    for pack in remote_packs
        .iter()
        .filter(|pack| is_uploaded_library_pack(pack))
    {
        let entry = library_knowledge_pack_manifest(pack, library_dir);
        manifest.upsert_resource(nenjo::ManifestResource::KnowledgePack(entry));
    }
    nenjo::ManifestWriter::replace_manifest(&store, &manifest).await
}

async fn upsert_library_knowledge_pack_manifest(
    manifests_dir: &Path,
    library_dir: &Path,
    pack: &KnowledgePackRecord,
) -> Result<()> {
    if !is_uploaded_library_pack(pack) {
        return Ok(());
    }
    let store = nenjo::LocalManifestStore::new(manifests_dir);
    ensure_library_knowledge_pack_cache(
        &store,
        library_dir,
        LibraryKnowledgePackCacheEntry {
            slug: Slug::derive(&pack.slug),
            name: Some(pack.name.clone()),
            description: Some(pack.description.clone()),
            selector: pack
                .selector
                .clone()
                .or_else(|| Some(format!("lib:{}", pack.slug))),
            version: pack.version.clone(),
            read_only: Some(pack.read_only),
            metadata: Some(pack.metadata.clone()),
        },
    )
    .await?;
    Ok(())
}

async fn ensure_library_knowledge_pack_registered(
    manifests_dir: &Path,
    pack_dir: &Path,
    pack_slug: &str,
) -> Result<()> {
    let Some(library_dir) = pack_dir.parent() else {
        return Ok(());
    };
    let store = nenjo::LocalManifestStore::new(manifests_dir);
    ensure_library_knowledge_pack_cache(
        &store,
        library_dir,
        LibraryKnowledgePackCacheEntry::from_slug(Slug::derive(pack_slug)),
    )
    .await?;
    Ok(())
}

fn library_knowledge_pack_manifest(
    pack: &KnowledgePackRecord,
    library_dir: &Path,
) -> KnowledgePackManifest {
    KnowledgePackManifest {
        slug: Slug::derive(&pack.slug),
        name: pack.name.clone(),
        description: pack.description.clone(),
        source_type: KnowledgePackSource::Library,
        selector: pack
            .selector
            .clone()
            .unwrap_or_else(|| format!("lib:{}", pack.slug)),
        version: pack.version.clone(),
        root_uri: format!("library://{}/", pack.slug),
        root_path: Some(library_dir.join(&pack.slug)),
        read_only: pack.read_only,
        metadata: pack.metadata.clone(),
    }
}

/// Remove local library packs and sidecar entries that no longer exist on the platform.
pub fn reconcile_library_knowledge_packs(
    library_dir: &Path,
    manifests_dir: &Path,
    remote_packs: &[KnowledgePackRecord],
) -> Result<()> {
    let remote_slugs = remote_library_pack_slugs(remote_packs);
    PlatformResourceIdStore::new(manifests_dir).reconcile_knowledge_packs(&remote_slugs)?;

    if !library_dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(library_dir).with_context(|| {
        format!(
            "Failed to read library knowledge directory {}",
            library_dir.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "Failed to read library knowledge directory entry in {}",
                library_dir.display()
            )
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(pack_slug) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if remote_slugs.contains(&pack_slug) {
            continue;
        }

        std::fs::remove_dir_all(&path).with_context(|| {
            format!(
                "Failed to remove stale library knowledge pack {}",
                path.display()
            )
        })?;
        info!(pack_slug = %pack_slug, "Removed stale library knowledge pack");
    }

    Ok(())
}

pub async fn sync_all(
    api: &ApiClient,
    nenjo_home: &Path,
    state_dir: &Path,
    manifests_dir: &Path,
    _projects: &[nenjo::manifest::ProjectManifest],
) -> Result<()> {
    let library_dir = nenjo_home.join("library");
    std::fs::create_dir_all(&library_dir).with_context(|| {
        format!(
            "Failed to create platform library directory: {}",
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

    if let Err(error) =
        sync_library_knowledge_pack_manifests(manifests_dir, &library_dir, &packs).await
    {
        warn!(
            error = %error,
            "Failed to sync local knowledge pack manifests"
        );
    }

    for pack in &packs {
        let result = if !is_uploaded_library_pack(pack) {
            debug!(
                pack_id = %pack.id,
                pack_slug = %pack.slug,
                source_type = %pack.source_type,
                "Skipping non-uploaded knowledge pack; content is served from its source resolver"
            );
            Ok(())
        } else {
            let pack_dir = library_dir.join(&pack.slug);
            sync_pack(api, &pack_dir, &pack.slug, state_dir, manifests_dir).await
        };
        if let Err(e) = result {
            warn!(
                pack_id = %pack.id,
                pack_slug = %pack.slug,
                source_type = %pack.source_type,
                error = %e,
                "Knowledge sync failed for pack — continuing"
            );
        }
    }

    if let Err(error) = reconcile_library_knowledge_packs(&library_dir, manifests_dir, &packs) {
        warn!(
            error = %error,
            "Failed to reconcile library knowledge packs against platform state"
        );
    }

    Ok(())
}

/// Sync one library knowledge pack by slug.
pub async fn sync_pack_by_slug(
    api: &ApiClient,
    nenjo_home: &Path,
    state_dir: &Path,
    manifests_dir: &Path,
    pack_slug: &nenjo::Slug,
) -> Result<()> {
    let packs = api
        .list_knowledge_packs()
        .await
        .context("failed to list knowledge packs")?;
    let Some(pack) = packs
        .into_iter()
        .find(|pack| nenjo::Slug::derive(&pack.slug) == *pack_slug)
    else {
        warn!(%pack_slug, "Knowledge pack not found during sync");
        return Ok(());
    };

    if !is_uploaded_library_pack(&pack) {
        debug!(
            pack_id = %pack.id,
            pack_slug = %pack.slug,
            source_type = %pack.source_type,
            "Skipping knowledge pack sync for non-uploaded pack"
        );
        return Ok(());
    }

    let pack_dir = nenjo_home.join("library").join(&pack.slug);
    upsert_library_knowledge_pack_manifest(manifests_dir, &nenjo_home.join("library"), &pack)
        .await?;
    sync_pack(api, &pack_dir, &pack.slug, state_dir, manifests_dir).await
}

/// Sync knowledge documents for a single pack.
pub async fn sync_pack(
    api: &ApiClient,
    pack_dir: &Path,
    pack_slug: &str,
    state_dir: &Path,
    manifests_dir: &Path,
) -> Result<()> {
    ensure_library_knowledge_pack_registered(manifests_dir, pack_dir, pack_slug).await?;

    let remote_docs = match api.list_knowledge_docs(pack_slug).await {
        Ok(docs) => docs,
        Err(e) => {
            warn!(
                pack_slug = %pack_slug,
                error = %e,
                "Failed to list knowledge documents — skipping sync"
            );
            return Ok(());
        }
    };

    let manifest = load_library_knowledge_manifest(pack_dir);
    let diff = compute_diff(manifest.as_ref(), &remote_docs);

    if diff.to_download.is_empty() && diff.to_delete.is_empty() && diff.to_rename.is_empty() {
        if let Err(error) = reconcile_knowledge_document_resource_ids(
            manifests_dir,
            pack_slug,
            &remote_docs,
            manifest.as_ref(),
        ) {
            warn!(
                pack_slug = %pack_slug,
                error = %error,
                "Failed to reconcile knowledge document resource ids"
            );
        }
        write_library_knowledge_manifest(
            pack_dir,
            &build_library_knowledge_manifest(pack_slug, &remote_docs),
        )?;
        debug!(
            pack_slug = %pack_slug,
            "Knowledge documents up to date; refreshed local manifest metadata"
        );
        return Ok(());
    }

    info!(
        pack_slug = %pack_slug,
        downloads = diff.to_download.len(),
        renames = diff.to_rename.len(),
        deletes = diff.to_delete.len(),
        "Knowledge pack sync started"
    );

    // Track which library documents were successfully downloaded.
    let mut failed_docs: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Ensure the local docs directory exists inside the library pack.
    let docs_dir = pack_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("Failed to create docs dir: {}", docs_dir.display()))?;

    // Download new/changed library documents.
    for doc in &diff.to_download {
        match api.get_knowledge_doc_content(pack_slug, &doc.slug).await {
            Ok(response) => {
                let content = resolve_document_content(
                    state_dir,
                    &response.encrypted_payload,
                    response.content,
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to resolve content for library document {}",
                        doc.slug
                    )
                })?;

                write_document_content(pack_dir, &doc.library_doc_relative_path(), &content)?;
                debug!(doc = %doc.slug, filename = %doc.filename, "Downloaded knowledge document");
            }
            Err(e) => {
                warn!(
                    doc = %doc.slug,
                    filename = %doc.filename,
                    error = %e,
                    "Failed to download knowledge document — skipping"
                );
                failed_docs.insert(doc.slug.clone());
            }
        }
    }

    for rename in &diff.to_rename {
        rename_document_file(pack_dir, &rename.from, &rename.to)?;
        debug!(from = %rename.from, to = %rename.to, "Renamed library document file");
    }

    // Delete removed library documents.
    for relative_path in &diff.to_delete {
        let path = docs_dir.join(relative_path);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete {}", path.display()))?;
            debug!(path = %relative_path, "Deleted removed library document file");
        }
    }

    let synced_docs = remote_docs
        .iter()
        .filter(|doc| !failed_docs.contains(&doc.slug))
        .cloned()
        .collect::<Vec<_>>();
    if let Err(error) = reconcile_knowledge_document_resource_ids(
        manifests_dir,
        pack_slug,
        &synced_docs,
        manifest.as_ref(),
    ) {
        warn!(
            pack_slug = %pack_slug,
            error = %error,
            "Failed to reconcile knowledge document resource ids"
        );
    }
    write_library_knowledge_manifest(
        pack_dir,
        &build_library_knowledge_manifest(pack_slug, &synced_docs),
    )?;
    info!(
        pack_slug = %pack_slug,
        downloaded = diff.to_download.len().saturating_sub(failed_docs.len()),
        renamed = diff.to_rename.len(),
        deleted = diff.to_delete.len(),
        failed = failed_docs.len(),
        "Knowledge pack sync completed"
    );

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
    api: &ApiClient,
    pack_dir: &Path,
    doc_slug: &nenjo::Slug,
    state_dir: &Path,
    manifests_dir: &Path,
    metadata: Option<&KnowledgeDocumentRecord>,
) -> Result<()> {
    let pack_slug = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("default")
        .to_string();
    ensure_library_knowledge_pack_registered(manifests_dir, pack_dir, &pack_slug).await?;
    let mut resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_knowledge_docs(&pack_slug)
            .await?
            .into_iter()
            .find(|doc| nenjo::Slug::derive(&doc.slug) == *doc_slug)
            .ok_or_else(|| {
                anyhow::anyhow!("knowledge document not found in metadata list: {doc_slug}")
            })?
    };
    let response = api
        .get_knowledge_doc_content(&pack_slug, &resolved_meta.slug)
        .await?;
    let content =
        resolve_document_content(state_dir, &response.encrypted_payload, response.content).await?;
    write_document_content(
        pack_dir,
        &resolved_meta.library_doc_relative_path(),
        &content,
    )?;

    if resolved_meta.edges.is_empty() {
        resolved_meta.edges = api
            .list_knowledge_doc_edges(&pack_slug, &resolved_meta.slug)
            .await
            .unwrap_or_default();
    }

    upsert_library_knowledge_entry(pack_dir, &pack_slug, &resolved_meta)?;
    Ok(())
}

pub async fn sync_document_metadata(
    api: &ApiClient,
    pack_dir: &Path,
    doc_slug: &nenjo::Slug,
    manifests_dir: &Path,
    metadata: Option<&KnowledgeDocumentRecord>,
    edges: Option<DocumentEdgesSource<'_>>,
) -> Result<()> {
    let pack_slug = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("default")
        .to_string();
    ensure_library_knowledge_pack_registered(manifests_dir, pack_dir, &pack_slug).await?;
    let mut resolved_meta = if let Some(metadata) = metadata.cloned() {
        metadata
    } else {
        api.list_knowledge_docs(&pack_slug)
            .await?
            .into_iter()
            .find(|doc| nenjo::Slug::derive(&doc.slug) == *doc_slug)
            .ok_or_else(|| {
                anyhow::anyhow!("knowledge document not found in metadata list: {doc_slug}")
            })?
    };

    resolved_meta.edges = match edges {
        Some(DocumentEdgesSource::Inline(edges)) => edges.to_vec(),
        Some(DocumentEdgesSource::FetchFromApi) | None => {
            if !resolved_meta.edges.is_empty() {
                resolved_meta.edges.clone()
            } else {
                api.list_knowledge_doc_edges(&pack_slug, &resolved_meta.slug)
                    .await
                    .unwrap_or_default()
            }
        }
    };

    if let Some(existing) = library_knowledge_doc_relative_path(pack_dir, doc_slug) {
        reconcile_document_file_location(
            pack_dir,
            &existing,
            &resolved_meta.library_doc_relative_path(),
        )?;
    }

    upsert_library_knowledge_entry_with_edges(
        pack_dir,
        &pack_slug,
        &resolved_meta,
        ReplaceDocumentEdges::Yes,
    )?;
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
    use nenjo::ManifestReader;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn meta(id: u128, filename: &str, updated: &str) -> KnowledgeDocumentRecord {
        let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
            .map(|value| value.with_timezone(&chrono::Utc))
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(updated, "%Y-%m-%d")
                    .map(|date| date.and_hms_opt(0, 0, 0).unwrap().and_utc())
            })
            .unwrap_or_else(|_| chrono::Utc::now());
        KnowledgeDocumentRecord {
            id: Uuid::from_u128(id),
            org_id: Uuid::from_u128(8),
            pack_id: Uuid::from_u128(7),
            pack_slug: "test".into(),
            slug: format!("doc_{id}"),
            filename: filename.into(),
            path: None,
            title: None,
            kind: None,
            summary: None,
            tags: Vec::new(),
            content_type: "text/markdown".into(),
            created_at: updated_at,
            updated_at,
            edges: Vec::new(),
        }
    }

    fn manifest(docs: Vec<KnowledgeDocumentRecord>) -> LibraryKnowledgePackManifest {
        build_library_knowledge_manifest("test", &docs)
    }

    #[test]
    fn diff_no_manifest_downloads_all() {
        let remote = vec![
            meta(1, "arch.md", "2026-01-01"),
            meta(2, "req.md", "2026-01-01"),
        ];
        let diff = compute_diff(None, &remote);
        assert_eq!(diff.to_download.len(), 2);
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_unchanged_skips() {
        let manifest = manifest(vec![meta(1, "arch.md", "2026-01-01")]);
        let remote = vec![meta(1, "arch.md", "2026-01-01")];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_updated_downloads() {
        let manifest = manifest(vec![meta(1, "arch.md", "2026-01-01")]);
        let remote = vec![meta(1, "arch.md", "2026-02-01")];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert!(diff.to_delete.is_empty());
    }

    #[test]
    fn diff_deleted_remotely() {
        let manifest = manifest(vec![
            meta(1, "arch.md", "2026-01-01"),
            meta(2, "old.md", "2026-01-01"),
        ]);
        let remote = vec![meta(1, "arch.md", "2026-01-01")];
        let diff = compute_diff(Some(&manifest), &remote);
        assert!(diff.to_download.is_empty());
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_new_and_deleted() {
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01")]);
        let remote = vec![meta(2, "new.md", "2026-02-01")];
        let diff = compute_diff(Some(&manifest), &remote);
        assert_eq!(diff.to_download.len(), 1);
        assert!(diff.to_rename.is_empty());
        assert_eq!(diff.to_download[0].filename, "new.md");
        assert_eq!(diff.to_delete, vec!["old.md"]);
    }

    #[test]
    fn diff_renamed_file_without_content_change_renames_only() {
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01")]);
        let remote = vec![meta(1, "new.md", "2026-01-01")];
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
        let manifest = manifest(vec![meta(1, "old.md", "2026-01-01")]);
        let remote = vec![meta(1, "new.md", "2026-02-01")];
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
    fn library_knowledge_manifest_keeps_traversal_metadata_concise() {
        let mut doc = meta(1, "random.md", "2026-02-22");
        doc.path = Some("domain/path".into());
        doc.title = Some("Random".into());
        doc.summary = Some("Just a test document".into());

        let manifest = build_library_knowledge_manifest("test", &[doc]);

        assert_eq!(manifest.docs.len(), 1);
        assert_eq!(manifest.docs[0].summary, "Just a test document");
        assert_eq!(
            manifest.docs[0].selector,
            "library://test/domain/path/random.md"
        );
    }

    #[test]
    fn library_doc_metadata_persists_to_knowledge_manifest() {
        let dir = tempdir().unwrap();
        let updated = "2026-02-22T00:00:00Z";
        let mut doc = meta(1, "random.md", updated);
        doc.path = Some("domain".into());
        doc.title = Some("Random".into());
        doc.kind = Some("guide".into());
        doc.summary = Some("Just a test document".into());
        doc.tags = vec!["library".into()];

        upsert_library_knowledge_entry(dir.path(), "test", &doc).unwrap();

        let knowledge = load_library_knowledge_manifest(dir.path()).unwrap();
        assert_eq!(knowledge.docs.len(), 1);
        assert_eq!(knowledge.docs[0].title, "Random");
        assert_eq!(knowledge.docs[0].summary, "Just a test document");
        assert_eq!(knowledge.docs[0].updated_at, doc.updated_at_rfc3339());
    }

    #[tokio::test]
    async fn sync_document_metadata_repairs_missing_pack_registry_entry() {
        let temp = tempdir().unwrap();
        let manifests_dir = temp.path().join("manifests");
        let library_dir = temp.path().join("library");
        let pack_dir = library_dir.join("humanizer");
        let api = ApiClient::new("http://127.0.0.1:9", "test");
        let mut doc = meta(1, "intro.md", "2026-02-22T00:00:00Z");
        doc.pack_slug = "humanizer".to_string();
        doc.slug = "intro".to_string();

        sync_document_metadata(
            &api,
            &pack_dir,
            &Slug::derive("intro"),
            &manifests_dir,
            Some(&doc),
            Some(DocumentEdgesSource::Inline(&[])),
        )
        .await
        .unwrap();

        let store = nenjo::LocalManifestStore::new(&manifests_dir);
        let manifest = store.load_manifest().await.unwrap();
        let pack = manifest
            .knowledge_packs
            .iter()
            .find(|pack| pack.slug.as_str() == "humanizer")
            .unwrap();
        assert_eq!(pack.selector, "lib:humanizer");
        assert_eq!(pack.root_path, Some(pack_dir.clone()));
        assert!(
            load_library_knowledge_manifest(&pack_dir)
                .unwrap()
                .doc_by_slug(&Slug::derive("intro"))
                .is_some()
        );
    }

    fn pack_meta(slug: &str) -> KnowledgePackRecord {
        let now = chrono::Utc::now();
        KnowledgePackRecord {
            id: Uuid::new_v4(),
            slug: slug.to_string(),
            name: slug.to_string(),
            description: None,
            source_type: "uploaded".to_string(),
            read_only: false,
            metadata: serde_json::Value::Null,
            selector: None,
            version: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn reconcile_library_knowledge_packs_removes_stale_dirs_and_sidecar_entries() {
        let nenjo_home = tempdir().unwrap();
        let manifests_dir = tempdir().unwrap();
        let library_dir = nenjo_home.path().join("library");
        let kept_dir = library_dir.join("product");
        let stale_dir = library_dir.join("removed");
        std::fs::create_dir_all(kept_dir.join("docs")).unwrap();
        std::fs::create_dir_all(stale_dir.join("docs")).unwrap();

        let store = PlatformResourceIdStore::new(manifests_dir.path());
        let doc = Slug::parse("overview").unwrap();
        store
            .upsert_knowledge_document(&Slug::parse("product").unwrap(), &doc, Uuid::new_v4())
            .unwrap();
        store
            .upsert_knowledge_document(&Slug::parse("removed").unwrap(), &doc, Uuid::new_v4())
            .unwrap();

        reconcile_library_knowledge_packs(
            &library_dir,
            manifests_dir.path(),
            &[pack_meta("product")],
        )
        .unwrap();

        assert!(kept_dir.exists());
        assert!(!stale_dir.exists());
        assert!(
            store
                .get_knowledge_document(&Slug::parse("product").unwrap(), &doc)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .get_knowledge_document(&Slug::parse("removed").unwrap(), &doc)
                .unwrap(),
            None
        );
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
