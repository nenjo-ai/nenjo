use anyhow::{Context, Result};
use nenjo_events::PackageGraphUpdate;
use tracing::info;

use crate::bootstrap::{BootstrapPackages, PlatformPackageSyncStatus};
use crate::runtime::CommandContext;

pub async fn handle_package_graph_changed(
    ctx: &CommandContext,
    packages: PackageGraphUpdate,
) -> Result<()> {
    let dependency_count = nenjo_nenpm::DependencyManifest::parse_yaml(&packages.nenpm_yml)
        .map(|manifest| manifest.dependencies.len())
        .ok();
    let lock_package_count =
        serde_yaml::from_str::<nenjo_nenpm::NenpmLock>(&packages.nenpm_lock_yml)
            .map(|lock| lock.packages.len())
            .ok();
    info!(
        schema = %packages.schema,
        dependency_count,
        lock_package_count,
        nenpm_yml_bytes = packages.nenpm_yml.len(),
        nenpm_lock_yml_bytes = packages.nenpm_lock_yml.len(),
        "Applying platform package graph update"
    );

    let packages = BootstrapPackages {
        schema: packages.schema,
        nenpm_yml: packages.nenpm_yml,
        nenpm_lock_yml: packages.nenpm_lock_yml,
        argument_bindings: packages.argument_bindings.clone(),
    };

    let status = crate::bootstrap::sync_platform_packages(&ctx.config.config_dir, &packages)
        .await
        .context("failed to sync platform package graph")?;
    if status != PlatformPackageSyncStatus::Applied {
        info!(?status, "Skipped platform package graph update");
        return Ok(());
    }
    let manifest = crate::assembly::load_runtime_manifest(&ctx.config)
        .await
        .context("failed to reload runtime manifest after package graph update")?;
    let argument_bindings = crate::assembly::load_platform_package_argument_bindings(&ctx.config)
        .context("failed to load platform package argument bindings")?;
    ctx.external_mcp.reconcile(&manifest.mcp_servers).await;
    ctx.skill_registry
        .reconcile(&manifest.skills, &manifest.hooks);
    ctx.harness
        .manifests()
        .replace_with_argument_bindings(manifest, argument_bindings)
        .await
        .context("failed to replace harness runtime manifest after package graph update")?;

    info!("Applied platform package graph update");
    Ok(())
}
