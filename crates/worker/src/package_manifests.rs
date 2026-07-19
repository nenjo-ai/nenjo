use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nenjo::Slug;
use nenjo::manifest::{KnowledgePackManifest, KnowledgePackSource, Manifest, ManifestLoader};
use nenjo_nenpm::{
    LockedModule, NenpmLock, PackageInstallIndex, PackageSource,
    package_install_path_in_packages_dir,
};
use nenjo_packages::{
    LocalPackageResolver, PackageKind, PackageResourceLogicalKey, ResolvedPackage, ResourceManifest,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::warn;

pub struct PackageManifestLoader {
    root: PathBuf,
    packages_dir: PathBuf,
}

impl PackageManifestLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let packages_dir = root.join(".nenjo").join("packages");
        Self { root, packages_dir }
    }

    pub fn with_packages_dir(root: impl Into<PathBuf>, packages_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            packages_dir: packages_dir.into(),
        }
    }
}

#[async_trait::async_trait]
impl ManifestLoader for PackageManifestLoader {
    async fn load(&self) -> Result<Manifest> {
        let lock_path = self.root.join("nenpm.lock.yml");
        if !lock_path.exists() {
            return Ok(Manifest::default());
        }
        let root = self.root.clone();
        let packages_dir = self.packages_dir.clone();
        tokio::task::spawn_blocking(move || load_package_manifest(&root, &packages_dir))
            .await
            .context("package manifest load task failed")?
    }
}

fn load_package_manifest(root: &Path, packages_dir: &Path) -> Result<Manifest> {
    let lock = NenpmLock::load_file(root.join("nenpm.lock.yml"))?;
    let index = load_package_install_index(packages_dir)?;
    let mut manifest = Manifest::default();

    for package in lock.packages {
        let installed_package = materialized_package(root, packages_dir, index.as_ref(), &package);
        if !installed_package.root.exists() {
            warn!(
                package = %package.name,
                version = %package.version,
                path = %installed_package.root.display(),
                "Skipping package without a materialized install directory"
            );
            continue;
        }
        let resolved_install = resolve_installed_package_manifest(&installed_package, &package)?;
        let resolved = resolved_install.package;
        let locked_source_paths = locked_module_source_paths(&package.modules);
        let locked_modules = locked_module_keys(&package.modules);
        let mut modules_by_key = BTreeMap::new();
        for (_, module) in resolved.modules {
            let key = (module.path.clone(), module.name().to_string());
            if locked_modules.contains(&key) {
                modules_by_key.entry(key).or_insert(module);
            }
        }
        let modules = modules_by_key.into_values().collect::<Vec<_>>();
        let assignment_index = build_assignment_index(
            &modules,
            &locked_source_paths,
            &package.name,
            package.source.as_ref(),
        );
        for module in modules {
            let mut resource = RuntimeResourceManifest::from_resource_manifest(&module.manifest)?;
            resource.apply_package_assignments(module.kind, &assignment_index)?;
            push_package_resource(
                &mut manifest,
                PackageResourceContext {
                    package_name: &package.name,
                    package_version: package.version.as_str(),
                    package_source: package.source.as_ref(),
                    package_root: &resolved_install.package_root,
                    module_path: module.path.as_str(),
                    source_path: module.source_path.as_str(),
                    kind: module.kind,
                },
                resource,
            )
            .with_context(|| {
                format!(
                    "failed to load package resource package={} version={} module={} source={}",
                    package.name, package.version, module.path, module.source_path
                )
            })?;
        }
    }

    Ok(manifest)
}

#[derive(Debug, Clone)]
struct PackageAssignmentTarget {
    kind: PackageKind,
    name: String,
}

fn locked_module_source_paths(modules: &[LockedModule]) -> BTreeMap<(String, String), Vec<String>> {
    let mut paths: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for module in modules {
        paths
            .entry((module.path.clone(), module.name.clone()))
            .or_default()
            .push(module.source_path.clone());
    }
    paths
}

fn locked_module_keys(modules: &[LockedModule]) -> BTreeSet<(String, String)> {
    modules
        .iter()
        .map(|module| (module.path.clone(), module.name.clone()))
        .collect()
}

fn build_assignment_index(
    modules: &[nenjo_packages::ResolvedModule],
    locked_source_paths: &BTreeMap<(String, String), Vec<String>>,
    package_name: &str,
    package_source: Option<&PackageSource>,
) -> BTreeMap<String, PackageAssignmentTarget> {
    let mut index = BTreeMap::new();
    for module in modules {
        let target = PackageAssignmentTarget {
            kind: module.kind,
            name: match module.kind {
                PackageKind::McpServer => {
                    package_runtime_slug(package_name, package_source, module.name())
                }
                _ => module.name().to_string(),
            },
        };
        insert_assignment_target(&mut index, &module.path, target.clone());
        insert_assignment_target(&mut index, &module.source_path, target.clone());
        insert_assignment_target(
            &mut index,
            &format!("{}#{}", module.path, module.name()),
            target.clone(),
        );
        insert_assignment_target(
            &mut index,
            &format!("{}#{}", module.source_path, module.name()),
            target.clone(),
        );
        if let Some(source_paths) =
            locked_source_paths.get(&(module.path.clone(), module.name().to_string()))
        {
            for source_path in source_paths {
                insert_assignment_target(&mut index, source_path, target.clone());
                insert_assignment_target(
                    &mut index,
                    &format!("{}#{}", source_path, module.name()),
                    target.clone(),
                );
            }
        }
    }
    index
}

fn insert_assignment_target(
    index: &mut BTreeMap<String, PackageAssignmentTarget>,
    path: &str,
    target: PackageAssignmentTarget,
) {
    let path = normalize_assignment_ref(path);
    if !path.is_empty() {
        index.insert(path, target);
    }
}

struct MaterializedPackage {
    root: PathBuf,
    manifest_path: String,
}

struct ResolvedInstalledPackage {
    package: ResolvedPackage,
    package_root: PathBuf,
}

fn resolve_installed_package_manifest(
    installed_package: &MaterializedPackage,
    package: &nenjo_nenpm::LockedPackage,
) -> Result<ResolvedInstalledPackage> {
    let candidates = installed_package_manifest_candidates(installed_package, package);
    let resolver = LocalPackageResolver::new(&installed_package.root);
    let mut attempted = Vec::new();

    for manifest_path in candidates {
        attempted.push(manifest_path.clone());
        if !installed_package.root.join(&manifest_path).is_file() {
            continue;
        }
        let resolved = resolver
            .resolve_package_manifest(&manifest_path)
            .with_context(|| {
                format!(
                    "failed to resolve installed package manifest package={} version={} path={manifest_path}",
                    package.name, package.version
                )
            })?;
        return Ok(ResolvedInstalledPackage {
            package: resolved,
            package_root: package_root_for_manifest(&installed_package.root, &manifest_path),
        });
    }

    bail!(
        "failed to resolve installed package manifest package={} version={} path={} tried={}",
        package.name,
        package.version,
        installed_package.manifest_path,
        attempted.join(", ")
    );
}

fn installed_package_manifest_candidates(
    installed_package: &MaterializedPackage,
    package: &nenjo_nenpm::LockedPackage,
) -> Vec<String> {
    let mut candidates = Vec::new();
    push_manifest_candidate(&mut candidates, installed_package.manifest_path.clone());
    push_manifest_candidate(
        &mut candidates,
        materialized_manifest_path(&package.manifest_path),
    );
    push_manifest_candidate(&mut candidates, package.manifest_path.clone());
    candidates
}

fn push_manifest_candidate(candidates: &mut Vec<String>, manifest_path: String) {
    if !candidates
        .iter()
        .any(|candidate| candidate == &manifest_path)
    {
        candidates.push(manifest_path);
    }
}

fn package_root_for_manifest(root: &Path, manifest_path: &str) -> PathBuf {
    Path::new(manifest_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| root.join(parent))
        .unwrap_or_else(|| root.to_path_buf())
}

fn materialized_package(
    root: &Path,
    packages_dir: &Path,
    index: Option<&PackageInstallIndex>,
    package: &nenjo_nenpm::LockedPackage,
) -> MaterializedPackage {
    if let Some(entry) = index.and_then(|index| index.get_package(&package.name, &package.version))
    {
        return MaterializedPackage {
            root: package_root_from_index(root, packages_dir, &entry.root),
            manifest_path: entry.manifest_path.clone(),
        };
    }

    MaterializedPackage {
        root: package_install_path_in_packages_dir(packages_dir, &package.name, &package.version),
        manifest_path: materialized_manifest_path(&package.manifest_path),
    }
}

fn materialized_manifest_path(manifest_path: &str) -> String {
    Path::new(manifest_path)
        .file_name()
        .map(|name| name.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| manifest_path.to_string())
}

fn load_package_install_index(packages_dir: &Path) -> Result<Option<PackageInstallIndex>> {
    let index_path = packages_dir.join(".nenpm-index.json");
    if index_path.exists() {
        return Ok(PackageInstallIndex::load_file(index_path).map(Some)?);
    }
    Ok(None)
}

fn package_root_from_index(root: &Path, packages_dir: &Path, indexed_root: &str) -> PathBuf {
    let indexed = Path::new(indexed_root);
    if indexed.is_absolute() {
        indexed.to_path_buf()
    } else if let Ok(relative_to_packages) = indexed.strip_prefix(".nenjo/packages") {
        packages_dir.join(relative_to_packages)
    } else {
        root.join(indexed)
    }
}

#[derive(Debug)]
struct RuntimeResourceManifest {
    name: String,
    selector: Option<String>,
    root_uri: Option<String>,
    fields: serde_json::Map<String, Value>,
}

impl RuntimeResourceManifest {
    fn from_resource_manifest(resource: &ResourceManifest) -> Result<Self> {
        let name = resource.name()?.to_string();
        let mut fields = resource.manifest_object()?.clone();
        fields.remove("name");
        Ok(Self {
            name,
            selector: resource.selector().map(str::to_string),
            root_uri: resource.root_uri().map(str::to_string),
            fields,
        })
    }

    fn into_value(self) -> Value {
        let mut fields = self.fields;
        fields.insert("name".to_string(), Value::String(self.name));
        Value::Object(fields)
    }

