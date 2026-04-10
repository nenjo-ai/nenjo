//! Manifest change handler — incremental resource updates.

use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo_events::{ResourceAction, ResourceType};

use crate::harness::CommandContext;
use crate::loader::FileSystemManifestLoader;

static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Handle a manifest.changed event.
///
/// Fetches only the changed resource and applies an incremental update to
/// the manifest. Falls back to a full refresh if the fetch fails.
pub async fn handle_manifest_changed(
    ctx: &CommandContext,
    resource_type: ResourceType,
    resource_id: Uuid,
    action: ResourceAction,
    project_id: Option<Uuid>,
    payload: Option<serde_json::Value>,
) -> Result<()> {
    info!(%resource_type, %resource_id, ?action, inline = payload.is_some(), "Manifest resource changed");

    if action == ResourceAction::Deleted {
        apply_delete(ctx, resource_type, resource_id);
    } else {
        // Try inline payload first, fall back to API fetch.
        let applied = if let Some(ref data) = payload {
            apply_inline_upsert(ctx, resource_type, resource_id, data)
        } else {
            false
        };

        if !applied && let Err(e) = apply_upsert(ctx, resource_type, resource_id).await {
            warn!(
                error = %e,
                %resource_type,
                %resource_id,
                "Incremental fetch failed, falling back to full refresh"
            );
            full_refresh(ctx).await?;
            return Ok(());
        }
    }

    // Side-effects for specific resource types
    match resource_type {
        ResourceType::McpServer => {
            let manifest = ctx.provider().manifest().clone();
            let servers = crate::harness::override_platform_mcp_url(
                manifest.mcp_servers,
                ctx.config.backend_api_url(),
            );
            ctx.external_mcp.reconcile(&servers).await;
        }
        ResourceType::Lambda => {
            let manifest = ctx.provider().manifest().clone();
            let _ = crate::manifest::sync_lambdas(&ctx.config.workspace_dir, &manifest.lambdas);
        }
        ResourceType::Document => {
            // Sync docs for the specific project that owns this document.
            if let Some(pid) = project_id {
                let manifest = ctx.provider().manifest().clone();
                let slug = manifest
                    .projects
                    .iter()
                    .find(|p| p.id == pid)
                    .map(|p| p.slug.clone())
                    .unwrap_or_else(|| pid.to_string());
                let project_dir = ctx.config.workspace_dir.join(&slug);
                if let Err(e) = crate::doc_sync::sync_project(&ctx.api, &project_dir, pid).await {
                    warn!(%pid, error = %e, "Doc sync failed");
                }
            } else {
                warn!("Document change without project_id, skipping sync");
            }
        }
        ResourceType::Project => {
            // New or updated project — sync its documents (skip system projects).
            let manifest = ctx.provider().manifest().clone();
            if let Some(project) = manifest
                .projects
                .iter()
                .find(|p| p.id == resource_id && !p.is_system)
            {
                let project_dir = ctx.config.workspace_dir.join(&project.slug);
                if let Err(e) =
                    crate::doc_sync::sync_project(&ctx.api, &project_dir, project.id).await
                {
                    warn!(project_id = %project.id, error = %e, "Doc sync failed");
                }
            }
        }
        _ => {}
    }

    // Persist the changed resource to the filesystem cache
    persist_cache(ctx, resource_type);

    Ok(())
}

/// Apply an inline payload directly to the manifest without an API fetch.
/// Returns `true` if the payload was successfully applied, `false` if
/// deserialization failed (caller should fall back to API fetch).
fn apply_inline_upsert(
    ctx: &CommandContext,
    rt: ResourceType,
    id: Uuid,
    data: &serde_json::Value,
) -> bool {
    let mut manifest = ctx.provider().manifest().clone();

    macro_rules! inline_upsert {
        ($field:ident, $ty:ty) => {{
            match serde_json::from_value::<$ty>(data.clone()) {
                Ok(item) => {
                    if let Some(pos) = manifest.$field.iter().position(|r| r.id == id) {
                        manifest.$field[pos] = item;
                    } else {
                        manifest.$field.push(item);
                    }
                    debug!(%rt, %id, "Applied inline resource payload");
                    true
                }
                Err(e) => {
                    warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                    false
                }
            }
        }};
    }

    let ok = match rt {
        ResourceType::Agent => inline_upsert!(agents, nenjo::manifest::AgentManifest),
        ResourceType::Model => inline_upsert!(models, nenjo::manifest::ModelManifest),
        ResourceType::Routine => inline_upsert!(routines, nenjo::manifest::RoutineManifest),
        ResourceType::Project => inline_upsert!(projects, nenjo::manifest::ProjectManifest),
        ResourceType::Council => inline_upsert!(councils, nenjo::manifest::CouncilManifest),
        ResourceType::Lambda => inline_upsert!(lambdas, nenjo::manifest::LambdaManifest),
        ResourceType::Ability => inline_upsert!(abilities, nenjo::manifest::AbilityManifest),
        ResourceType::ContextBlock => {
            inline_upsert!(context_blocks, nenjo::manifest::ContextBlockManifest)
        }
        ResourceType::McpServer => inline_upsert!(mcp_servers, nenjo::manifest::McpServerManifest),
        ResourceType::Domain => inline_upsert!(domains, nenjo::manifest::DomainManifest),
        ResourceType::Document => return false, // documents don't live in manifest
    };

    if ok {
        ctx.swap_provider(ctx.provider().with_manifest(manifest));
    }
    ok
}

