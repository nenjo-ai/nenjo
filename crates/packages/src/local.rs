use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::{Context, anyhow};

use crate::command_content::command_content_path_candidates;
use crate::identity::local_import_module_path;
use crate::module::index_child_module_path;
use crate::{
    ModuleImport, ModuleIndexManifest, ModulePackageManifest, PackageKind, PackageModule,
    PackageRegistryManifest, ResolvedModule, ResolvedPackage, ResolvedPackageFile,
    ResolvedPackageGraph, complete_package_resource_manifest, module_file_schema,
    module_reference_is_directory, normalize_module_reference, package_module_source_path,
    parse_json_or_yaml_as, parse_module_file, sha256_hex, validate_package_name,
    validate_source_path,
};

#[derive(Debug, Clone)]
pub struct LocalPackageResolver {
    root: PathBuf,
    registry_path: String,
}

impl LocalPackageResolver {
    /// Create a local resolver rooted at a package registry directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            registry_path: "packages.yaml".to_string(),
        }
    }

    /// Create a local resolver with an explicit registry manifest path.
    pub fn with_repository_path(
        root: impl Into<PathBuf>,
        registry_path: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            registry_path: registry_path.into(),
        }
    }

    /// Create a local resolver with an explicit registry manifest path.
    pub fn with_registry_path(root: impl Into<PathBuf>, registry_path: impl Into<String>) -> Self {
        Self::with_repository_path(root, registry_path)
    }

    /// Return the local registry root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the configured registry manifest path.
    pub fn repository_path(&self) -> &str {
        &self.registry_path
    }

    /// Return the configured registry manifest path.
    pub fn registry_path(&self) -> &str {
        &self.registry_path
    }

    /// Read a registry-relative text file from the local package registry.
    pub fn read_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        Ok(fs::read_to_string(self.root.join(&path))
            .with_context(|| format!("failed to read local package file {path}"))?)
    }

    /// Load and validate the local registry manifest.
    pub fn load_registry(&self) -> Result<PackageRegistryManifest> {
        let content = self.read_text(&self.registry_path)?;
        let registry: PackageRegistryManifest =
            parse_json_or_yaml_as(&content).context("failed to parse package registry")?;
        registry
            .validate()
            .context("failed to validate package registry")?;
        Ok(registry)
    }

    /// Resolve a package and its dependencies from the local registry.
    pub fn resolve_package_graph(&self, root_package: &str) -> Result<ResolvedPackageGraph> {
        validate_package_name(root_package)?;
        let registry = self.load_registry()?;
        let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
        let mut stack = vec![root_package.to_string()];

        while let Some(name) = stack.pop() {
            if packages.contains_key(&name) {
                continue;
            }
            let path = registry
                .packages
                .get(&name)
                .ok_or_else(|| anyhow!("package {name} is not listed in registry"))?;
            let package = self.resolve_package_manifest(path)?;
            if package.name != name {
                bail!(
                    "registry maps {name} to {path}, but package manifest declares {}",
                    package.name
                );
            }
            for dependency in package.dependencies().keys() {
                stack.push(dependency.clone());
            }
            packages.insert(name, package);
        }

        let graph = ResolvedPackageGraph {
            root_package: root_package.to_string(),
            packages,
        };
        graph.validate_versions()?;
        Ok(graph)
    }

    /// Resolve one package manifest and its included modules without following dependencies.
    pub fn resolve_package_manifest(&self, package_path: &str) -> Result<ResolvedPackage> {
        let package_path = validate_source_path(package_path)?;
        let package_content = self.read_text(&package_path)?;
        let manifest: ModulePackageManifest = parse_json_or_yaml_as(&package_content)
            .with_context(|| format!("failed to parse package manifest {package_path}"))?;
        manifest.validate(&package_path)?;

        let mut modules = BTreeMap::new();
        self.resolve_package_modules(&package_path, &manifest, &mut modules)
            .with_context(|| format!("failed to expand modules in {package_path}"))?;

        let package = ResolvedPackage {
            name: manifest.name.clone(),
            path: package_path,
            version: manifest.version.clone(),
            hash: sha256_hex(package_content.as_bytes()),
            manifest,
            modules,
        };
        package.validate_knowledge_pack_names()?;
        Ok(package)
    }

    fn resolve_package_modules(
        &self,
        package_path: &str,
        manifest: &ModulePackageManifest,
        modules: &mut BTreeMap<String, ResolvedModule>,
    ) -> Result<()> {
        let mut pending = manifest.modules.clone();
        let mut index_stack = BTreeSet::new();
        let mut expanded_modules = BTreeSet::new();

        while let Some(module) = pending.pop() {
            let module_files =
                self.expand_package_module(package_path, &module, &mut index_stack)?;
            for module_file in module_files {
                if !expanded_modules.insert(module_file.path.clone()) {
                    continue;
                }
                let imports: Vec<ModuleImport> = self.insert_resolved_module_file(
                    package_path,
                    &module_file,
                    manifest,
                    modules,
                )?;
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
        Ok(())
    }

    fn insert_resolved_module_file(
        &self,
        package_path: &str,
        module: &ExpandedPackageModule,
        package: &ModulePackageManifest,
        modules: &mut BTreeMap<String, ResolvedModule>,
    ) -> Result<Vec<ModuleImport>> {
        let resources: Vec<crate::ResourceManifest> =
            parse_module_file(&module.content, &module.source_path)?;
        let multiple_resources = resources.len() > 1;
        let mut all_imports = Vec::new();
        for resource_manifest in resources {
            let resource_manifest = complete_package_resource_manifest(resource_manifest, package)?;
            let kind = resource_manifest.kind()?;
            resource_manifest.name().with_context(|| {
                format!("failed to validate module manifest {}", module.source_path)
            })?;
            let resource_name = resource_manifest
                .name()
                .expect("resource name was just validated")
                .to_string();
            let imports = resource_manifest.imports();
            let files = self
                .resolved_module_files(package_path, module, kind, &resource_manifest)
                .with_context(|| {
                    format!("failed to resolve runtime files for {}", module.source_path)
                })?;
            all_imports.extend(imports.clone());
            let resolved = ResolvedModule {
                package_name: package.name.clone(),
                package_version: package.version.clone(),
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
                bail!(
                    "{} declares duplicate resolved module '{resource_key}'",
                    module.source_path
                );
            }
            if !multiple_resources && modules.insert(module.path.clone(), resolved).is_some() {
                bail!(
                    "{} declares duplicate resolved module '{}'",
                    module.source_path,
                    module.path
                );
            }
        }
        Ok(all_imports)
    }

    fn resolved_module_files(
        &self,
        package_path: &str,
        module: &ExpandedPackageModule,
        kind: PackageKind,
        resource_manifest: &crate::ResourceManifest,
    ) -> Result<Vec<ResolvedPackageFile>> {
        let mut files = BTreeMap::new();
        if kind == PackageKind::Command
            && let Some(content_path) = resource_manifest
                .manifest
                .get("content_path")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|path| !path.is_empty())
        {
            let (path, content) = self.read_command_content_file(
                package_path,
                &module.path,
                &module.source_path,
                content_path,
            )?;
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

    fn read_command_content_file(
        &self,
        package_path: &str,
        module_path: &str,
        module_source_path: &str,
        content_path: &str,
    ) -> Result<(String, String)> {
        let candidates = command_content_path_candidates(
            package_path,
            module_path,
            module_source_path,
            content_path,
        )?;
        let mut last_error = None;
        for candidate in candidates {
            match self.read_text(&candidate.read_path) {
                Ok(content) => return Ok((candidate.package_path, content)),
                Err(err) => last_error = Some(err),
            }
        }
        if let Some(err) = last_error {
            return Err(err.context(format!(
                "command content_path '{content_path}' referenced by {module_source_path} was not found"
            )));
        }
        bail!(
            "command content_path '{content_path}' referenced by {module_source_path} was not found"
        );
    }

    fn expand_package_module(
        &self,
        package_path: &str,
        module: &PackageModule,
        stack: &mut BTreeSet<String>,
    ) -> Result<Vec<ExpandedPackageModule>> {
        let mut expanded = Vec::new();
        let module_path = normalize_module_reference(&module.path)?;
        let (module_path, source_path) = if module_reference_is_directory(&module.path) {
            self.resolve_directory_index_path(package_path, &module_path)?
        } else {
            let source_path = package_module_source_path(package_path, &module_path)?;
            (module_path, source_path)
        };
        let module_content = self.read_text(&source_path)?;
        let schema = module_file_schema(&module_content, &source_path)?;
        if schema == "nenjo.module_index.v1" {
            self.expand_module_index(
                package_path,
                &module_path,
                &source_path,
                stack,
                &mut expanded,
            )?;
        } else {
            expanded.push(ExpandedPackageModule {
                path: module_path,
                source_path,
                content: module_content,
            });
        }
        Ok(expanded)
    }

    fn expand_module_index(
        &self,
        package_path: &str,
        index_module_path: &str,
        index_source_path: &str,
        stack: &mut BTreeSet<String>,
        expanded: &mut Vec<ExpandedPackageModule>,
    ) -> Result<()> {
        if !stack.insert(index_source_path.to_string()) {
            bail!("module index cycle includes {index_source_path}");
        }
        let index_content = self.read_text(index_source_path)?;
        let index: ModuleIndexManifest = parse_json_or_yaml_as(&index_content)
            .with_context(|| format!("failed to parse module index {index_source_path}"))?;
        index.validate(index_source_path)?;
        let index_dir = index_module_path.rsplit_once('/').map(|(dir, _)| dir);
        for module in &index.modules {
            let child_path = index_child_module_path(index_dir, &module.path)?;
            let child = PackageModule {
                path: child_path,
                metadata: module.metadata.clone(),
            };
            let child_modules = self
                .expand_package_module(package_path, &child, stack)
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

    fn resolve_directory_index_path(
        &self,
        package_path: &str,
        module_path: &str,
    ) -> Result<(String, String)> {
        let yml_module_path = format!("{module_path}/index.yml");
        let yml_source_path = package_module_source_path(package_path, &yml_module_path)?;
        if self.root.join(&yml_source_path).is_file() {
            return Ok((yml_module_path, yml_source_path));
        }
        let yaml_module_path = format!("{module_path}/index.yaml");
        let yaml_source_path = package_module_source_path(package_path, &yaml_module_path)?;
        if self.root.join(&yaml_source_path).is_file() {
            return Ok((yaml_module_path, yaml_source_path));
        }
        let skill_module_path = format!("{module_path}/SKILL.md");
        let skill_source_path = package_module_source_path(package_path, &skill_module_path)?;
        if self.root.join(&skill_source_path).is_file() {
            return Ok((module_path.to_string(), skill_source_path));
        }
        bail!("directory module '{module_path}/' requires index.yml, index.yaml, or SKILL.md");
    }
}

#[derive(Debug)]
struct ExpandedPackageModule {
    path: String,
    source_path: String,
    content: String,
}