    fn apply_package_assignments(
        &mut self,
        kind: PackageKind,
        index: &BTreeMap<String, PackageAssignmentTarget>,
    ) -> Result<()> {
        let Some(assignments) = self.fields.remove("assignments") else {
            return Ok(());
        };
        match kind {
            PackageKind::Agent => {
                self.apply_name_assignments(
                    &assignments,
                    index,
                    "abilities",
                    PackageKind::Ability,
                    "abilities",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "domains",
                    PackageKind::Domain,
                    "domains",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "mcp_servers",
                    PackageKind::McpServer,
                    "mcp_servers",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "script_tools",
                    PackageKind::ScriptTool,
                    "script_tools",
                )?;
            }
            PackageKind::Domain => {
                self.apply_name_assignments(
                    &assignments,
                    index,
                    "abilities",
                    PackageKind::Ability,
                    "abilities",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "mcp_servers",
                    PackageKind::McpServer,
                    "mcp_servers",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "script_tools",
                    PackageKind::ScriptTool,
                    "script_tools",
                )?;
            }
            PackageKind::Ability => {
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "mcp_servers",
                    PackageKind::McpServer,
                    "mcp_servers",
                )?;
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "script_tools",
                    PackageKind::ScriptTool,
                    "script_tools",
                )?;
            }
            PackageKind::Model
            | PackageKind::Routine
            | PackageKind::Knowledge
            | PackageKind::Skill
            | PackageKind::Plugin
            | PackageKind::ContextBlock
            | PackageKind::McpServer
            | PackageKind::Command
            | PackageKind::Hook
            | PackageKind::ScriptTool => {}
        }
        Ok(())
    }

    fn apply_name_assignments(
        &mut self,
        assignments: &Value,
        index: &BTreeMap<String, PackageAssignmentTarget>,
        assignment_key: &str,
        expected_kind: PackageKind,
        output_key: &str,
    ) -> Result<()> {
        let names = resolve_assignment_names(assignments, index, assignment_key, expected_kind)?;
        if !names.is_empty() {
            self.fields
                .insert(output_key.to_string(), serde_json::json!(names));
        }
        Ok(())
    }

    fn apply_slug_assignments(
        &mut self,
        assignments: &Value,
        index: &BTreeMap<String, PackageAssignmentTarget>,
        assignment_key: &str,
        expected_kind: PackageKind,
        output_key: &str,
    ) -> Result<()> {
        let names = resolve_assignment_names(assignments, index, assignment_key, expected_kind)?;
        if !names.is_empty() {
            let slugs = names
                .into_iter()
                .map(|name| nenjo::Slug::derive(name).to_string())
                .collect::<Vec<_>>();
            self.fields
                .insert(output_key.to_string(), serde_json::json!(slugs));
        }
        Ok(())
    }
}

fn resolve_assignment_names(
    assignments: &Value,
    index: &BTreeMap<String, PackageAssignmentTarget>,
    assignment_key: &str,
    expected_kind: PackageKind,
) -> Result<Vec<String>> {
    assignment_refs(assignments, assignment_key)?
        .into_iter()
        .map(|reference| {
            let target = resolve_assignment_target(index, &reference, assignment_key)?;
            if target.kind != expected_kind {
                bail!(
                    "Assignment '{}' references package path '{}' with kind {}, expected {}",
                    assignment_key,
                    reference,
                    target.kind.as_str(),
                    expected_kind.as_str()
                );
            }
            Ok(target.name.clone())
        })
        .collect()
}

fn assignment_refs(assignments: &Value, assignment_key: &str) -> Result<Vec<String>> {
    let Some(value) = assignments.get(assignment_key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        bail!("Assignment '{assignment_key}' must be an array of package paths");
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(normalize_assignment_ref)
                .filter(|reference| !reference.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Assignment '{assignment_key}' must contain only non-empty package paths"
                    )
                })
        })
        .collect()
}

fn resolve_assignment_target<'a>(
    index: &'a BTreeMap<String, PackageAssignmentTarget>,
    reference: &str,
    assignment_key: &str,
) -> Result<&'a PackageAssignmentTarget> {
    if let Some(target) = index.get(reference) {
        return Ok(target);
    }

    let matches = index
        .iter()
        .filter(|(path, _)| {
            reference.ends_with(&format!("/{path}")) || path.ends_with(&format!("/{reference}"))
        })
        .map(|(_, target)| target)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [target] => Ok(target),
        [] => bail!(
            "Assignment '{}' references package path '{}' that was not installed. Add it to package modules or imports.",
            assignment_key,
            reference
        ),
        _ => bail!(
            "Assignment '{}' references ambiguous package path '{}'",
            assignment_key,
            reference
        ),
    }
}

