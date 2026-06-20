use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, anyhow};
use async_trait::async_trait;

use crate::command_content::command_content_path_candidates;
use crate::identity::local_import_module_path;
use crate::module::index_child_module_path;
use crate::{
    ModuleImport, ModuleIndexManifest, ModulePackageManifest, PackageKind, PackageModule,
    PackageRegistryManifest, ResolvedModule, ResolvedPackage, ResolvedPackageFile,
    ResolvedPackageGraph, complete_package_resource_manifest, module_file_schema,
    module_reference_is_directory, normalize_module_reference, package_module_source_path,
    parse_json_or_yaml_as, parse_module_file, sha256_hex, validate_source_path, version_satisfies,
};

#[async_trait]
pub trait PackageFileReader: Send + Sync {
    async fn read_text(&self, path: &str) -> anyhow::Result<String>;
}

pub async fn resolve_module_package_graph_from_reader<R>(
    reader: &R,
    registry_path: &str,
    root_path: &str,
) -> anyhow::Result<ResolvedPackageGraph>
where
    R: PackageFileReader + ?Sized,
{
    let registry = load_repository_manifest(reader, registry_path).await?;
    let root_package = load_module_package(reader, root_path).await?;
    let root_name = registry
        .packages
        .iter()
        .find_map(|(name, path)| {
            validate_source_path(path)
                .ok()
                .filter(|path| path == root_path)
                .map(|_| name.clone())
        })
        .unwrap_or_else(|| root_package.manifest.name.clone());

    resolve_module_package_graph_inner(reader, registry, root_package, root_name).await
}

pub async fn resolve_module_package_manifest_from_reader<R>(
    reader: &R,
    package_path: &str,
) -> anyhow::Result<ResolvedPackage>
where
    R: PackageFileReader + ?Sized,
{
    let package = load_module_package(reader, package_path).await?;
    let package = resolve_package_modules(reader, package).await?;
    package.validate_knowledge_pack_names()?;
    Ok(package)
}

struct LoadedModulePackage {
    path: String,
    manifest: ModulePackageManifest,
    content: String,
}

async fn load_repository_manifest<R>(
    reader: &R,
    registry_path: &str,
) -> anyhow::Result<PackageRegistryManifest>
where
    R: PackageFileReader + ?Sized,
{
    let content = reader.read_text(registry_path).await?;
    let manifest: PackageRegistryManifest =
        parse_json_or_yaml_as(&content).context("failed to parse package repository")?;
    manifest.validate()?;
    Ok(manifest)
}

async fn load_module_package<R>(reader: &R, path: &str) -> anyhow::Result<LoadedModulePackage>
where
    R: PackageFileReader + ?Sized,
{
    let path = validate_source_path(path)?;
    let content = reader.read_text(&path).await?;
    let manifest: ModulePackageManifest = parse_json_or_yaml_as(&content)
        .with_context(|| format!("failed to parse package manifest {path}"))?;
    manifest.validate(&path)?;
    Ok(LoadedModulePackage {
        path,
        manifest,
        content,
    })
}

async fn resolve_module_package_graph_inner<R>(
    reader: &R,
    registry: PackageRegistryManifest,
    root_package: LoadedModulePackage,
    root_name: String,
) -> anyhow::Result<ResolvedPackageGraph>
where
    R: PackageFileReader + ?Sized,
{
    let mut packages: BTreeMap<String, LoadedModulePackage> = BTreeMap::new();
    let mut stack = vec![root_package];
    while let Some(package) = stack.pop() {
        if let Some(existing) = packages.get(&package.manifest.name) {
            validate_package_version_requirement(
                &package.manifest.name,
                &existing.manifest.version,
                &package.manifest.version,
            )?;
            continue;
        }
        for (dependency, requirement) in &package.manifest.dependencies {
            let dependency_path = registry.packages.get(dependency).ok_or_else(|| {
                anyhow!(
                    "{} depends on {dependency}, but packages.yaml does not list it",
                    package.manifest.name
                )
            })?;
            let dependency_package = load_module_package(reader, dependency_path).await?;
            validate_package_version_requirement(
                dependency,
                &dependency_package.manifest.version,
                requirement,
            )?;
            stack.push(dependency_package);
        }
        packages.insert(package.manifest.name.clone(), package);
    }

    let mut resolved = BTreeMap::new();
    for (name, package) in packages {
        resolved.insert(name, resolve_package_modules(reader, package).await?);
    }
    let graph = ResolvedPackageGraph {
        root_package: root_name,
        packages: resolved,
    };
    graph.validate_versions()?;
    Ok(graph)
}

