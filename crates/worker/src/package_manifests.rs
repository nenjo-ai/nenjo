use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nenjo::manifest::{Manifest, ManifestLoader};
use nenjo_nenpm::{
    LockedModule, NenpmLock, PackageInstallIndex, PackageSource,
    package_install_path_in_packages_dir,
};
use nenjo_packages::{
    LocalPackageResolver, PackageKind, PackageResourceLogicalKey, ResourceManifest,
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
        let resolver = LocalPackageResolver::new(&installed_package.root);
        let resolved = resolver
            .resolve_package_manifest(&installed_package.manifest_path)
            .with_context(|| {
                format!(
                    "failed to resolve installed package manifest package={} version={} path={}",
                    package.name, package.version, installed_package.manifest_path
                )
            })?;
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
        let assignment_index = build_assignment_index(&modules, &locked_source_paths);
        for module in modules {
            let mut resource = RuntimeResourceManifest::from_resource_manifest(&module.manifest)?;
            resource.apply_package_assignments(module.kind, &assignment_index)?;
            push_package_resource(
                &mut manifest,
                PackageResourceContext {
                    package_name: &package.name,
                    package_version: package.version.as_str(),
                    package_source: package.source.as_ref(),
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
) -> BTreeMap<String, PackageAssignmentTarget> {
    let mut index = BTreeMap::new();
    for module in modules {
        let target = PackageAssignmentTarget {
            kind: module.kind,
            name: module.name().to_string(),
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
    fields: serde_json::Map<String, Value>,
}

impl RuntimeResourceManifest {
    fn from_resource_manifest(resource: &ResourceManifest) -> Result<Self> {
        let name = resource.name()?.to_string();
        let mut fields = resource.manifest_object()?.clone();
        fields.remove("name");
        Ok(Self { name, fields })
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
            }
            PackageKind::Ability => {
                self.apply_slug_assignments(
                    &assignments,
                    index,
                    "mcp_servers",
                    PackageKind::McpServer,
                    "mcp_servers",
                )?;
            }
            PackageKind::Routine
            | PackageKind::Knowledge
            | PackageKind::Skill
            | PackageKind::Plugin
            | PackageKind::ContextBlock
            | PackageKind::McpServer => {}
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

fn push_package_resource(
    manifest: &mut Manifest,
    context: PackageResourceContext<'_>,
    resource_manifest: RuntimeResourceManifest,
) -> Result<()> {
    match context.kind {
        PackageKind::Routine
        | PackageKind::Knowledge
        | PackageKind::Skill
        | PackageKind::Plugin => return Ok(()),
        PackageKind::Agent
        | PackageKind::Ability
        | PackageKind::Domain
        | PackageKind::ContextBlock
        | PackageKind::McpServer => {}
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
        },
    );
    match context.kind {
        PackageKind::Agent => manifest.agents.push(deserialize_manifest(value)?),
        PackageKind::Ability => manifest.abilities.push(deserialize_manifest(value)?),
        PackageKind::Domain => manifest.domains.push(deserialize_manifest(value)?),
        PackageKind::ContextBlock => manifest.context_blocks.push(deserialize_manifest(value)?),
        PackageKind::McpServer => manifest.mcp_servers.push(deserialize_manifest(value)?),
        PackageKind::Routine
        | PackageKind::Knowledge
        | PackageKind::Skill
        | PackageKind::Plugin => {
            unreachable!("non-runtime package kinds returned before id derivation")
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct PackageResourceContext<'a> {
    package_name: &'a str,
    package_version: &'a str,
    package_source: Option<&'a PackageSource>,
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
}

fn with_package_defaults(mut value: Value, defaults: PackageDefaults<'_>) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object
        .entry("id")
        .or_insert_with(|| Value::String(defaults.id.to_string()));
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
            object.insert(
                "path".to_string(),
                Value::String(derived_resource_path(
                    defaults.package_name,
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
                Value::String(derived_resource_path(
                    defaults.package_name,
                    defaults.module_path,
                )),
            );
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("package_domain")
                .to_string();
            object
                .entry("display_name")
                .or_insert_with(|| Value::String(name));
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
                Value::String(derived_package_context_path(
                    defaults.package_name,
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
                .or_insert_with(|| Value::String(name));
            object
                .entry("transport")
                .or_insert_with(|| Value::String("stdio".to_string()));
            object
                .entry("env_schema")
                .or_insert_with(|| serde_json::json!({}));
        }
        PackageKind::Routine
        | PackageKind::Knowledge
        | PackageKind::Skill
        | PackageKind::Plugin => {}
    }
    value
}

fn default_agent_prompt_config() -> Value {
    serde_json::json!({
        "system_prompt": "",
        "developer_prompt": "",
        "templates": {
            "task": "",
            "chat": "",
            "gate": "",
            "heartbeat": ""
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
    prompt_object.entry("templates").or_insert_with(
        || serde_json::json!({ "task": "", "chat": "", "gate": "", "heartbeat": "" }),
    );
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

fn package_path(package_name: &str) -> String {
    package_name
        .trim_start_matches('@')
        .replace('/', ".")
        .replace('-', "_")
}

fn package_selector_segments(package_name: &str, source: Option<&PackageSource>) -> Vec<String> {
    let mut segments = source
        .and_then(package_source_selector_segments)
        .unwrap_or_else(|| {
            package_name
                .trim_start_matches('@')
                .split('/')
                .filter(|segment| !segment.trim().is_empty())
                .map(selector_segment)
                .collect()
        });
    let leaf = package_leaf_segment(package_name);
    if segments.last().is_none_or(|segment| segment != &leaf) {
        segments.push(leaf);
    }
    segments
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

fn derived_resource_path(package_name: &str, module_path: &str) -> String {
    module_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .filter(|dir| !dir.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| package_path(package_name))
}

fn derived_package_context_path(
    package_name: &str,
    package_source: Option<&PackageSource>,
    module_path: &str,
) -> String {
    let mut segments = vec!["pkg".to_string()];
    segments.extend(package_selector_segments(package_name, package_source));
    if let Some((dir, _)) = module_path.rsplit_once('/') {
        segments.extend(
            dir.split('/')
                .filter(|segment| !segment.trim().is_empty())
                .map(selector_segment),
        );
    }
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::manifest::{
        AbilityManifest, AgentManifest, ContextBlockManifest, DomainManifest, McpServerManifest,
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
                module_path: "context/guide.yaml",
                source_path: "packages/core/context/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.id, uuid::Uuid::nil());
        assert_eq!(block.path, "pkg/nenjo/core/context");
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
                module_path: "context/shared/guide.yaml",
                source_path: "packages/core/context/shared/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo/core/context/shared");
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
                module_path: "memory/remembrance.yml",
                source_path: "nenjo/context/memory/remembrance.yml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo_ai/packages/context/memory");
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
                module_path: "nenji/abilities/design/agent.yml",
                source_path: "nenji/abilities/design/agent.yml",
                kind: PackageKind::Ability,
            },
        );
        let ability: AbilityManifest = serde_json::from_value(value).unwrap();
        assert_eq!(ability.path.as_deref(), Some("nenji/abilities/design"));
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
                module_path: "nenji/domains/creator.yml",
                source_path: "nenji/domains/creator.yml",
                kind: PackageKind::Domain,
            },
        );
        let domain: DomainManifest = serde_json::from_value(value).unwrap();
        assert_eq!(domain.path, "nenji/domains");
        assert_eq!(domain.display_name, "creator");
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
                module_path: "guide.yml",
                source_path: "guide.yml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "pkg/nenjo/core_knowledge");
    }

    #[test]
    fn package_loader_skips_non_runtime_resource_names_before_id_validation() {
        let mut manifest = Manifest::default();
        push_package_resource(
            &mut manifest,
            PackageResourceContext {
                package_name: "@nenjo-ai/knowledge",
                package_version: "0.1.0",
                package_source: None,
                module_path: "core/manifest.yaml",
                source_path: "nenjo/knowledge/core/manifest.yaml",
                kind: PackageKind::Knowledge,
            },
            RuntimeResourceManifest {
                name: "Nenjo Core".to_string(),
                fields: serde_json::Map::new(),
            },
        )
        .unwrap();
        assert!(manifest.agents.is_empty());
        assert!(manifest.context_blocks.is_empty());
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
                module_path: "mcp/mcp.yaml",
                source_path: "mcp/mcp.yaml",
                kind: PackageKind::McpServer,
            },
        );
        let _: McpServerManifest = serde_json::from_value(mcp_server).unwrap();
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
    #[ignore = "requires NENJO_TEST_PLATFORM_PKGS_ROOT to point at a materialized package root"]
    fn platform_package_loader_loads_materialized_packages() {
        let root = std::env::var("NENJO_TEST_PLATFORM_PKGS_ROOT")
            .expect("set NENJO_TEST_PLATFORM_PKGS_ROOT");
        let root = PathBuf::from(root);
        let manifest = load_package_manifest(&root, &root).unwrap();
        assert!(!manifest.agents.is_empty());
    }
}