fn normalize_assignment_ref(reference: impl AsRef<str>) -> String {
    reference
        .as_ref()
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

fn package_knowledge_selector_name(
    package_name: &str,
    source: Option<&PackageSource>,
    pack_id: &str,
) -> String {
    let mut segments = package_selector_segments(package_name, source);
    segments.push(knowledge_pack_leaf_segment(pack_id));
    segments
        .into_iter()
        .filter(|segment| !segment.trim().is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

fn knowledge_pack_leaf_segment(pack_id: &str) -> String {
    let leaf = pack_id
        .trim()
        .rsplit(['.', '/'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("knowledge");
    selector_segment(leaf)
}

fn push_package_resource(
    manifest: &mut Manifest,
    context: PackageResourceContext<'_>,
    resource_manifest: RuntimeResourceManifest,
) -> Result<()> {
    match context.kind {
        PackageKind::Knowledge => {
            push_package_knowledge_resource(manifest, context, resource_manifest);
            return Ok(());
        }
        PackageKind::Routine | PackageKind::Plugin => return Ok(()),
        PackageKind::Model
        | PackageKind::Agent
        | PackageKind::Ability
        | PackageKind::Domain
        | PackageKind::ContextBlock
        | PackageKind::McpServer
        | PackageKind::Skill
        | PackageKind::Command
        | PackageKind::Hook
        | PackageKind::ScriptTool => {}
    }

    let id = PackageResourceLogicalKey::new(
        context.package_name,
        context.kind,
        context.module_path,
        &resource_manifest.name,
    )?
    .resource_id();
    let value = with_package_defaults(
        resource_manifest.into_value(),
        PackageDefaults {
            id,
            package_name: context.package_name,
            package_version: context.package_version,
            package_source: context.package_source,
            module_path: context.module_path,
            source_path: context.source_path,
            kind: context.kind,
            package_root: context.package_root,
        },
    );
    match context.kind {
        PackageKind::Model => manifest.models.push(deserialize_manifest(value)?),
        PackageKind::Agent => manifest.agents.push(deserialize_manifest(value)?),
        PackageKind::Ability => manifest.abilities.push(deserialize_manifest(value)?),
        PackageKind::Domain => manifest.domains.push(deserialize_manifest(value)?),
        PackageKind::ContextBlock => manifest.context_blocks.push(deserialize_manifest(value)?),
        PackageKind::McpServer => manifest.mcp_servers.push(deserialize_manifest(value)?),
        PackageKind::Skill => manifest.skills.push(deserialize_manifest(value)?),
        PackageKind::Command => manifest.commands.push(deserialize_manifest(value)?),
        PackageKind::Hook => manifest.hooks.push(deserialize_manifest(value)?),
        PackageKind::ScriptTool => manifest.script_tools.push(deserialize_manifest(value)?),
        PackageKind::Routine | PackageKind::Knowledge | PackageKind::Plugin => {
            unreachable!("non-runtime package kinds returned before id derivation")
        }
    }
    Ok(())
}

fn push_package_knowledge_resource(
    manifest: &mut Manifest,
    context: PackageResourceContext<'_>,
    resource_manifest: RuntimeResourceManifest,
) {
    let pack_id = resource_manifest
        .fields
        .get("pack_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(resource_manifest.name.as_str());
    let selector_name =
        package_knowledge_selector_name(context.package_name, context.package_source, pack_id);
    let selector = resource_manifest
        .selector
        .filter(|value| value.starts_with("pkg:") && !value.starts_with("pkg://"))
        .unwrap_or_else(|| format!("pkg:{selector_name}"));
    let root_uri = resource_manifest
        .root_uri
        .or_else(|| {
            resource_manifest
                .fields
                .get("root_uri")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("pkg://{}/", pack_id.trim().trim_matches('/')));

    let package_version = resource_manifest
        .fields
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| context.package_version.to_string());
    // Versioned slug so multi-version packs coexist under the same logical selector.
    let version_label = format!(
        "v{}",
        package_version
            .trim()
            .trim_start_matches(['v', 'V'])
            .replace('.', "_")
    );
    let slug = Slug::derive(format!(
        "{}-{}",
        selector.trim_start_matches("pkg:"),
        version_label
    ));

    manifest.knowledge_packs.push(KnowledgePackManifest {
        slug,
        name: resource_manifest.name,
        description: resource_manifest
            .fields
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_type: KnowledgePackSource::Package,
        selector,
        version: Some(package_version),
        root_uri,
        root_path: Some(context.package_root.join(context.module_path)),
        read_only: true,
        metadata: serde_json::json!({
            "package": {
                "name": context.package_name,
                "version": context.package_version,
                "module_path": context.module_path,
                "source_path": context.source_path,
                "kind": context.kind,
            }
        }),
    });
}

#[derive(Clone, Copy)]
struct PackageResourceContext<'a> {
    package_name: &'a str,
    package_version: &'a str,
    package_source: Option<&'a PackageSource>,
    package_root: &'a Path,
    module_path: &'a str,
    source_path: &'a str,
    kind: PackageKind,
}

fn deserialize_manifest<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("failed to deserialize package runtime manifest")
}

struct PackageDefaults<'a> {
    id: uuid::Uuid,
    package_name: &'a str,
    package_version: &'a str,
    package_source: Option<&'a PackageSource>,
    module_path: &'a str,
    source_path: &'a str,
    kind: PackageKind,
    package_root: &'a Path,
}

fn with_package_defaults(mut value: Value, defaults: PackageDefaults<'_>) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object
        .entry("id")
        .or_insert_with(|| Value::String(defaults.id.to_string()));
    ensure_slug(object, &defaults);
    object
        .entry("source_type")
        .or_insert_with(|| Value::String("package".to_string()));
    object
        .entry("read_only")
        .or_insert_with(|| Value::Bool(true));
    object.entry("metadata").or_insert_with(|| {
        serde_json::json!({
            "package": {
                "name": defaults.package_name,
                "version": defaults.package_version,
                "module_path": defaults.module_path,
                "source_path": defaults.source_path,
                "kind": defaults.kind,
            }
        })
    });

    match defaults.kind {
        PackageKind::Model => {
            let model_provider = object
                .get("model_provider")
                .or_else(|| object.get("provider"))
                .and_then(Value::as_str)
                .unwrap_or("openai")
                .to_string();
            object
                .entry("model_provider")
                .or_insert_with(|| Value::String(model_provider.clone()));
            object
                .entry("temperature")
                .or_insert_with(|| serde_json::json!(0.7));
            object
                .entry("native_tools")
                .or_insert_with(|| serde_json::json!([]));
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("package_model");
            let slug = package_runtime_slug(defaults.package_name, defaults.package_source, name);
            object.insert("name".to_string(), Value::String(slug.clone()));
            object.insert("slug".to_string(), Value::String(slug));
        }
        PackageKind::Agent => {
            ensure_agent_prompt_config(object);
            object.entry("color").or_insert(Value::Null);
            object.entry("model_id").or_insert(Value::Null);
            object
                .entry("domains")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("platform_scopes")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_servers")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("abilities")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("prompt_locked")
                .or_insert_with(|| Value::Bool(true));
        }
        PackageKind::Ability => {
            // Same pkg/<scope>/<version>/<package>/... convention as context blocks.
            object.insert(
                "path".to_string(),
                Value::String(derived_package_content_path(
                    defaults.package_name,
                    defaults.package_version,
                    defaults.package_source,
                    defaults.module_path,
                )),
            );
            object
                .entry("activation_condition")
                .or_insert_with(|| Value::String(String::new()));
            ensure_ability_prompt_config(object);
            object
                .entry("platform_scopes")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_servers")
                .or_insert_with(|| serde_json::json!([]));
        }
        PackageKind::Domain => {
            object.insert(
                "path".to_string(),
                Value::String(derived_package_content_path(
                    defaults.package_name,
                    defaults.package_version,
                    defaults.package_source,
                    defaults.module_path,
                )),
            );
            object
                .entry("command")
                .or_insert_with(|| Value::String(String::new()));
            object
                .entry("platform_scopes")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("abilities")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_servers")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("prompt_config")
                .or_insert_with(|| serde_json::json!({}));
        }
        PackageKind::ContextBlock => {
            object.insert(
                "path".to_string(),
                Value::String(derived_package_content_path(
                    defaults.package_name,
                    defaults.package_version,
                    defaults.package_source,
                    defaults.module_path,
                )),
            );
            object
                .entry("template")
                .or_insert_with(|| Value::String(String::new()));
        }
        PackageKind::McpServer => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("package_mcp_server")
                .to_string();
            object
                .entry("display_name")
                .or_insert_with(|| Value::String(name.clone()));
            object.insert(
                "name".to_string(),
                Value::String(package_runtime_slug(
                    defaults.package_name,
                    defaults.package_source,
                    &name,
                )),
            );
            object
                .entry("transport")
                .or_insert_with(|| Value::String("stdio".to_string()));
            object
                .entry("env_schema")
                .or_insert_with(|| serde_json::json!({}));
            ensure_mcp_runtime_metadata(object, defaults.package_root);
        }
        PackageKind::Skill => {
            scope_hook_references(object, &defaults);
            let root_path = skill_root_path(object, defaults.source_path);
            object
                .entry("root_path")
                .or_insert_with(|| Value::String(root_path.clone()));
            object
                .entry("entry_path")
                .or_insert_with(|| Value::String("SKILL.md".to_string()));
            object
                .entry("aliases")
                .or_insert_with(|| serde_json::json!([]));
            let root_dir = skill_root_dir(object, defaults.package_root, &root_path);
            object.insert(
                "root_dir".to_string(),
                Value::String(root_dir.to_string_lossy().into_owned()),
            );
            if let Some(plugin_root_path) = skill_plugin_root_path(object) {
                object
                    .entry("plugin_root_path")
                    .or_insert_with(|| Value::String(plugin_root_path.clone()));
                let plugin_root_dir =
                    skill_plugin_root_dir(object, defaults.package_root, &plugin_root_path);
                object.insert(
                    "plugin_root_dir".to_string(),
                    Value::String(plugin_root_dir.to_string_lossy().into_owned()),
                );
            }
            object
                .entry("scripts")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("references")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("assets")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_servers")
                .or_insert_with(|| serde_json::json!([]));
        }
        PackageKind::Command => {
            scope_hook_references(object, &defaults);
            let (root_path, entry_path) =
                command_content_paths(object, defaults.module_path, defaults.source_path);
            object.insert("root_path".to_string(), Value::String(root_path.clone()));
            object.insert("entry_path".to_string(), Value::String(entry_path));
            let root_dir = skill_root_dir(object, defaults.package_root, &root_path);
            object.insert(
                "root_dir".to_string(),
                Value::String(root_dir.to_string_lossy().into_owned()),
            );
            if let Some(plugin_root_path) = skill_plugin_root_path(object) {
                object
                    .entry("plugin_root_path")
                    .or_insert_with(|| Value::String(plugin_root_path.clone()));
                let plugin_root_dir =
                    skill_plugin_root_dir(object, defaults.package_root, &plugin_root_path);
                object.insert(
                    "plugin_root_dir".to_string(),
                    Value::String(plugin_root_dir.to_string_lossy().into_owned()),
                );
            }
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("command")
                .to_string();
            object
                .entry("command")
                .or_insert_with(|| Value::String(format!("/{name}")));
            object
                .entry("hooks")
                .or_insert_with(|| serde_json::json!([]));
        }
        PackageKind::Hook => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("package_hook")
                .to_string();
            object
                .entry("display_name")
                .or_insert_with(|| Value::String(name.clone()));
            object.insert(
                "name".to_string(),
                Value::String(package_runtime_slug(
                    defaults.package_name,
                    defaults.package_source,
                    &name,
                )),
            );
            if let Some(plugin_root_path) = skill_plugin_root_path(object) {
                object
                    .entry("plugin_root_path")
                    .or_insert_with(|| Value::String(plugin_root_path.clone()));
                let plugin_root_dir =
                    skill_plugin_root_dir(object, defaults.package_root, &plugin_root_path);
                object.insert(
                    "plugin_root_dir".to_string(),
                    Value::String(plugin_root_dir.to_string_lossy().into_owned()),
                );
            }
            object
                .entry("matcher")
                .or_insert_with(|| Value::String("*".to_string()));
        }
        PackageKind::ScriptTool => {
            let root_path = command_root_path(object, defaults.source_path);
            object
                .entry("root_path")
                .or_insert_with(|| Value::String(root_path.clone()));
            let root_dir = skill_root_dir(object, defaults.package_root, &root_path);
            object.insert(
                "root_dir".to_string(),
                Value::String(root_dir.to_string_lossy().into_owned()),
            );
            object
                .entry("category")
                .or_insert_with(|| Value::String("read_write".to_string()));
            object
                .entry("parameters")
                .or_insert_with(|| serde_json::json!({ "type": "object", "properties": {} }));
        }
        PackageKind::Routine | PackageKind::Knowledge | PackageKind::Plugin => {}
    }
    value
}

fn ensure_slug(object: &mut serde_json::Map<String, Value>, defaults: &PackageDefaults<'_>) {
    if !matches!(defaults.kind, PackageKind::Agent | PackageKind::Routine) {
        return;
    }
    let local = object
        .get("slug")
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("resource");
    object.insert(
        "slug".to_string(),
        Value::String(package_runtime_slug(
            defaults.package_name,
            defaults.package_source,
            local,
        )),
    );
}

fn scope_hook_references(
    object: &mut serde_json::Map<String, Value>,
    defaults: &PackageDefaults<'_>,
) {
    let Some(hooks) = object.get_mut("hooks").and_then(Value::as_array_mut) else {
        return;
    };
    for hook in hooks {
        let Some(local_name) = hook.as_str().map(str::to_string) else {
            continue;
        };
        *hook = Value::String(package_runtime_slug(
            defaults.package_name,
            defaults.package_source,
            &local_name,
        ));
    }
}

fn package_runtime_slug(
    package_name: &str,
    package_source: Option<&PackageSource>,
    local_name: &str,
) -> String {
    let scope = package_runtime_scope(package_name, package_source);
    let local = Slug::derive(local_name).into_string();
    if local == scope || local.starts_with(&format!("{scope}-")) {
        local
    } else {
        format!("{scope}-{local}")
    }
}

fn package_runtime_scope(package_name: &str, package_source: Option<&PackageSource>) -> String {
    let source_scope = package_source.and_then(|source| match source {
        PackageSource::Git { url, .. } => github_owner_repo_from_url(url).map(|(owner, _)| owner),
        PackageSource::Local {
            scope: Some(scope), ..
        } => scope.split('.').next().map(str::to_string),
        PackageSource::Local { scope: None, .. }
        | PackageSource::Artifact { .. }
        | PackageSource::Remote { .. } => None,
    });
    let package_scope = package_name
        .trim_start_matches('@')
        .split_once('/')
        .map(|(scope, _)| scope.to_string());
    let fallback = package_name
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .unwrap_or("package");
    Slug::derive(
        source_scope
            .or(package_scope)
            .unwrap_or_else(|| fallback.to_string()),
    )
    .into_string()
}

fn skill_root_path(object: &serde_json::Map<String, Value>, source_path: &str) -> String {
    object
        .get("root_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .unwrap_or_else(|| {
            source_path
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_else(|| ".".to_string())
        })
}

fn command_root_path(object: &serde_json::Map<String, Value>, source_path: &str) -> String {
    object
        .get("root_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .unwrap_or_else(|| {
            source_path
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_else(|| ".".to_string())
        })
}