fn validate_package_version_requirement(
    package: &str,
    actual: &str,
    required: &str,
) -> anyhow::Result<()> {
    if !version_satisfies(actual, required) {
        anyhow::bail!("{package} requires version {required}, got {actual}");
    }
    Ok(())
}

async fn resolve_package_modules<R>(
    reader: &R,
    package: LoadedModulePackage,
) -> anyhow::Result<ResolvedPackage>
where
    R: PackageFileReader + ?Sized,
{
    let mut modules = BTreeMap::new();
    let mut pending = package.manifest.modules.clone();
    let mut index_stack = BTreeSet::new();
    let mut expanded_modules = BTreeSet::new();

    while let Some(module) = pending.pop() {
        let module_files = expand_package_module(reader, &package.path, &module, &mut index_stack)
            .await
            .with_context(|| format!("failed to expand module '{}'", module.path))?;
        for module_file in module_files {
            if !expanded_modules.insert(module_file.path.clone()) {
                continue;
            }
            let imports =
                insert_resolved_module_file(reader, &module_file, &package, &mut modules).await?;
            for import in imports {
                if !import.is_local_module_ref() {
                    continue;
                }
                let imported = local_import_module_path(&module_file.path, &import.reference)
                    .with_context(|| {
                        format!(
                            "failed to resolve import '{}' from {}",
                            import.reference, module_file.path
                        )
                    })?;
                pending.push(PackageModule {
                    path: imported,
                    metadata: serde_json::Value::Null,
                });
            }
        }
    }

    Ok(ResolvedPackage {
        name: package.manifest.name.clone(),
        path: package.path,
        version: package.manifest.version.clone(),
        hash: sha256_hex(package.content.as_bytes()),
        manifest: package.manifest,
        modules,
    })
}

#[derive(Debug)]
struct ExpandedPackageModule {
    path: String,
    source_path: String,
    content: String,
}

async fn expand_package_module<R>(
    reader: &R,
    package_path: &str,
    module: &PackageModule,
    stack: &mut BTreeSet<String>,
) -> anyhow::Result<Vec<ExpandedPackageModule>>
where
    R: PackageFileReader + ?Sized,
{
    let mut expanded = Vec::new();
    let module_path = normalize_module_reference(&module.path)?;
    let (module_path, source_path) = if module_reference_is_directory(&module.path) {
        resolve_directory_index_path(reader, package_path, &module_path).await?
    } else {
        let source_path = package_module_source_path(package_path, &module_path)?;
        (module_path, source_path)
    };
    let module_content = reader.read_text(&source_path).await?;
    let schema = module_file_schema(&module_content, &source_path)?;
    if schema == "nenjo.module_index.v1" {
        Box::pin(expand_module_index(
            reader,
            package_path,
            &module_path,
            &source_path,
            stack,
            &mut expanded,
        ))
        .await?;
    } else {
        expanded.push(ExpandedPackageModule {
            path: module_path,
            source_path,
            content: module_content,
        });
    }
    Ok(expanded)
}

async fn expand_module_index<R>(
    reader: &R,
    package_path: &str,
    index_module_path: &str,
    index_source_path: &str,
    stack: &mut BTreeSet<String>,
    expanded: &mut Vec<ExpandedPackageModule>,
) -> anyhow::Result<()>
where
    R: PackageFileReader + ?Sized,
{
    if !stack.insert(index_source_path.to_string()) {
        anyhow::bail!("module index cycle includes {index_source_path}");
    }
    let index_content = reader.read_text(index_source_path).await?;
    let index: ModuleIndexManifest = parse_json_or_yaml_as(&index_content)
        .with_context(|| format!("failed to parse module index {index_source_path}"))?;
    index.validate(index_source_path)?;
    let index_dir = index_module_path.rsplit_once('/').map(|(dir, _)| dir);
    for module in &index.modules {
        let child = PackageModule {
            path: index_child_module_path(index_dir, &module.path)?,
            metadata: module.metadata.clone(),
        };
        let child_modules = Box::pin(expand_package_module(reader, package_path, &child, stack))
            .await
            .with_context(|| {
                format!(
                    "failed to expand module '{}' from index {index_source_path}",
                    module.path
                )
            })?;
        expanded.extend(child_modules);
    }
    stack.remove(index_source_path);
    Ok(())
}