/// Remove a deleted resource from the in-memory manifest.
fn apply_delete(ctx: &CommandContext, rt: ResourceType, id: Uuid) {
    let mut manifest = ctx.provider().manifest().clone();

    match rt {
        ResourceType::Agent => manifest.agents.retain(|r| r.id != id),
        ResourceType::Model => manifest.models.retain(|r| r.id != id),
        ResourceType::Routine => manifest.routines.retain(|r| r.id != id),
        ResourceType::Project => manifest.projects.retain(|r| r.id != id),
        ResourceType::Council => manifest.councils.retain(|r| r.id != id),
        ResourceType::Lambda => manifest.lambdas.retain(|r| r.id != id),
        ResourceType::Ability => manifest.abilities.retain(|r| r.id != id),
        ResourceType::ContextBlock => manifest.context_blocks.retain(|r| r.id != id),
        ResourceType::McpServer => manifest.mcp_servers.retain(|r| r.id != id),
        ResourceType::Domain => manifest.domains.retain(|r| r.id != id),
        ResourceType::Document => return,
    }

    info!(%rt, %id, "Removed deleted resource from manifest");
    ctx.swap_provider(ctx.provider().with_manifest(manifest));
}

/// Fetch a single resource from the API and upsert it into the manifest.
async fn apply_upsert(ctx: &CommandContext, rt: ResourceType, id: Uuid) -> Result<()> {
    let mut manifest = ctx.provider().manifest().clone();

    macro_rules! upsert {
        ($field:ident, $fetch:ident) => {{
            match ctx.api.$fetch(id).await? {
                Some(item) => {
                    if let Some(pos) = manifest.$field.iter().position(|r| r.id == id) {
                        manifest.$field[pos] = item;
                        debug!(%rt, %id, "Updated existing resource");
                    } else {
                        manifest.$field.push(item);
                        debug!(%rt, %id, "Added new resource");
                    }
                }
                None => {
                    manifest.$field.retain(|r| r.id != id);
                    debug!(%rt, %id, "Resource returned 404, removing");
                }
            }
        }};
    }

    match rt {
        ResourceType::Agent => upsert!(agents, fetch_agent),
        ResourceType::Model => upsert!(models, fetch_model),
        ResourceType::Routine => upsert!(routines, fetch_routine),
        ResourceType::Project => upsert!(projects, fetch_project),
        ResourceType::Council => upsert!(councils, fetch_council),
        ResourceType::Lambda => upsert!(lambdas, fetch_lambda),
        ResourceType::Ability => upsert!(abilities, fetch_ability),
        ResourceType::ContextBlock => upsert!(context_blocks, fetch_context_block),
        ResourceType::McpServer => upsert!(mcp_servers, fetch_mcp_server),
        ResourceType::Domain => upsert!(domains, fetch_domain),
        ResourceType::Document => return Ok(()),
    }

    ctx.swap_provider(ctx.provider().with_manifest(manifest));
    Ok(())
}

/// Full re-fetch of all manifest data (fallback).
async fn full_refresh(ctx: &CommandContext) -> Result<()> {
    crate::manifest::sync(
        &ctx.api,
        &ctx.config.manifests_dir,
        &ctx.config.workspace_dir,
    )
    .await?;

    let loader = FileSystemManifestLoader::new(&ctx.config.manifests_dir);
    let manifest = nenjo::ManifestLoader::load(&loader).await?;

    let servers = crate::harness::override_platform_mcp_url(
        manifest.mcp_servers.clone(),
        ctx.config.backend_api_url(),
    );
    ctx.external_mcp.reconcile(&servers).await;

    ctx.swap_provider(ctx.provider().with_manifest(manifest));

    info!("Full manifest refresh complete");
    Ok(())
}

/// Persist the current manifest to the filesystem cache for this resource type.
fn persist_cache(ctx: &CommandContext, rt: ResourceType) {
    let manifest = ctx.provider().manifest().clone();
    let manifests_dir = &ctx.config.manifests_dir;

    let result = match rt {
        ResourceType::Model => atomic_write(manifests_dir, "models.json", &manifest.models),
        ResourceType::Agent => atomic_write(manifests_dir, "agents.json", &manifest.agents),
        ResourceType::Routine => atomic_write(manifests_dir, "routines.json", &manifest.routines),
        ResourceType::Project => atomic_write(manifests_dir, "projects.json", &manifest.projects),
        ResourceType::Council => atomic_write(manifests_dir, "councils.json", &manifest.councils),
        ResourceType::Lambda => atomic_write(manifests_dir, "lambdas.json", &manifest.lambdas),
        ResourceType::Ability => {
            crate::manifest::sync_tree(&manifests_dir.join("abilities"), &manifest.abilities)
        }
        ResourceType::ContextBlock => crate::manifest::sync_tree(
            &manifests_dir.join("context_blocks"),
            &manifest.context_blocks,
        ),
        ResourceType::McpServer => {
            atomic_write(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)
        }
        ResourceType::Domain => {
            crate::manifest::sync_tree(&manifests_dir.join("domains"), &manifest.domains)
        }
        ResourceType::Document => return,
    };

    if let Err(e) = result {
        warn!(error = %e, %rt, "Failed to persist resource cache");
    }
}

fn atomic_write<T: serde::Serialize>(
    dir: &std::path::Path,
    filename: &str,
    value: &T,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(filename);
    let tmp = unique_tmp_path(&target, filename);
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

fn unique_tmp_path(target: &std::path::Path, filename: &str) -> std::path::PathBuf {
    let nonce = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    target.with_file_name(format!(".{filename}.{pid}.{nonce}.tmp"))
}