fn command_content_paths(
    object: &serde_json::Map<String, Value>,
    module_path: &str,
    source_path: &str,
) -> (String, String) {
    if let Some(content_path) = object
        .get("content_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let content_path = content_path
            .trim()
            .trim_start_matches("./")
            .trim_end_matches('/');
        let content_path = package_relative_content_path(content_path, module_path, source_path);
        if let Some((root_path, entry_path)) = content_path.rsplit_once('/') {
            return (root_path.to_string(), entry_path.to_string());
        }
        return (String::new(), content_path.to_string());
    }

    let root_path = command_root_path(object, source_path);
    let entry_path = object
        .get("entry_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("command.md")
        .to_string();
    (root_path, entry_path)
}

fn package_relative_content_path(
    content_path: &str,
    module_path: &str,
    source_path: &str,
) -> String {
    let source_dir = source_path.rsplit_once('/').map(|(dir, _)| dir);
    let module_dir = module_path.rsplit_once('/').map(|(dir, _)| dir);
    match (source_dir, module_dir) {
        (Some(source_dir), Some(module_dir)) => content_path
            .strip_prefix(&format!("{source_dir}/"))
            .map(|suffix| format!("{module_dir}/{suffix}"))
            .unwrap_or_else(|| module_stem_content_path(module_path, content_path)),
        _ => content_path.to_string(),
    }
}

fn module_stem_content_path(module_path: &str, content_path: &str) -> String {
    let Some((_, filename)) = content_path.rsplit_once('/') else {
        return content_path.to_string();
    };
    let module_stem = module_path
        .strip_suffix(".yaml")
        .or_else(|| module_path.strip_suffix(".yml"))
        .unwrap_or(module_path);
    format!("{module_stem}/{filename}")
}

fn skill_root_dir(
    object: &serde_json::Map<String, Value>,
    package_root: &Path,
    root_path: &str,
) -> PathBuf {
    if let Some(root_dir) = object
        .get("root_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let path = PathBuf::from(root_dir);
        if path.is_absolute() {
            return path;
        }
        return package_root.join(path);
    }
    if root_path == "." {
        package_root.to_path_buf()
    } else {
        package_root.join(root_path)
    }
}

fn skill_plugin_root_path(object: &serde_json::Map<String, Value>) -> Option<String> {
    object
        .get("plugin_root_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
}

fn skill_plugin_root_dir(
    object: &serde_json::Map<String, Value>,
    package_root: &Path,
    plugin_root_path: &str,
) -> PathBuf {
    if let Some(plugin_root_dir) = object
        .get("plugin_root_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let path = PathBuf::from(plugin_root_dir);
        if path.is_absolute() {
            return path;
        }
        return package_root.join(path);
    }
    if plugin_root_path == "." {
        package_root.to_path_buf()
    } else {
        package_root.join(plugin_root_path)
    }
}

fn ensure_mcp_runtime_metadata(object: &mut serde_json::Map<String, Value>, package_root: &Path) {
    let cwd_path = object
        .get("metadata")
        .and_then(mcp_runtime_cwd_path)
        .map(str::to_string);
    let Some(cwd_path) = cwd_path else {
        return;
    };
    let Some(cwd) = package_runtime_path(package_root, &cwd_path) else {
        return;
    };
    let metadata = object
        .entry("metadata")
        .or_insert_with(|| serde_json::json!({}));
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    let runtime = metadata
        .entry("runtime")
        .or_insert_with(|| serde_json::json!({}));
    let Some(runtime) = runtime.as_object_mut() else {
        return;
    };
    runtime
        .entry("cwd")
        .or_insert_with(|| Value::String(cwd.to_string_lossy().into_owned()));
}

fn mcp_runtime_cwd_path(metadata: &Value) -> Option<&str> {
    metadata
        .pointer("/runtime/cwd_path")
        .or_else(|| metadata.pointer("/claude/mcp/cwd_path"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn package_runtime_path(package_root: &Path, path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path == "." {
        return Some(package_root.to_path_buf());
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(package_root.join(path))
}

fn default_agent_prompt_config() -> Value {
    serde_json::json!({
        "system_prompt": "",
        "developer_prompt": "",
        "templates": {
            "task": "",
            "chat": "",
            "gate": ""
        },
        "memory_profile": {
            "core_focus": [],
            "project_focus": [],
            "shared_focus": []
        }
    })
}

fn ensure_agent_prompt_config(object: &mut serde_json::Map<String, Value>) {
    let prompt_config = object
        .entry("prompt_config")
        .or_insert_with(default_agent_prompt_config);
    let Some(prompt_object) = prompt_config.as_object_mut() else {
        *prompt_config = default_agent_prompt_config();
        return;
    };
    prompt_object
        .entry("system_prompt")
        .or_insert_with(|| Value::String(String::new()));
    prompt_object
        .entry("developer_prompt")
        .or_insert_with(|| Value::String(String::new()));
    prompt_object
        .entry("templates")
        .or_insert_with(|| serde_json::json!({ "task": "", "chat": "", "gate": "" }));
    let memory_profile = prompt_object.entry("memory_profile").or_insert_with(
        || serde_json::json!({ "core_focus": [], "project_focus": [], "shared_focus": [] }),
    );
    let Some(memory_object) = memory_profile.as_object_mut() else {
        *memory_profile =
            serde_json::json!({ "core_focus": [], "project_focus": [], "shared_focus": [] });
        return;
    };
    memory_object
        .entry("core_focus")
        .or_insert_with(|| serde_json::json!([]));
    memory_object
        .entry("project_focus")
        .or_insert_with(|| serde_json::json!([]));
    memory_object
        .entry("shared_focus")
        .or_insert_with(|| serde_json::json!([]));
}

fn ensure_ability_prompt_config(object: &mut serde_json::Map<String, Value>) {
    let prompt_config = object
        .entry("prompt_config")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(prompt_object) = prompt_config.as_object_mut() {
        prompt_object
            .entry("developer_prompt")
            .or_insert_with(|| Value::String(String::new()));
    }
}

fn package_selector_segments(package_name: &str, source: Option<&PackageSource>) -> Vec<String> {
    let mut segments = package_source_path_segments(package_name, source);
    let leaf = package_leaf_segment(package_name);
    if segments.last().is_none_or(|segment| segment != &leaf) {
        segments.push(leaf);
    }
    segments
}

/// Registry/source path segments without the package leaf (mirrors platform).
fn package_source_path_segments(package_name: &str, source: Option<&PackageSource>) -> Vec<String> {
    if let Some(segments) = source.and_then(package_source_selector_segments) {
        return segments;
    }
    let mut segments = package_name
        .trim_start_matches('@')
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(selector_segment)
        .collect::<Vec<_>>();
    // Drop package leaf so version is inserted before it.
    if segments.len() > 1 {
        segments.pop();
    }
    segments
}

/// Normalized version segment (`1.0.4` → `v1_0_4`).
fn package_version_label(version: &str) -> Option<String> {
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    Some(format!("v{}", selector_segment(version)))
}

fn package_source_selector_segments(source: &PackageSource) -> Option<Vec<String>> {
    match source {
        PackageSource::Git { url, .. } => github_owner_repo_from_url(url)
            .map(|(owner, repo)| vec![selector_segment(&owner), selector_segment(&repo)]),
        PackageSource::Local { scope, .. } => scope
            .as_deref()
            .map(|scope| scope.split('.').map(selector_segment).collect()),
        PackageSource::Artifact { .. } | PackageSource::Remote { .. } => None,
    }
}

fn github_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let normalized = url.trim_end_matches(".git").trim_end_matches('/');
    if let Some(ssh) = normalized.strip_prefix("git@github.com:") {
        let (owner, repo) = ssh.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }
    let parts = normalized.rsplit('/').take(2).collect::<Vec<_>>();
    if parts.len() == 2 {
        Some((parts[1].to_string(), parts[0].to_string()))
    } else {
        None
    }
}

fn selector_segment(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn package_leaf_segment(package_name: &str) -> String {
    package_name
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .map(selector_segment)
        .filter(|segment| !segment.is_empty())
        .unwrap_or_else(|| "package".to_string())
}

/// Content path for package abilities/domains/context blocks.
///
/// Shape: `pkg/<source>/<version>/<package>/<module-dir...>`
/// e.g. `pkg/nenjo_ai/packages/v1_0_4/nenji/capabilities/build`
fn derived_package_content_path(
    package_name: &str,
    package_version: &str,
    package_source: Option<&PackageSource>,
    module_path: &str,
) -> String {
    let mut segments = vec!["pkg".to_string()];
    segments.extend(package_source_path_segments(package_name, package_source));
    if let Some(version) = package_version_label(package_version) {
        segments.push(version);
    }
    let leaf = package_leaf_segment(package_name);
    if segments.last().is_none_or(|segment| segment != &leaf) {
        segments.push(leaf.clone());
    }
    if let Some((dir, _)) = module_path.rsplit_once('/') {
        let mut module_segments = dir
            .split('/')
            .filter(|segment| !segment.trim().is_empty())
            .map(selector_segment)
            .collect::<Vec<_>>();
        // Some lockfiles carry paths relative to the registry root rather than
        // the package root (for example `nenji/capabilities/build/...`). The
        // package leaf is already part of the canonical pkg path above.
        if module_segments
            .first()
            .is_some_and(|segment| segment == &leaf)
        {
            module_segments.remove(0);
        }
        segments.extend(module_segments);
    }
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::skills::{LocalSkillProvider, SkillRegistry};
    use crate::tools::SecurityPolicy;
    use nenjo::hooks::{HookEvent, HookRuntime, HookRuntimeEvent};
    use nenjo::manifest::{
        AbilityManifest, AgentManifest, CommandManifest, ContextBlockManifest, DomainManifest,
        McpServerManifest, SkillManifest,
    };
    use nenjo::skills::SkillProvider;
    use nenjo_models::ChatMessage;
    use nenjo_nenpm::LockedPackage;
    use nenjo_packages::{
        ClaudePluginResource, claude_plugin_resources, parse_claude_plugin_command,
        parse_claude_plugin_hooks, parse_claude_plugin_manifest, parse_claude_plugin_skill,
    };

    #[test]
    fn package_defaults_create_read_only_context_block() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "guide",
                "template": "Use the guide."
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/core",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "context/guide.yaml",
                source_path: "packages/core/context/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.name, "guide");
        assert_eq!(block.path, "pkg/nenjo/v0_1_0/core/context");
        assert_eq!(block.template, "Use the guide.");
    }

    #[test]
    fn package_defaults_override_authored_resource_path() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "guide",
                "path": "authored/path",
                "template": "Use the guide."
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/core",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "context/shared/guide.yaml",
                source_path: "packages/core/context/shared/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo/v0_1_0/core/context/shared");
    }

    #[test]
    fn package_defaults_derive_context_path_from_source_package_and_module() {
        let source = PackageSource::Git {
            url: "https://github.com/nenjo-ai/packages.git".into(),
            reference: "feat/v2".into(),
            manifest_path: "nenjo/context/package.yaml".into(),
        };
        let value = with_package_defaults(
            serde_json::json!({
                "name": "remembrance",
                "template": "Remember."
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo-ai/context",
                package_version: "0.1.0",
                package_source: Some(&source),
                package_root: Path::new("/package-root"),
                module_path: "memory/remembrance.yml",
                source_path: "nenjo/context/memory/remembrance.yml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo_ai/packages/v0_1_0/context/memory");
    }

    #[test]
    fn package_defaults_derive_ability_path_from_module_directory() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "design_agent",
                "path": "authored/path"
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/nenji",
                package_version: "0.2.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "nenji/abilities/design/agent.yml",
                source_path: "nenji/abilities/design/agent.yml",
                kind: PackageKind::Ability,
            },
        );
        let ability: AbilityManifest = serde_json::from_value(value).unwrap();
        assert_eq!(
            ability.path.as_deref(),
            Some("pkg/nenjo/v0_2_0/nenji/abilities/design")
        );
        assert!(ability.read_only);
        assert_eq!(ability.source_type, "package");
        assert_eq!(ability.metadata["package"]["version"], "0.2.0");
    }

    #[test]
    fn package_defaults_derive_domain_path_from_module_directory() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "creator",
                "path": "authored/path"
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/nenji",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "nenji/domains/creator.yml",
                source_path: "nenji/domains/creator.yml",
                kind: PackageKind::Domain,
            },
        );
        let domain: DomainManifest = serde_json::from_value(value).unwrap();
        assert_eq!(domain.path, "pkg/nenjo/v0_1_0/nenji/domains");
    }

    #[test]
    fn package_content_path_inserts_version_after_scope() {
        assert_eq!(
            derived_package_content_path(
                "@nenjo-ai/nenji",
                "1.0.4",
                Some(&PackageSource::Git {
                    url: "https://github.com/nenjo-ai/packages.git".to_string(),
                    reference: "main".to_string(),
                    manifest_path: "packages.yaml".to_string(),
                }),
                "nenji/capabilities/build/build_ability.yaml",
            ),
            "pkg/nenjo_ai/packages/v1_0_4/nenji/capabilities/build"
        );
    }

    #[test]
    fn package_defaults_use_package_path_for_root_level_resources() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "guide",
                "template": "Root module"
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/core-knowledge",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "guide.yml",
                source_path: "guide.yml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo/v0_1_0/core_knowledge");
    }

    #[test]
    fn package_defaults_create_skill_runtime_paths() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "review",
                "description": "Review code."
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/skills",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "skills/review",
                source_path: "skills/review/SKILL.md",
                kind: PackageKind::Skill,
            },
        );
        let skill: SkillManifest = serde_json::from_value(value).unwrap();
        assert_eq!(skill.entry_path, "SKILL.md");
        assert_eq!(skill.root_path, "skills/review");
        assert_eq!(
            skill.root_dir,
            Path::new("/package-root").join("skills/review")
        );
        assert!(skill.read_only);
        assert_eq!(skill.source_type, "package");
    }

    #[test]
    fn package_defaults_create_command_runtime_paths_from_content_path() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "design",
                "command": "/design",
                "content_path": "nenjo/nenji/commands/design/command.md"
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/nenji",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "commands/design.yaml",
                source_path: "nenjo/nenji/commands/design.yaml",
                kind: PackageKind::Command,
            },
        );
        let command: CommandManifest = serde_json::from_value(value).unwrap();
        assert_eq!(command.entry_path, "command.md");
        assert_eq!(command.root_path, "commands/design");
        assert_eq!(
            command.root_dir,
            Path::new("/package-root").join("commands/design")
        );
        assert!(command.read_only);
        assert_eq!(command.source_type, "package");
    }

    #[test]
    fn package_defaults_create_plugin_skill_runtime_paths() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "acme__review",
                "display_name": "acme:review",
                "aliases": ["review"],
                "description": "Review code.",
                "plugin_root_path": "."
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@claude-plugin/acme",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "skills/review",
                source_path: "skills/review/SKILL.md",
                kind: PackageKind::Skill,
            },
        );
        let skill: SkillManifest = serde_json::from_value(value).unwrap();
        assert_eq!(skill.name, "acme__review");
        assert_eq!(skill.display_name.as_deref(), Some("acme:review"));
        assert_eq!(skill.aliases, vec!["review"]);
        assert_eq!(
            skill.root_dir,
            Path::new("/package-root").join("skills/review")
        );
        assert_eq!(skill.plugin_root_path.as_deref(), Some("."));
        assert_eq!(
            skill.plugin_root_dir,
            Some(Path::new("/package-root").to_path_buf())
        );
    }

    #[test]
    fn package_loader_pushes_knowledge_pack_manifests() {
        let mut manifest = Manifest::default();
        push_package_resource(
            &mut manifest,
            PackageResourceContext {
                package_name: "@nenjo-ai/knowledge",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "core/manifest.yaml",
                source_path: "nenjo/knowledge/core/manifest.yaml",
                kind: PackageKind::Knowledge,
            },
            RuntimeResourceManifest {
                name: "Nenjo Core".to_string(),
                selector: Some("pkg://nenjo.core/".to_string()),
                root_uri: Some("pkg://nenjo/core/".to_string()),
                fields: serde_json::Map::new(),
            },
        )
        .unwrap();
        assert!(manifest.agents.is_empty());
        assert!(manifest.context_blocks.is_empty());
        assert_eq!(manifest.knowledge_packs.len(), 1);
        assert_eq!(
            manifest.knowledge_packs[0].selector,
            "pkg:nenjo_ai.knowledge.nenjo_core"
        );
        // Logical selector is unversioned; slug includes version for coexistence.
        assert!(
            manifest.knowledge_packs[0].slug.as_str().contains("v0_1_0"),
            "slug should embed version label, got {}",
            manifest.knowledge_packs[0].slug
        );
        assert_eq!(manifest.knowledge_packs[0].root_uri, "pkg://nenjo/core/");
        assert_eq!(
            manifest.knowledge_packs[0].version.as_deref(),
            Some("0.1.0")
        );
    }

    #[test]
    fn package_knowledge_multi_version_coexists_with_versioned_slugs() {
        let mut manifest = Manifest::default();
        for version in ["1.0.0", "1.0.1"] {
            push_package_resource(
                &mut manifest,
                PackageResourceContext {
                    package_name: "@nenjo-ai/knowledge",
                    package_version: version,
                    package_source: None,
                    package_root: Path::new("/package-root"),
                    module_path: "core/manifest.yaml",
                    source_path: "nenjo/knowledge/core/manifest.yaml",
                    kind: PackageKind::Knowledge,
                },
                RuntimeResourceManifest {
                    name: "Nenjo Core".to_string(),
                    selector: Some("pkg:nenjo_ai.knowledge.core".to_string()),
                    root_uri: Some("pkg://nenjo/core/".to_string()),
                    fields: serde_json::Map::new(),
                },
            )
            .unwrap();
        }
        assert_eq!(manifest.knowledge_packs.len(), 2);
        assert_eq!(
            manifest.knowledge_packs[0].selector,
            manifest.knowledge_packs[1].selector
        );
        assert_ne!(
            manifest.knowledge_packs[0].slug,
            manifest.knowledge_packs[1].slug
        );
    }

    #[test]
    fn package_defaults_support_minimal_runtime_resource_shapes() {
        let package_name = "@nenjo/minimal";
        let package_version = "0.1.0";

        let agent = with_package_defaults(
            serde_json::json!({ "name": "agent" }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name,
                package_version,
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "agent.yaml",
                source_path: "agent.yaml",
                kind: PackageKind::Agent,
            },
        );
        let _: AgentManifest = serde_json::from_value(agent).unwrap();

        let ability = with_package_defaults(
            serde_json::json!({ "name": "ability" }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name,
                package_version,
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "abilities/ability.yaml",
                source_path: "abilities/ability.yaml",
                kind: PackageKind::Ability,
            },
        );
        let _: AbilityManifest = serde_json::from_value(ability).unwrap();

        let domain = with_package_defaults(
            serde_json::json!({ "name": "domain" }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name,
                package_version,
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "domains/domain.yaml",
                source_path: "domains/domain.yaml",
                kind: PackageKind::Domain,
            },
        );
        let _: DomainManifest = serde_json::from_value(domain).unwrap();

        let context_block = with_package_defaults(
            serde_json::json!({ "name": "context" }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name,
                package_version,
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "context/context.yaml",
                source_path: "context/context.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let _: ContextBlockManifest = serde_json::from_value(context_block).unwrap();

        let mcp_server = with_package_defaults(
            serde_json::json!({ "name": "mcp" }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name,
                package_version,
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "mcp/mcp.yaml",
                source_path: "mcp/mcp.yaml",
                kind: PackageKind::McpServer,
            },
        );
        let _: McpServerManifest = serde_json::from_value(mcp_server).unwrap();
    }

    #[test]
    fn package_defaults_resolve_plugin_mcp_runtime_cwd() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "acme__review_server",
                "transport": "stdio",
                "command": "node",
                "args": ["servers/review.js"],
                "metadata": {
                    "runtime": {
                        "cwd_path": "."
                    }
                }
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@claude-plugin/acme",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: ".mcp.json",
                source_path: ".mcp.json",
                kind: PackageKind::McpServer,
            },
        );
        assert_eq!(value["metadata"]["runtime"]["cwd"], "/package-root");
        let _: McpServerManifest = serde_json::from_value(value).unwrap();
    }

    #[test]
    fn package_defaults_preserve_managed_connector_declarations() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "agent_browser",
                "display_name": "Agent Browser",
                "transport": "stdio",
                "metadata": {
                    "nenjo": {
                        "managed_connector": "agent_browser"
                    }
                }
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "connectors",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "connectors/agent-browser.yaml",
                source_path: "connectors/agent-browser.yaml",
                kind: PackageKind::McpServer,
            },
        );

        assert_eq!(
            value.pointer("/metadata/nenjo/managed_connector"),
            Some(&serde_json::json!("agent_browser"))
        );
        let manifest: McpServerManifest = serde_json::from_value(value).unwrap();
        assert_eq!(manifest.name, "connectors-agent_browser");
    }

    #[test]
    fn package_defaults_fill_missing_agent_memory_profile_fields() {
        let value = with_package_defaults(
            serde_json::json!({
                "name": "guide",
                "prompt_config": {
                    "system_prompt": "",
                    "developer_prompt": "",
                    "templates": {},
                    "memory_profile": {
                        "core_focus": ["user preferences"],
                        "project_focus": ["project architecture"]
                    }
                }
            }),
            PackageDefaults {
                id: uuid::Uuid::nil(),
                package_name: "@nenjo/nenji",
                package_version: "0.1.0",
                package_source: None,
                package_root: Path::new("/package-root"),
                module_path: "agent.yaml",
                source_path: "nenjo/nenji/agent.yaml",
                kind: PackageKind::Agent,
            },
        );
        let agent: nenjo::manifest::AgentManifest = serde_json::from_value(value).unwrap();
        assert_eq!(
            agent.prompt_config.memory_profile.core_focus,
            vec!["user preferences"]
        );
        assert_eq!(
            agent.prompt_config.memory_profile.project_focus,
            vec!["project architecture"]
        );
        assert!(agent.prompt_config.memory_profile.shared_focus.is_empty());
    }

    #[test]
    fn package_loader_resolves_imported_assignment_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join(".nenjo").join("packages");
        let package_root = package_install_path_in_packages_dir(&packages_dir, "nenji", "0.1.0");
        std::fs::create_dir_all(package_root.join("domains")).unwrap();
        std::fs::write(
            root.join("nenpm.lock.yml"),
            r##"
schema: nenjo.lock.v1
packages:
  - name: nenji
    version: "0.1.0"
    manifest_path: nenjo/nenji/package.yaml
    hash: test
    modules:
      - path: agent.yaml
        resource: system
        source_path: nenjo/nenji/agent.yaml
        schema: nenjo.agent.v1
        kind: agent
        name: system
        hash: test
      - path: domains/creator.yaml
        resource: creator
        source_path: nenjo/nenji/domains/creator.yaml
        schema: nenjo.domain.v1
        kind: domain
        name: creator
        hash: test
"##,
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: nenji
version: "0.1.0"
modules:
  - agent.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("agent.yaml"),
            r#"
schema: nenjo.agent.v1
imports:
  domains:
    - ./domains/creator.yaml
manifest:
  name: system
  assignments:
    domains:
      - nenjo/nenji/domains/creator.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("domains").join("creator.yaml"),
            r##"
schema: nenjo.domain.v1
manifest:
  name: creator
  command: "#creator"
"##,
        )
        .unwrap();

        let manifest = load_package_manifest(root, &packages_dir).unwrap();

        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.domains.len(), 1);
        assert_eq!(manifest.domains[0].name, "creator");
        assert_eq!(
            manifest.agents[0].domains,
            vec![nenjo::Slug::derive("creator")]
        );
    }

    #[test]
    fn package_loader_pushes_raw_skill_resources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join(".nenjo").join("packages");
        let package_root = package_install_path_in_packages_dir(&packages_dir, "skills", "0.1.0");
        std::fs::create_dir_all(package_root.join("skills/review/scripts")).unwrap();
        std::fs::write(
            root.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
  - name: skills
    version: "0.1.0"
    manifest_path: package.yaml
    hash: test
    modules:
      - path: skills/review
        resource: review
        source_path: skills/review/SKILL.md
        schema: nenjo.skill.v1
        kind: skill
        name: review
        hash: test
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: skills
version: "0.1.0"
modules:
  - skills/review/
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("skills/review/SKILL.md"),
            r#"---
name: review
description: Review code changes.
---

# Review

Run ${CLAUDE_SKILL_DIR}/scripts/review.sh when useful.
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("skills/review/scripts/review.sh"),
            "#!/bin/sh\n",
        )
        .unwrap();

        let manifest = load_package_manifest(root, &packages_dir).unwrap();

        assert_eq!(manifest.skills.len(), 1);
        let skill = &manifest.skills[0];
        assert_eq!(skill.name, "review");
        assert_eq!(skill.description.as_deref(), Some("Review code changes."));
        assert_eq!(skill.entry_path, "SKILL.md");
        assert_eq!(skill.root_path, "skills/review");
        assert_eq!(skill.root_dir, package_root.join("skills/review"));
        assert_eq!(skill.metadata["package"]["kind"], "skill");
    }

    #[tokio::test]
    async fn package_loader_loads_claude_plugin_command_skill_and_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join(".nenjo").join("packages");
        let package_root = write_installed_claude_plugin_package(
            root,
            &packages_dir,
            "ralph-loop-plugin",
            "0.1.0",
            &ralph_loop_plugin_resources(),
        );

        let manifest = load_package_manifest(root, &packages_dir).unwrap();

        assert_eq!(manifest.commands.len(), 1);
        let command = &manifest.commands[0];
        assert_eq!(command.name, "ralph_loop__ralph_loop");
        assert_eq!(command.command, "/ralph-loop");
        assert_eq!(
            command
                .hooks
                .iter()
                .map(|hook| hook.as_str())
                .collect::<Vec<_>>(),
            vec!["ralph-loop-plugin-ralph_loop_stop_ralph_loop_stop"]
        );

        assert_eq!(manifest.skills.len(), 1);
        let skill = &manifest.skills[0];
        assert_eq!(skill.name, "ralph_loop__ralph_loop");
        assert_eq!(skill.display_name.as_deref(), Some("ralph_loop:ralph_loop"));
        assert_eq!(skill.root_dir, package_root.join("skills/ralph-loop"));
        assert_eq!(
            skill.plugin_root_dir.as_deref(),
            Some(package_root.as_path())
        );
        assert_eq!(
            skill
                .hooks
                .iter()
                .map(|hook| hook.as_str())
                .collect::<Vec<_>>(),
            vec!["ralph-loop-plugin-ralph_loop_stop_ralph_loop_stop"]
        );

        assert_eq!(manifest.hooks.len(), 1);
        let hook = &manifest.hooks[0];
        assert_eq!(
            hook.name,
            "ralph-loop-plugin-ralph_loop_stop_ralph_loop_stop"
        );
        assert_eq!(hook.event, "Stop");
        assert_eq!(
            hook.plugin_root_dir.as_deref(),
            Some(package_root.as_path())
        );

        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(&manifest.skills, &manifest.hooks);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let security = Arc::new(SecurityPolicy::with_workspace_and_runtime_roots(
            workspace.clone(),
            vec![package_root.clone()],
        ));
        let provider = LocalSkillProvider::new(registry, security);

        let loaded = provider.load_skill(skill).await.unwrap();
        assert!(loaded.context.contains("# Ralph Loop"));
        assert_eq!(loaded.hook_scopes.len(), 1);
        assert_eq!(loaded.hook_scopes[0].hooks.len(), 1);
        let canonical_package_root = package_root
            .canonicalize()
            .unwrap_or_else(|_| package_root.clone());
        assert!(loaded.activation_env.contains(&(
            "CLAUDE_PLUGIN_ROOT".to_string(),
            canonical_package_root.to_string_lossy().into_owned()
        )));

        let session_id = uuid::Uuid::new_v4();
        let hook_transcript_dir = root.join("state").join("hooks");
        let runtime = HookRuntime::new(
            session_id,
            &workspace,
            &hook_transcript_dir,
            loaded.hook_scopes.clone(),
        );
        let active_hook = runtime
            .matching_hooks(&HookEvent::Stop, None)
            .into_iter()
            .next()
            .unwrap();
        let messages = vec![
            ChatMessage::user("run the loop".to_string()),
            ChatMessage::assistant("done".to_string()),
        ];

        let execution = runtime
            .execute(
                &active_hook,
                HookRuntimeEvent::Stop {
                    messages: &messages,
                    final_text: "done",
                },
            )
            .await;

        assert!(execution.success, "hook stderr: {}", execution.stderr);
        assert!(!execution.blocked);
        assert_eq!(
            execution.system_message.as_deref(),
            Some("ralph loop hook ran")
        );
        assert!(
            hook_transcript_dir
                .join(format!("{session_id}.jsonl"))
                .exists()
        );
        let hook_input =
            std::fs::read_to_string(workspace.join("ralph-loop-hook-input.json")).unwrap();
        assert!(hook_input.contains("transcript_path"));
        assert_eq!(
            std::fs::read_to_string(workspace.join("ralph-loop-skill-dir.txt")).unwrap(),
            package_root
                .join("skills/ralph-loop")
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("ralph-loop-plugin-root.txt")).unwrap(),
            package_root.to_string_lossy().into_owned()
        );
    }

    #[test]
    fn package_loader_loads_mixed_native_and_claude_plugin_packages() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join(".nenjo").join("packages");
        let native_root = write_installed_native_package(&packages_dir);
        let plugin_resources = ralph_loop_plugin_resources();
        let plugin_root = write_claude_plugin_package_files(
            &packages_dir,
            "ralph-loop-plugin",
            "0.1.0",
            &plugin_resources,
        );
        write_mixed_package_lock_and_index(
            root,
            &packages_dir,
            &native_root,
            &plugin_root,
            &plugin_resources,
        );

        let manifest = load_package_manifest(root, &packages_dir).unwrap();

        assert_eq!(manifest.agents.len(), 1);
        let agent = &manifest.agents[0];
        assert_eq!(agent.name, "native_coder");
        assert!(agent.prompt_locked);
        assert_eq!(agent.abilities, vec!["review_changes"]);
        assert_eq!(agent.domains, vec![nenjo::Slug::derive("creator")]);
        assert_eq!(
            agent.mcp_servers,
            vec![nenjo::Slug::derive("native_tools-review_server")]
        );
        assert_eq!(agent.script_tools, vec![nenjo::Slug::derive("copy_repo")]);

        assert_eq!(manifest.abilities.len(), 1);
        let ability = &manifest.abilities[0];
        assert_eq!(ability.name, "review_changes");
        assert_eq!(ability.source_type, "package");
        assert!(ability.read_only);
        assert_eq!(
            ability.mcp_servers,
            vec![nenjo::Slug::derive("native_tools-review_server")]
        );
        assert_eq!(ability.script_tools, vec![nenjo::Slug::derive("copy_repo")]);

        assert_eq!(manifest.domains.len(), 1);
        let domain = &manifest.domains[0];
        assert_eq!(domain.name, "creator");
        assert_eq!(domain.abilities, vec!["review_changes"]);
        assert_eq!(
            domain.mcp_servers,
            vec![nenjo::Slug::derive("native_tools-review_server")]
        );
        assert_eq!(domain.script_tools, vec![nenjo::Slug::derive("copy_repo")]);

        assert_eq!(manifest.context_blocks.len(), 1);
        let block = &manifest.context_blocks[0];
        assert_eq!(block.name, "guide");
        assert_eq!(block.template, "# Guide\nUse the native package guide.");
        assert_eq!(
            block.path,
            "pkg/native_tools/v0_1_0/native_tools/context_blocks"
        );

        assert_eq!(manifest.mcp_servers.len(), 1);
        let mcp = &manifest.mcp_servers[0];
        assert_eq!(mcp.name, "native_tools-review_server");
        assert_eq!(mcp.source_type, "package");
        assert!(mcp.read_only);

        assert_eq!(manifest.script_tools.len(), 1);
        let script_tool = &manifest.script_tools[0];
        assert_eq!(script_tool.name, "copy_repo");
        assert_eq!(script_tool.root_dir, native_root.join("script_tools"));
        assert_eq!(script_tool.command.path, "scripts/copy-repo.sh");

        assert_eq!(manifest.commands.len(), 1);
        let command = &manifest.commands[0];
        assert_eq!(command.name, "ralph_loop__ralph_loop");
        assert_eq!(command.command, "/ralph-loop");
        assert_eq!(
            command.plugin_root_dir.as_deref(),
            Some(plugin_root.as_path())
        );

        assert_eq!(manifest.skills.len(), 1);
        let skill = &manifest.skills[0];
        assert_eq!(skill.name, "ralph_loop__ralph_loop");
        assert_eq!(skill.root_dir, plugin_root.join("skills/ralph-loop"));
        assert_eq!(
            skill.plugin_root_dir.as_deref(),
            Some(plugin_root.as_path())
        );

        assert_eq!(manifest.hooks.len(), 1);
        let hook = &manifest.hooks[0];
        assert_eq!(
            hook.name,
            "ralph-loop-plugin-ralph_loop_stop_ralph_loop_stop"
        );
        assert_eq!(hook.event, "Stop");
        assert_eq!(hook.plugin_root_dir.as_deref(), Some(plugin_root.as_path()));
    }

    #[test]
    fn package_loader_uses_lock_as_authoritative_module_set() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join(".nenjo").join("packages");
        let package_root = package_install_path_in_packages_dir(&packages_dir, "nenji", "0.1.0");
        std::fs::create_dir_all(package_root.join("abilities")).unwrap();
        std::fs::write(
            root.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
  - name: nenji
    version: "0.1.0"
    manifest_path: nenjo/nenji/package.yaml
    hash: test
    modules:
      - path: abilities/build.yaml
        resource: build_agent
        source_path: nenjo/nenji/capabilities/build/agent.yaml
        schema: nenjo.ability.v1
        kind: ability
        name: build_agent
        hash: test
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: nenji
version: "0.1.0"
modules:
  - abilities/build.yaml
  - abilities/design.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("abilities").join("build.yaml"),
            r#"
schema: nenjo.ability.v1
manifest:
  name: build_agent
  prompt_config:
    developer_prompt: Build agents.
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("abilities").join("design.yaml"),
            r#"
schema: nenjo.ability.v1
manifest:
  name: design_agent
  prompt_config:
    developer_prompt: Design agents.
"#,
        )
        .unwrap();

        let manifest = load_package_manifest(root, &packages_dir).unwrap();

        assert_eq!(manifest.abilities.len(), 1);
        assert_eq!(manifest.abilities[0].name, "build_agent");
    }

    #[test]
    fn package_loader_falls_back_to_lock_manifest_path_for_nested_platform_package() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join("platform_pkgs");
        let package_root =
            package_install_path_in_packages_dir(&packages_dir, "@nenjo-ai/nenji", "1.0.0");
        std::fs::create_dir_all(package_root.join("nenjo/nenji/commands/design")).unwrap();
        std::fs::write(
            packages_dir.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
  - name: "@nenjo-ai/nenji"
    version: "1.0.0"
    manifest_path: nenjo/nenji/package.yaml
    hash: test
    modules:
      - path: commands/design.yaml
        resource: design
        source_path: nenjo/nenji/commands/design.yaml
        schema: nenjo.command.v1
        kind: command
        name: design
        hash: test
"#,
        )
        .unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            r#"{
              "schema": "nenjo.package-index.v1",
              "packages": {
                "@nenjo-ai/nenji@1.0.0": {
                  "name": "@nenjo-ai/nenji",
                  "version": "1.0.0",
                  "root": "@nenjo-ai/nenji@1.0.0",
                  "manifest_path": "package.yaml"
                }
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/package.yaml"),
            r#"
schema: nenjo.package.v1
name: "@nenjo-ai/nenji"
version: "1.0.0"
modules:
  - commands/design.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/commands/design.yaml"),
            r#"
schema: nenjo.command.v1
manifest:
  name: design
  command: /design
  content_path: nenjo/nenji/commands/design/command.md
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/commands/design/command.md"),
            "Design the requested artifact.\n",
        )
        .unwrap();

        let manifest = load_package_manifest(&packages_dir, &packages_dir).unwrap();

        assert_eq!(manifest.commands.len(), 1);
        let command = &manifest.commands[0];
        assert_eq!(command.name, "design");
        assert_eq!(command.entry_path, "command.md");
        assert_eq!(command.root_path, "commands/design");
        assert_eq!(
            command.root_dir,
            package_root.join("nenjo/nenji/commands/design")
        );
    }

    #[test]
    fn package_loader_maps_flat_command_content_path_from_repository_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages_dir = root.join("platform_pkgs");
        let package_root =
            package_install_path_in_packages_dir(&packages_dir, "@nenjo-ai/nenji", "1.0.0");
        std::fs::create_dir_all(package_root.join("commands/design")).unwrap();
        std::fs::write(
            packages_dir.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
  - name: "@nenjo-ai/nenji"
    version: "1.0.0"
    manifest_path: nenjo/nenji/package.yaml
    hash: test
    modules:
      - path: commands/design.yaml
        resource: design
        source_path: nenjo/nenji/commands/design.yaml
        schema: nenjo.command.v1
        kind: command
        name: design
        hash: test
"#,
        )
        .unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            r#"{
              "schema": "nenjo.package-index.v1",
              "packages": {
                "@nenjo-ai/nenji@1.0.0": {
                  "name": "@nenjo-ai/nenji",
                  "version": "1.0.0",
                  "root": "@nenjo-ai/nenji@1.0.0",
                  "manifest_path": "package.yaml"
                }
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: "@nenjo-ai/nenji"
version: "1.0.0"
modules:
  - commands/design.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("commands/design.yaml"),
            r#"
schema: nenjo.command.v1
manifest:
  name: design
  command: /design
  content_path: nenjo/nenji/commands/design/command.md
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("commands/design/command.md"),
            "Design the requested artifact.\n",
        )
        .unwrap();

        let manifest = load_package_manifest(&packages_dir, &packages_dir).unwrap();

        assert_eq!(manifest.commands.len(), 1);
        let command = &manifest.commands[0];
        assert_eq!(command.name, "design");
        assert_eq!(command.entry_path, "command.md");
        assert_eq!(command.root_path, "commands/design");
        assert_eq!(command.root_dir, package_root.join("commands/design"));
    }

    const RALPH_LOOP_PLUGIN_JSON: &str = r#"{
      "name": "Ralph Loop",
      "version": "0.1.0",
      "description": "Fixture Claude plugin with a command, skill, and stop hook."
    }"#;

    const RALPH_LOOP_COMMAND_MD: &str = r#"---