async fn resolve_directory_index_path<R>(
    reader: &R,
    package_path: &str,
    module_path: &str,
) -> anyhow::Result<(String, String)>
where
    R: PackageFileReader + ?Sized,
{
    for index_name in ["index.yml", "index.yaml"] {
        let module_path = format!("{module_path}/{index_name}");
        let source_path = package_module_source_path(package_path, &module_path)?;
        if reader.read_text(&source_path).await.is_ok() {
            return Ok((module_path, source_path));
        }
    }
    let skill_module_path = format!("{module_path}/SKILL.md");
    let skill_source_path = package_module_source_path(package_path, &skill_module_path)?;
    if reader.read_text(&skill_source_path).await.is_ok() {
        return Ok((module_path.to_string(), skill_source_path));
    }
    anyhow::bail!("directory module '{module_path}/' requires index.yml, index.yaml, or SKILL.md")
}

async fn insert_resolved_module_file<R>(
    reader: &R,
    module: &ExpandedPackageModule,
    package: &LoadedModulePackage,
    modules: &mut BTreeMap<String, ResolvedModule>,
) -> anyhow::Result<Vec<ModuleImport>>
where
    R: PackageFileReader + ?Sized,
{
    let resources = parse_module_file(&module.content, &module.source_path)?;
    let multiple_resources = resources.len() > 1;
    let mut all_imports = Vec::new();
    for resource_manifest in resources {
        let resource_manifest =
            complete_package_resource_manifest(resource_manifest, &package.manifest)?;
        let kind = resource_manifest.kind()?;
        resource_manifest.name().with_context(|| {
            format!("failed to validate module manifest {}", module.source_path)
        })?;
        let resource_name = resource_manifest
            .name()
            .expect("resource name was just validated")
            .to_string();
        let imports = resource_manifest.imports();
        let files = resolved_module_files(reader, package, module, kind, &resource_manifest)
            .await
            .with_context(|| {
                format!("failed to resolve runtime files for {}", module.source_path)
            })?;
        all_imports.extend(imports.clone());
        let resolved = ResolvedModule {
            package_name: package.manifest.name.clone(),
            package_version: package.manifest.version.clone(),
            path: module.path.clone(),
            source_path: module.source_path.clone(),
            hash: sha256_hex(module.content.as_bytes()),
            kind,
            manifest: resource_manifest,
            imports,
            files,
        };
        let resource_key = format!("{}#{resource_name}", module.path);
        if modules
            .insert(resource_key.clone(), resolved.clone())
            .is_some()
        {
            anyhow::bail!(
                "{} declares duplicate resolved module '{resource_key}'",
                module.source_path
            );
        }
        if !multiple_resources && modules.insert(module.path.clone(), resolved).is_some() {
            anyhow::bail!(
                "{} declares duplicate resolved module '{}'",
                module.source_path,
                module.path
            );
        }
    }
    Ok(all_imports)
}

async fn resolved_module_files<R>(
    reader: &R,
    package: &LoadedModulePackage,
    module: &ExpandedPackageModule,
    kind: PackageKind,
    resource_manifest: &crate::ResourceManifest,
) -> anyhow::Result<Vec<ResolvedPackageFile>>
where
    R: PackageFileReader + ?Sized,
{
    let mut files = BTreeMap::new();
    if kind == PackageKind::Command
        && let Some(content_path) = resource_manifest
            .manifest
            .get("content_path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
    {
        let (path, content) = read_command_content_file(
            reader,
            &package.path,
            &module.path,
            &module.source_path,
            content_path,
        )
        .await?;
        files.insert(
            path.clone(),
            ResolvedPackageFile {
                path,
                hash: sha256_hex(content.as_bytes()),
            },
        );
    }
    Ok(files.into_values().collect())
}

async fn read_command_content_file<R>(
    reader: &R,
    package_path: &str,
    module_path: &str,
    module_source_path: &str,
    content_path: &str,
) -> anyhow::Result<(String, String)>
where
    R: PackageFileReader + ?Sized,
{
    let candidates = command_content_path_candidates(
        package_path,
        module_path,
        module_source_path,
        content_path,
    )?;
    let mut last_error = None;
    for candidate in candidates {
        match reader.read_text(&candidate.read_path).await {
            Ok(content) => return Ok((candidate.package_path, content)),
            Err(err) => last_error = Some(err),
        }
    }
    if let Some(err) = last_error {
        return Err(err).with_context(|| {
            format!(
                "command content_path '{content_path}' referenced by {module_source_path} was not found"
            )
        });
    }
    anyhow::bail!(
        "command content_path '{content_path}' referenced by {module_source_path} was not found"
    );
}
