use std::collections::{BTreeMap, BTreeSet};

use crate::{PackageError, Result};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::schema::parse_package_file_schema;
use crate::{
    ManifestSchemaVersion, PackageKind, PackageModule, ResourceSchema, parse_json_or_yaml,
    validate_relative_module_import_path, validate_resource_name, validate_source_path,
};

pub(crate) fn parse_module_file(content: &str, source_path: &str) -> Result<Vec<ResourceManifest>> {
    let value = parse_json_or_yaml(content)
        .with_context(|| format!("failed to parse module file {source_path}"))?;
    let schema = module_schema_from_value(&value, source_path)?;
    if schema == "nenjo.modules.v1" {
        let bundle: ModuleBundle = serde_json::from_value(value)
            .with_context(|| format!("failed to parse module bundle {source_path}"))?;
        bundle.validate(source_path)?;
        Ok(bundle.resources)
    } else {
        let manifest: ResourceManifest = serde_json::from_value(value)
            .with_context(|| format!("failed to parse resource manifest {source_path}"))?;
        manifest
            .validate_wrapper()
            .with_context(|| format!("failed to validate resource wrapper {source_path}"))?;
        Ok(vec![manifest])
    }
}

pub(crate) fn module_file_schema(content: &str, source_path: &str) -> Result<String> {
    let value = parse_json_or_yaml(content)
        .with_context(|| format!("failed to parse module file {source_path}"))?;
    Ok(module_schema_from_value(&value, source_path)?.to_string())
}

fn module_schema_from_value<'a>(
    value: &'a serde_json::Value,
    source_path: &str,
) -> Result<&'a str> {
    value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PackageError::invalid_schema(source_path, "module file is missing schema"))
}

pub(crate) fn normalize_module_reference(path: &str) -> Result<String> {
    let normalized = validate_source_path(path)?;
    if module_reference_is_directory(path) {
        Ok(normalized)
    } else {
        Ok(normalized.trim_end_matches('/').to_string())
    }
}

pub(crate) fn module_reference_is_directory(path: &str) -> bool {
    path.trim().ends_with('/')
}

pub(crate) fn index_child_module_path(index_dir: Option<&str>, child: &str) -> Result<String> {
    let is_directory = module_reference_is_directory(child);
    let child = normalize_module_reference(child)?;
    let path = match index_dir {
        Some(dir) if !dir.is_empty() => validate_source_path(&format!("{dir}/{child}")),
        _ => validate_source_path(&child),
    }?;
    if is_directory {
        Ok(format!("{path}/"))
    } else {
        Ok(path)
    }
}