description: Run the Ralph loop workflow.
argument-hint: TASK
---

Use the Ralph loop process for the requested task.
"#;

    const RALPH_LOOP_SKILL_MD: &str = r#"---
name: ralph-loop
description: Use Ralph loop iteration discipline.
hooks:
  - Stop ralph-loop-stop
---

# Ralph Loop

Use this skill when a task should continue iterating until the work is complete.
"#;

    const RALPH_LOOP_HOOKS_JSON: &str = r#"{
      "hooks": {
        "Stop": [
          {
            "matcher": "*",
            "hooks": [
              {
                "type": "command",
                "command": "scripts/ralph-loop-stop.sh"
              }
            ]
          }
        ]
      }
    }"#;

    const RALPH_LOOP_STOP_SH: &str = r#"#!/usr/bin/env bash
set -euo pipefail

payload="$(cat)"
printf '%s' "$payload" > "${NENJO_WORKSPACE_DIR}/ralph-loop-hook-input.json"
printf '%s' "${CLAUDE_SKILL_DIR}" > "${NENJO_WORKSPACE_DIR}/ralph-loop-skill-dir.txt"
printf '%s' "${CLAUDE_PLUGIN_ROOT}" > "${NENJO_WORKSPACE_DIR}/ralph-loop-plugin-root.txt"
test -d "${CLAUDE_PLUGIN_ROOT}"
test -d "${CLAUDE_SKILL_DIR}"
printf '{"decision":"allow","systemMessage":"ralph loop hook ran"}'
"#;

    fn ralph_loop_plugin_resources() -> Vec<ClaudePluginResource> {
        let plugin = parse_claude_plugin_manifest(RALPH_LOOP_PLUGIN_JSON).unwrap();
        let command =
            parse_claude_plugin_command(RALPH_LOOP_COMMAND_MD, "commands/ralph-loop.md").unwrap();
        let skill =
            parse_claude_plugin_skill(RALPH_LOOP_SKILL_MD, "skills/ralph-loop/SKILL.md").unwrap();
        let hooks = parse_claude_plugin_hooks(RALPH_LOOP_HOOKS_JSON).unwrap();

        claude_plugin_resources(&plugin, &[skill], &[command], &hooks, &[], &[], ".").unwrap()
    }

    fn write_installed_native_package(packages_dir: &Path) -> PathBuf {
        let package_root =
            package_install_path_in_packages_dir(packages_dir, "native_tools", "0.1.0");
        std::fs::create_dir_all(package_root.join("agents")).unwrap();
        std::fs::create_dir_all(package_root.join("abilities")).unwrap();
        std::fs::create_dir_all(package_root.join("domains")).unwrap();
        std::fs::create_dir_all(package_root.join("context_blocks")).unwrap();
        std::fs::create_dir_all(package_root.join("mcp")).unwrap();
        std::fs::create_dir_all(package_root.join("script_tools/scripts")).unwrap();

        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: native_tools
version: "0.1.0"
modules:
  - agents/native-coder.yaml
  - abilities/review.yaml
  - domains/creator.yaml
  - context_blocks/guide.yaml
  - mcp/review-server.yaml
  - script_tools/copy-repo.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("agents/native-coder.yaml"),
            r#"
schema: nenjo.agent.v1
manifest:
  name: native_coder
  description: Native fixture coding agent.
  assignments:
    abilities:
      - abilities/review.yaml
    domains:
      - domains/creator.yaml
    mcp_servers:
      - mcp/review-server.yaml
    script_tools:
      - script_tools/copy-repo.yaml
  prompt_config:
    system_prompt: You are a native package fixture agent.
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("abilities/review.yaml"),
            r#"
schema: nenjo.ability.v1
manifest:
  name: review_changes
  description: Review code changes.
  activation_condition: When reviewing code.
  assignments:
    mcp_servers:
      - mcp/review-server.yaml
    script_tools:
      - script_tools/copy-repo.yaml
  prompt_config:
    developer_prompt: Review with evidence.
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("domains/creator.yaml"),
            r##"
schema: nenjo.domain.v1
manifest:
  name: creator
  command: "#creator"
  assignments:
    abilities:
      - abilities/review.yaml
    mcp_servers:
      - mcp/review-server.yaml
    script_tools:
      - script_tools/copy-repo.yaml
  prompt_config:
    developer_prompt_addon: Work in creator mode.
"##,
        )
        .unwrap();
        std::fs::write(
            package_root.join("context_blocks/guide.yaml"),
            r#"
schema: nenjo.context_block.v1
manifest:
  name: guide
  template: |-
    # Guide
    Use the native package guide.
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("mcp/review-server.yaml"),
            r#"
schema: nenjo.mcp_server.v1
manifest:
  name: review_server
  display_name: Review Server
  transport: stdio
  command: node
  args:
    - servers/review.js
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("script_tools/copy-repo.yaml"),
            r#"
schema: nenjo.script_tool.v1
manifest:
  name: copy_repo
  display_name: Copy Repo
  command:
    path: scripts/copy-repo.sh
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("script_tools/scripts/copy-repo.sh"),
            "#!/usr/bin/env bash\necho copy\n",
        )
        .unwrap();

        package_root
    }

    fn write_mixed_package_lock_and_index(
        root: &Path,
        packages_dir: &Path,
        native_root: &Path,
        plugin_root: &Path,
        plugin_resources: &[ClaudePluginResource],
    ) {
        let lock = NenpmLock {
            schema: "nenjo.lock.v1".to_string(),
            packages: vec![
                LockedPackage {
                    name: "native_tools".to_string(),
                    version: "0.1.0".to_string(),
                    manifest_path: "package.yaml".to_string(),
                    hash: "sha256:native".to_string(),
                    source: None,
                    checksum: None,
                    dependencies: BTreeMap::new(),
                    resolved_dependencies: BTreeMap::new(),
                    modules: native_locked_modules(),
                },
                LockedPackage {
                    name: "ralph-loop-plugin".to_string(),
                    version: "0.1.0".to_string(),
                    manifest_path: "package.yaml".to_string(),
                    hash: "sha256:ralph-loop".to_string(),
                    source: None,
                    checksum: None,
                    dependencies: BTreeMap::new(),
                    resolved_dependencies: BTreeMap::new(),
                    modules: plugin_locked_modules(plugin_resources),
                },
            ],
        };
        std::fs::write(
            root.join("nenpm.lock.yml"),
            serde_yaml::to_string(&lock).unwrap(),
        )
        .unwrap();

        let mut packages = BTreeMap::new();
        packages.insert(
            nenjo_nenpm::package_instance_key("native_tools", "0.1.0"),
            nenjo_nenpm::PackageInstallIndexEntry {
                name: "native_tools".to_string(),
                version: "0.1.0".to_string(),
                root: native_root.to_string_lossy().into_owned(),
                manifest_path: "package.yaml".to_string(),
            },
        );
        packages.insert(
            nenjo_nenpm::package_instance_key("ralph-loop-plugin", "0.1.0"),
            nenjo_nenpm::PackageInstallIndexEntry {
                name: "ralph-loop-plugin".to_string(),
                version: "0.1.0".to_string(),
                root: plugin_root.to_string_lossy().into_owned(),
                manifest_path: "package.yaml".to_string(),
            },
        );
        let index = PackageInstallIndex {
            schema: "nenjo.package-index.v1".to_string(),
            packages,
        };
        std::fs::create_dir_all(packages_dir).unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            serde_json::to_string_pretty(&index).unwrap(),
        )
        .unwrap();
    }

    fn native_locked_modules() -> Vec<LockedModule> {
        vec![
            locked_module(
                "agents/native-coder.yaml",
                "agents/native-coder.yaml",
                "nenjo.agent.v1",
                PackageKind::Agent,
                "native_coder",
            ),
            locked_module(
                "abilities/review.yaml",
                "abilities/review.yaml",
                "nenjo.ability.v1",
                PackageKind::Ability,
                "review_changes",
            ),
            locked_module(
                "domains/creator.yaml",
                "domains/creator.yaml",
                "nenjo.domain.v1",
                PackageKind::Domain,
                "creator",
            ),
            locked_module(
                "context_blocks/guide.yaml",
                "context_blocks/guide.yaml",
                "nenjo.context_block.v1",
                PackageKind::ContextBlock,
                "guide",
            ),
            locked_module(
                "mcp/review-server.yaml",
                "mcp/review-server.yaml",
                "nenjo.mcp_server.v1",
                PackageKind::McpServer,
                "review_server",
            ),
            locked_module(
                "script_tools/copy-repo.yaml",
                "script_tools/copy-repo.yaml",
                "nenjo.script_tool.v1",
                PackageKind::ScriptTool,
                "copy_repo",
            ),
        ]
    }

    fn plugin_locked_modules(resources: &[ClaudePluginResource]) -> Vec<LockedModule> {
        resources
            .iter()
            .map(|resource| {
                locked_module(
                    &resource.path,
                    &resource.source_path,
                    &resource.manifest.schema,
                    resource.kind,
                    resource.manifest.name().unwrap(),
                )
            })
            .collect()
    }

    fn locked_module(
        path: &str,
        source_path: &str,
        schema: &str,
        kind: PackageKind,
        name: &str,
    ) -> LockedModule {
        LockedModule {
            path: path.to_string(),
            resource: Some(name.to_string()),
            source_path: source_path.to_string(),
            schema: schema.to_string(),
            kind,
            name: name.to_string(),
            hash: "sha256:test".to_string(),
            imports: Vec::new(),
            files: Vec::new(),
        }
    }

    fn write_installed_claude_plugin_package(
        root: &Path,
        packages_dir: &Path,
        package_name: &str,
        version: &str,
        resources: &[ClaudePluginResource],
    ) -> PathBuf {
        let package_root =
            write_claude_plugin_package_files(packages_dir, package_name, version, resources);

        let modules = resources
            .iter()
            .map(|resource| LockedModule {
                path: resource.path.clone(),
                resource: Some(resource.manifest.name().unwrap().to_string()),
                source_path: resource.source_path.clone(),
                schema: resource.manifest.schema.clone(),
                kind: resource.kind,
                name: resource.manifest.name().unwrap().to_string(),
                hash: "sha256:test".to_string(),
                imports: Vec::new(),
                files: Vec::new(),
            })
            .collect::<Vec<_>>();

        let lock = NenpmLock {
            schema: "nenjo.lock.v1".to_string(),
            packages: vec![LockedPackage {
                name: package_name.to_string(),
                version: version.to_string(),
                manifest_path: "package.yaml".to_string(),
                hash: "sha256:test".to_string(),
                source: None,
                checksum: None,
                dependencies: BTreeMap::new(),
                resolved_dependencies: BTreeMap::new(),
                modules,
            }],
        };
        std::fs::write(
            root.join("nenpm.lock.yml"),
            serde_yaml::to_string(&lock).unwrap(),
        )
        .unwrap();

        let module_lines = resources
            .iter()
            .map(|resource| format!("  - {}", resource.path))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            package_root.join("package.yaml"),
            format!(
                "schema: nenjo.package.v1\nname: {package_name}\nversion: \"{version}\"\nmodules:\n{module_lines}\n"
            ),
        )
        .unwrap();

        for resource in resources {
            let path = package_root.join(&resource.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, serde_yaml::to_string(&resource.manifest).unwrap()).unwrap();
        }

        write_plugin_source_file(
            &package_root,
            ".claude-plugin/plugin.json",
            RALPH_LOOP_PLUGIN_JSON,
        );
        write_plugin_source_file(
            &package_root,
            "commands/ralph-loop.md",
            RALPH_LOOP_COMMAND_MD,
        );
        write_plugin_source_file(
            &package_root,
            "skills/ralph-loop/SKILL.md",
            RALPH_LOOP_SKILL_MD,
        );
        write_plugin_source_file(&package_root, "hooks/hooks.json", RALPH_LOOP_HOOKS_JSON);
        write_plugin_source_file(
            &package_root,
            "scripts/ralph-loop-stop.sh",
            RALPH_LOOP_STOP_SH,
        );

        package_root
    }

    fn write_claude_plugin_package_files(
        packages_dir: &Path,
        package_name: &str,
        version: &str,
        resources: &[ClaudePluginResource],
    ) -> PathBuf {
        let package_root =
            package_install_path_in_packages_dir(packages_dir, package_name, version);
        std::fs::create_dir_all(&package_root).unwrap();

        let module_lines = resources
            .iter()
            .map(|resource| format!("  - {}", resource.path))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            package_root.join("package.yaml"),
            format!(
                "schema: nenjo.package.v1\nname: {package_name}\nversion: \"{version}\"\nmodules:\n{module_lines}\n"
            ),
        )
        .unwrap();

        for resource in resources {
            let path = package_root.join(&resource.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, serde_yaml::to_string(&resource.manifest).unwrap()).unwrap();
        }

        write_plugin_source_file(
            &package_root,
            ".claude-plugin/plugin.json",
            RALPH_LOOP_PLUGIN_JSON,
        );
        write_plugin_source_file(
            &package_root,
            "commands/ralph-loop.md",
            RALPH_LOOP_COMMAND_MD,
        );
        write_plugin_source_file(
            &package_root,
            "skills/ralph-loop/SKILL.md",
            RALPH_LOOP_SKILL_MD,
        );
        write_plugin_source_file(&package_root, "hooks/hooks.json", RALPH_LOOP_HOOKS_JSON);
        write_plugin_source_file(
            &package_root,
            "scripts/ralph-loop-stop.sh",
            RALPH_LOOP_STOP_SH,
        );

        package_root
    }

    fn write_plugin_source_file(package_root: &Path, path: &str, content: &str) {
        let path = package_root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn package_runtime_slugs_are_scoped_and_not_versioned() {
        let source = PackageSource::Git {
            url: "https://github.com/acme/runtime.git".to_string(),
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        };

        assert_eq!(
            package_runtime_slug("@acme/runtime", Some(&source), "Review Server"),
            "acme-review-server"
        );
        assert_eq!(
            package_runtime_slug("@acme/runtime", Some(&source), "review-server"),
            "acme-review-server"
        );
        assert_eq!(
            package_runtime_slug("@acme/runtime", None, "review-server"),
            "acme-review-server"
        );
    }

    #[test]
    #[ignore = "requires NENJO_TEST_PLATFORM_PKGS_ROOT to point at a materialized package root"]
    fn platform_package_loader_loads_materialized_packages() {
        let root = std::env::var("NENJO_TEST_PLATFORM_PKGS_ROOT")
            .expect("set NENJO_TEST_PLATFORM_PKGS_ROOT");
        let root = PathBuf::from(root);
        let manifest = load_package_manifest(&root, &root).unwrap();
        assert!(!manifest.agents.is_empty());
    }
}
