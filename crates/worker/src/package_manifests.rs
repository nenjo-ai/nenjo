use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nenjo::manifest::{Manifest, ManifestLoader};
use nenjo_nenpm::{NenpmLock, PackageInstallIndex, package_install_path_in_packages_dir};
use nenjo_packages::{PackageKind, PackageResourceLogicalKey, parse_json_or_yaml};
use serde::Deserialize;
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
        let package_root = index
            .as_ref()
            .and_then(|index| index.get_package(&package.name, &package.version))
            .map(|entry| package_root_from_index(root, packages_dir, &entry.root))
            .unwrap_or_else(|| {
                package_install_path_in_packages_dir(packages_dir, &package.name, &package.version)
            });
        if !package_root.exists() {
            warn!(
                package = %package.name,
                version = %package.version,
                path = %package_root.display(),
                "Skipping package without a materialized install directory"
            );
            continue;
        }
        for module in package.modules {
            let path = package_root.join(&module.path);
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read package module {}", path.display()))?;
            for resource in parse_package_module(&content)
                .with_context(|| format!("failed to parse package module {}", path.display()))?
            {
                push_package_resource(
                    &mut manifest,
                    &package.name,
                    package.version.as_str(),
                    module.path.as_str(),
                    module.source_path.as_str(),
                    module.kind,
                    resource,
                )?;
            }
        }
    }

    Ok(manifest)
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PackageModuleDocument {
    Bundle { resources: Vec<ResourceEnvelope> },
    Single(ResourceEnvelope),
}

#[derive(Debug, Deserialize)]
struct ResourceEnvelope {
    manifest: RuntimeResourceManifest,
}

#[derive(Debug, Deserialize)]
struct RuntimeResourceManifest {
    name: String,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

impl RuntimeResourceManifest {
    fn into_value(self) -> Value {
        let mut fields = self.fields;
        fields.insert("name".to_string(), Value::String(self.name));
        Value::Object(fields)
    }
}

fn parse_package_module(content: &str) -> Result<Vec<RuntimeResourceManifest>> {
    let value = parse_json_or_yaml(content)?;
    let document: PackageModuleDocument =
        serde_json::from_value(value).context("package module has invalid resource shape")?;
    Ok(match document {
        PackageModuleDocument::Bundle { resources } => resources
            .into_iter()
            .map(|resource| resource.manifest)
            .collect(),
        PackageModuleDocument::Single(resource) => vec![resource.manifest],
    })
}

fn push_package_resource(
    manifest: &mut Manifest,
    package_name: &str,
    package_version: &str,
    module_path: &str,
    source_path: &str,
    kind: PackageKind,
    resource_manifest: RuntimeResourceManifest,
) -> Result<()> {
    let id =
        PackageResourceLogicalKey::new(package_name, kind, module_path, &resource_manifest.name)?
            .resource_id();
    let value = with_package_defaults(
        resource_manifest.into_value(),
        PackageDefaults {
            id,
            package_name,
            package_version,
            module_path,
            source_path,
            kind,
        },
    );
    match kind {
        PackageKind::Agent => manifest.agents.push(deserialize_manifest(value)?),
        PackageKind::Ability => manifest.abilities.push(deserialize_manifest(value)?),
        PackageKind::Domain => manifest.domains.push(deserialize_manifest(value)?),
        PackageKind::ContextBlock => manifest.context_blocks.push(deserialize_manifest(value)?),
        PackageKind::McpServer => manifest.mcp_servers.push(deserialize_manifest(value)?),
        PackageKind::Routine
        | PackageKind::Knowledge
        | PackageKind::Skill
        | PackageKind::Plugin => {}
    }
    Ok(())
}

fn deserialize_manifest<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("failed to deserialize package runtime manifest")
}

struct PackageDefaults<'a> {
    id: uuid::Uuid,
    package_name: &'a str,
    package_version: &'a str,
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
                .entry("domain_ids")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("platform_scopes")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_server_ids")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("ability_ids")
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
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("package_ability")
                .to_string();
            object
                .entry("tool_name")
                .or_insert_with(|| Value::String(name));
            object
                .entry("activation_condition")
                .or_insert_with(|| Value::String(String::new()));
            ensure_ability_prompt_config(object);
            object
                .entry("platform_scopes")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_server_ids")
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
                .entry("ability_ids")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("mcp_server_ids")
                .or_insert_with(|| serde_json::json!([]));
            object
                .entry("prompt_config")
                .or_insert_with(|| serde_json::json!({}));
        }
        PackageKind::ContextBlock => {
            object.insert(
                "path".to_string(),
                Value::String(derived_resource_path(
                    defaults.package_name,
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
            "cron": "",
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
        || serde_json::json!({ "task": "", "chat": "", "gate": "", "cron": "", "heartbeat": "" }),
    );
    prompt_object.entry("memory_profile").or_insert_with(
        || serde_json::json!({ "core_focus": [], "project_focus": [], "shared_focus": [] }),
    );
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

fn derived_resource_path(package_name: &str, module_path: &str) -> String {
    module_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .filter(|dir| !dir.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| package_path(package_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::manifest::{AbilityManifest, ContextBlockManifest, DomainManifest};

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
                module_path: "context/guide.yaml",
                source_path: "packages/core/context/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.id, uuid::Uuid::nil());
        assert_eq!(block.path, "context");
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
                module_path: "context/shared/guide.yaml",
                source_path: "packages/core/context/shared/guide.yaml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "context/shared");
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
                module_path: "nenji/abilities/design/agent.yml",
                source_path: "nenji/abilities/design/agent.yml",
                kind: PackageKind::Ability,
            },
        );
        let ability: AbilityManifest = serde_json::from_value(value).unwrap();
        assert_eq!(ability.path, "nenji/abilities/design");
        assert_eq!(ability.tool_name, "design_agent");
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
                module_path: "guide.yml",
                source_path: "guide.yml",
                kind: PackageKind::ContextBlock,
            },
        );
        let block: ContextBlockManifest = serde_json::from_value(value).unwrap();
        assert_eq!(block.path, "nenjo.core_knowledge");
    }
}
