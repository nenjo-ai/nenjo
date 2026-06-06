use anyhow::Result;
use nenjo_events::PackageGraphUpdate;
use tracing::info;

use crate::bootstrap::{BootstrapPackages, PlatformPackageSyncStatus};
use crate::runtime::CommandContext;

pub async fn handle_package_graph_changed(
    ctx: &CommandContext,
    packages: PackageGraphUpdate,
) -> Result<()> {
    let packages = BootstrapPackages {
        schema: packages.schema,
        nenpm_yml: packages.nenpm_yml,
        nenpm_lock_yml: packages.nenpm_lock_yml,
    };

    let status =
        crate::bootstrap::sync_platform_packages(&ctx.config.config_dir, &packages).await?;
    if status != PlatformPackageSyncStatus::Applied {
        info!(?status, "Skipped platform package graph update");
        return Ok(());
    }
    let manifest = crate::assembly::load_runtime_manifest(&ctx.config).await?;
    ctx.external_mcp.reconcile(&manifest.mcp_servers).await;
    ctx.skill_registry
        .reconcile(&manifest.skills, &manifest.hooks);
    ctx.harness.manifests().replace(manifest).await?;

    info!("Applied platform package graph update");
    Ok(())
}
