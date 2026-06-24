use anyhow::anyhow;

use crate::{PackageKind, ResolvedModule, ResolvedPackage, validate_source_path};

pub(crate) fn validate_assignments(
    packages: &std::collections::BTreeMap<String, ResolvedPackage>,
    module: &ResolvedModule,
) -> anyhow::Result<()> {
    let Some(assignments) = module.manifest.manifest.get("assignments") else {
        return Ok(());
    };
    match module.kind {
        PackageKind::Agent => {
            validate_assignment_field(packages, assignments, "abilities", PackageKind::Ability)?;
            validate_assignment_field(packages, assignments, "domains", PackageKind::Domain)?;
            validate_assignment_field(
                packages,
                assignments,
                "mcp_servers",
                PackageKind::McpServer,
            )?;
            validate_assignment_field(
                packages,
                assignments,
                "script_tools",
                PackageKind::ScriptTool,
            )?;
        }
        PackageKind::Domain => {
            validate_assignment_field(packages, assignments, "abilities", PackageKind::Ability)?;
            validate_assignment_field(
                packages,
                assignments,
                "mcp_servers",
                PackageKind::McpServer,
            )?;
            validate_assignment_field(
                packages,
                assignments,
                "script_tools",
                PackageKind::ScriptTool,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_assignment_field(
    packages: &std::collections::BTreeMap<String, ResolvedPackage>,
    assignments: &serde_json::Value,
    field: &str,
    expected: PackageKind,
) -> anyhow::Result<()> {
    let Some(value) = assignments.get(field) else {
        return Ok(());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("manifest.assignments.{field} must be an array"))?;
    for item in items {
        let path = item
            .as_str()
            .ok_or_else(|| anyhow!("manifest.assignments.{field} entries must be package paths"))?;
        let path = validate_source_path(path)?;
        let Some(target) = find_module_by_source_path(packages, &path) else {
            anyhow::bail!(
                "manifest.assignments.{field} references package path '{path}' that was not resolved"
            );
        };
        if target.kind != expected {
            anyhow::bail!(
                "manifest.assignments.{field} references {path}, but it is {} not {}",
                target.kind.as_str(),
                expected.as_str()
            );
        }
    }
    Ok(())
}

pub(crate) fn find_module_by_source_path<'a>(
    packages: &'a std::collections::BTreeMap<String, ResolvedPackage>,
    path: &str,
) -> Option<&'a ResolvedModule> {
    packages
        .values()
        .flat_map(|package| package.modules.values())
        .find(|module| module.source_path == path || module.path == path)
}