fn extract_module_imports(imports: &BTreeMap<String, serde_json::Value>) -> Vec<ModuleImport> {
    let mut out = Vec::new();
    for (surface, value) in imports {
        match value {
            serde_json::Value::String(reference) => {
                out.push(ModuleImport {
                    surface: surface.clone(),
                    reference: reference.clone(),
                });
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    if let Some(reference) = item.as_str() {
                        out.push(ModuleImport {
                            surface: surface.clone(),
                            reference: reference.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Bundle envelope for a module file that contains multiple resource manifests.
pub struct ModuleBundle {
    /// Bundle schema string, for example `nenjo.modules.v1`.
    pub schema: String,
    /// Resource manifests included in this module file.
    #[serde(default)]
    pub resources: Vec<ResourceManifest>,
}

impl ModuleBundle {
    /// Return the validated bundle schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        parse_package_file_schema(&self.schema, "modules")
    }

    /// Validate bundle schema and all included resource manifests.
    pub fn validate(&self, path: &str) -> Result<()> {
        self.schema_version()
            .with_context(|| format!("{path} has unsupported module bundle schema"))?;
        let mut names = BTreeSet::new();
        for resource in &self.resources {
            resource
                .validate_wrapper()
                .with_context(|| format!("failed to validate bundled resource in {path}"))?;
            resource
                .name()
                .with_context(|| format!("failed to validate bundled resource in {path}"))?;
            let name = resource.name().expect("resource name was just validated");
            if !names.insert(name.to_string()) {
                bail!("{path} declares duplicate bundled resource '{name}'");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Index envelope for a directory of package modules.
pub struct ModuleIndexManifest {
    /// Index schema string, for example `nenjo.module_index.v1`.
    pub schema: String,
    /// Module files or child directories included by this index.
    #[serde(default)]
    pub modules: Vec<PackageModule>,
}

impl ModuleIndexManifest {
    /// Return the validated module index schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        parse_package_file_schema(&self.schema, "module_index")
    }

    /// Validate index schema and module paths.
    pub fn validate(&self, path: &str) -> Result<()> {
        self.schema_version()
            .with_context(|| format!("{path} has unsupported module index schema"))?;
        let mut module_paths = BTreeSet::new();
        for module in &self.modules {
            validate_source_path(&module.path)
                .with_context(|| format!("{path} has invalid module path '{}'", module.path))?;
            if !module_paths.insert(normalize_module_reference(&module.path)?) {
                bail!("{path} declares duplicate module path '{}'", module.path);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Resource manifest envelope stored at a package descriptor's `entry`.
pub struct ResourceManifest {
    /// Resource schema string, for example `nenjo.agent.v1`.
    pub schema: String,
    /// Optional platform or package slug for the resource.
    #[serde(default)]
    pub slug: Option<String>,
    /// Optional root URI used to identify source-managed resources.
    #[serde(default)]
    pub root_uri: Option<String>,
    /// Optional stable selector used when syncing or replacing source-managed resources.
    #[serde(default)]
    pub selector: Option<String>,
    /// Structured module imports declared by this resource wrapper.
    #[serde(default)]
    pub imports: BTreeMap<String, serde_json::Value>,
    /// Resource-specific manifest body.
    #[serde(default)]
    pub manifest: serde_json::Value,
}

impl ResourceManifest {
    /// Return the parsed resource schema.
    pub fn resource_schema(&self) -> Result<ResourceSchema> {
        ResourceSchema::parse(&self.schema)
    }

    /// Return the resource kind declared by `schema`.
    pub fn kind(&self) -> Result<PackageKind> {
        Ok(self.resource_schema()?.kind)
    }

    /// Return the resource schema version declared by `schema`.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        Ok(self.resource_schema()?.version)
    }

    /// Return the resource manifest body as an object.
    pub fn manifest_object(&self) -> Result<&Map<String, serde_json::Value>> {
        self.manifest.as_object().ok_or_else(|| {
            PackageError::invalid_resource_manifest("manifest body must be an object")
        })
    }

    /// Return the required resource name from the manifest body.
    pub fn name(&self) -> Result<&str> {
        self.manifest_object()?
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| PackageError::invalid_resource_manifest("manifest body is missing name"))
    }

    /// Return the optional resource version from the manifest body.
    pub fn version(&self) -> Option<&str> {
        self.manifest
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    /// Return the optional resource slug.
    pub fn slug(&self) -> Option<&str> {
        self.slug.as_deref()
    }

    /// Return the optional source root URI.
    pub fn root_uri(&self) -> Option<&str> {
        self.root_uri.as_deref()
    }

    /// Return the source selector, falling back to `root_uri` when absent.
    pub fn selector(&self) -> Option<&str> {
        self.selector.as_deref().or(self.root_uri())
    }

    /// Return structured module imports declared by this resource manifest.
    pub fn imports(&self) -> Vec<ModuleImport> {
        extract_module_imports(&self.imports)
    }

    /// Validate canonical module wrapper shape.
    pub fn validate_wrapper(&self) -> Result<()> {
        let kind = self.kind()?;
        if self
            .manifest
            .get("imports")
            .and_then(serde_json::Value::as_object)
            .is_some()
        {
            bail!("resource manifest body must not contain imports; use wrapper-level imports");
        }
        for import in self.imports() {
            import.validate()?;
        }
        self.validate_package_authored_manifest_fields(kind)?;
        Ok(())
    }

    fn validate_package_authored_manifest_fields(&self, kind: PackageKind) -> Result<()> {
        if matches!(
            kind,
            PackageKind::Ability | PackageKind::Domain | PackageKind::ContextBlock
        ) && self.manifest_object()?.contains_key("path")
        {
            bail!(
                "{} manifests must not define manifest.path; package resolution derives path from the module location",
                kind.as_str()
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Structured resource-level import discovered from a manifest body.
pub struct ModuleImport {
    /// Import surface, such as `abilities`, `domains`, `mcp_servers`, or `context`.
    pub surface: String,
    /// Raw reference string supplied by the manifest author.
    pub reference: String,
}

impl ModuleImport {
    /// Validate canonical module import references.
    pub fn validate(&self) -> Result<()> {
        if self.surface.trim().is_empty() {
            bail!("module import has empty surface");
        }
        let reference = self.reference.trim();
        if reference.is_empty() {
            bail!("module import on '{}' has empty reference", self.surface);
        }
        if reference.starts_with('@') {
            bail!(
                "module import '{}' on '{}' references a package; package dependencies belong in nenjo.package.v1 dependencies",
                self.reference,
                self.surface
            );
        }
        if reference.starts_with("./") || reference.starts_with("../") {
            let path = reference
                .split_once('#')
                .map_or(reference, |(path, _)| path);
            validate_relative_module_import_path(path)?;
            return Ok(());
        }
        if let Some(fragment) = reference.strip_prefix('#') {
            validate_resource_name(fragment)?;
            return Ok(());
        }
        bail!(
            "module import '{}' on '{}' must be a local ref beginning with ./, ../, or #",
            self.reference,
            self.surface
        );
    }

    /// Return whether this import points at another local package module file.
    pub fn is_local_module_ref(&self) -> bool {
        let reference = self.reference.trim();
        reference.starts_with("./") || reference.starts_with("../")
    }
}
