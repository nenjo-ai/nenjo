use anyhow::anyhow;

use crate::{
    PackageKind, PackageResourceLogicalKey, ResolvedModule, ResolvedPackage, validate_source_path,
};

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
        let reference = item
            .as_str()
            .ok_or_else(|| anyhow!("manifest.assignments.{field} entries must be package refs"))?;
        if reference.starts_with("pkg:") {
            let logical_ref = PackageResourceLogicalKey::parse(reference)?;
            if logical_ref.kind() != Some(expected) {
                anyhow::bail!(
                    "manifest.assignments.{field} references {reference}, but it is {} not {}",
                    logical_ref
                        .kind()
                        .map(PackageKind::as_str)
                        .unwrap_or("invalid"),
                    expected.as_str()
                );
            }
            let matches = find_modules_by_logical_ref(packages, &logical_ref)?;
            match matches.as_slice() {
                [_] => continue,
                [] => anyhow::bail!(
                    "manifest.assignments.{field} references logical ref '{reference}' that was not resolved"
                ),
                _ => anyhow::bail!(
                    "manifest.assignments.{field} references ambiguous logical ref '{reference}'"
                ),
            }
        }

        let path = validate_source_path(reference)?;
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

fn find_modules_by_logical_ref<'a>(
    packages: &'a std::collections::BTreeMap<String, ResolvedPackage>,
    logical_ref: &PackageResourceLogicalKey,
) -> anyhow::Result<Vec<&'a ResolvedModule>> {
    let repository = logical_ref
        .repository()
        .ok_or_else(|| anyhow!("logical ref is missing GitHub repository"))?;
    let package_slug = logical_ref
        .package()
        .ok_or_else(|| anyhow!("logical ref is missing package"))?;
    let resource_slug = logical_ref
        .resource_slug()
        .ok_or_else(|| anyhow!("logical ref is missing resource slug"))?;
    let owner = repository
        .trim_start_matches('@')
        .split_once('/')
        .map(|(owner, _)| owner)
        .ok_or_else(|| anyhow!("logical ref has invalid GitHub repository"))?;
    let scoped_package = format!("@{owner}/{package_slug}");

    Ok(packages
        .values()
        .filter(|package| package.name == scoped_package || package.name == package_slug)
        .flat_map(|package| {
            package
                .modules
                .iter()
                .filter(|(key, module)| *key == &module.key())
                .map(|(_, module)| module)
        })
        .filter(|module| {
            module.kind == logical_ref.kind().expect("logical ref kind was parsed")
                && module.manifest.slug() == Some(resource_slug)
        })
        .collect())
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
