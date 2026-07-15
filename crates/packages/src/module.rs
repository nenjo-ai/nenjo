use std::collections::{BTreeMap, BTreeSet};

use crate::{PackageError, Result};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::schema::parse_package_file_schema;
use crate::{
    ManifestSchemaVersion, ModulePackageManifest, PackageKind, PackageModule, ResourceSchema,
    parse_json_or_yaml, validate_relative_module_import_path, validate_resource_name,
    validate_source_path,
};

pub(crate) fn parse_module_file(content: &str, source_path: &str) -> Result<Vec<ResourceManifest>> {
    if is_skill_markdown_path(source_path) {
        return parse_skill_markdown(content, source_path).map(|manifest| vec![manifest]);
    }

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
    if is_skill_markdown_path(source_path) {
        return Ok("nenjo.skill.v1".to_string());
    }

    let value = parse_json_or_yaml(content)
        .with_context(|| format!("failed to parse module file {source_path}"))?;
    Ok(module_schema_from_value(&value, source_path)?.to_string())
}

fn is_skill_markdown_path(path: &str) -> bool {
    path.rsplit('/').next() == Some("SKILL.md")
}

fn parse_skill_markdown(content: &str, source_path: &str) -> Result<ResourceManifest> {
    let frontmatter = skill_frontmatter(content, source_path)?;
    let frontmatter: serde_json::Value = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("failed to parse skill frontmatter {source_path}"))?;
    let frontmatter = frontmatter.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("skill frontmatter must be an object")
    })?;
    let name = frontmatter
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            PackageError::invalid_resource_manifest("skill frontmatter is missing name")
        })?
        .trim()
        .to_string();
    let description = frontmatter
        .get("description")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let root_path = source_path
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_else(|| ".".to_string());

    Ok(ResourceManifest {
        schema: "nenjo.skill.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: None,
        imports: BTreeMap::new(),
        manifest: serde_json::json!({
            "name": name,
            "description": description,
            "entry_path": "SKILL.md",
            "root_path": root_path,
        }),
    })
}

fn skill_frontmatter<'a>(content: &'a str, source_path: &str) -> Result<&'a str> {
    let Some(rest) = content.strip_prefix("---") else {
        bail!("{source_path} is missing required YAML frontmatter");
    };
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest);
    let Some((frontmatter, _body)) = rest.split_once("\n---") else {
        bail!("{source_path} is missing closing YAML frontmatter marker");
    };
    Ok(frontmatter)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Soft preferred engine hint for install-time candidate sorting.
///
/// Preferred models never auto-bind when multiple org models still match.
pub struct PackagePreferredModel {
    /// Provider id, such as `xai` or `openai`.
    pub provider: String,
    /// Provider model id, such as `grok-2-image`.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Model capability requirement declared by a package resource.
///
/// Requirements are keyed by assignable operation capability (never `chat` or
/// feature capabilities). Optional preferred models reorder install UI/error
/// suggestions only.
pub struct PackageModelRequirement {
    /// Assignable operation capability, such as `generate_image`.
    pub capability: String,
    /// Soft preferred engines for install-time candidate sorting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_models: Vec<PackagePreferredModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Provider-native media capability requirement declared by a package resource.
///
/// Prefer [`PackageModelRequirement`] / `model_requirements` for new packages.
/// This shape remains for dual-read of legacy `media:` manifests.
pub struct PackageMediaRequirement {
    /// Native media capability name, such as `generate_image` or `reference_to_video`.
    pub capability: String,
    /// Optional provider pin, such as `xai` or `openai`.
    #[serde(default)]
    pub provider: Option<String>,
    /// Optional provider model pin. A model pin requires a provider pin.
    #[serde(default)]
    pub model: Option<String>,
}

impl From<PackageMediaRequirement> for PackageModelRequirement {
    fn from(value: PackageMediaRequirement) -> Self {
        let preferred_models = match (value.provider, value.model) {
            (Some(provider), Some(model)) => {
                vec![PackagePreferredModel { provider, model }]
            }
            _ => Vec::new(),
        };
        Self {
            capability: value.capability,
            preferred_models,
        }
    }
}

impl From<&PackageModelRequirement> for PackageMediaRequirement {
    fn from(value: &PackageModelRequirement) -> Self {
        let (provider, model) = value
            .preferred_models
            .first()
            .map(|preferred| {
                (
                    Some(preferred.provider.clone()),
                    Some(preferred.model.clone()),
                )
            })
            .unwrap_or((None, None));
        Self {
            capability: value.capability.clone(),
            provider,
            model,
        }
    }
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

    /// Return model capability requirements declared by this resource manifest.
    ///
    /// Prefers `model_requirements` when present; otherwise dual-reads legacy
    /// `media` entries. Capability IDs must be package-requirement capable
    /// (assignable non-`chat` operations only).
    ///
    /// ```yaml
    /// model_requirements:
    ///   - capability: generate_image
    ///     preferred_models:
    ///       - provider: xai
    ///         model: grok-2-image
    ///   - capability: transcribe_audio
    /// ```
    pub fn model_requirements(&self) -> Result<Vec<PackageModelRequirement>> {
        if let Some(value) = self.manifest.get("model_requirements") {
            return parse_model_requirements(value);
        }
        let Some(value) = self.manifest.get("media") else {
            return Ok(Vec::new());
        };
        let media = parse_media_requirements(value)?;
        let mut requirements = Vec::with_capacity(media.len());
        let mut seen = BTreeSet::new();
        for item in media {
            validate_package_requirement_capability(&item.capability)?;
            if !seen.insert(item.capability.clone()) {
                return Err(PackageError::invalid_resource_manifest(format!(
                    "duplicate model requirement '{}'",
                    item.capability
                )));
            }
            requirements.push(PackageModelRequirement::from(item));
        }
        Ok(requirements)
    }

    /// Return provider-native media requirements declared by this resource manifest.
    ///
    /// Accepts both shorthand capability strings and object entries:
    ///
    /// ```yaml
    /// media:
    ///   - generate_image
    ///   - capability: reference_to_video
    ///     provider: xai
    ///     model: grok-imagine-video
    /// ```
    ///
    /// Prefer [`Self::model_requirements`] for new package authoring. When only
    /// `model_requirements` is present, this dual-reads those entries into the
    /// legacy provider/model pin shape (first preferred model when set).
    pub fn media_requirements(&self) -> Result<Vec<PackageMediaRequirement>> {
        if self.manifest.get("model_requirements").is_some() {
            return Ok(self
                .model_requirements()?
                .iter()
                .map(PackageMediaRequirement::from)
                .collect());
        }
        let Some(value) = self.manifest.get("media") else {
            return Ok(Vec::new());
        };
        parse_media_requirements(value)
    }

    /// Validate canonical module wrapper shape.
    pub fn validate_wrapper(&self) -> Result<()> {
        let kind = self.kind()?;
        if self.slug.is_some() {
            bail!(
                "resource manifests must not define wrapper slug; package resolution derives slug from manifest.name"
            );
        }
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
        self.model_requirements()?;
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

fn parse_model_requirements(value: &serde_json::Value) -> Result<Vec<PackageModelRequirement>> {
    let items = value.as_array().ok_or_else(|| {
        PackageError::invalid_resource_manifest("manifest.model_requirements must be an array")
    })?;
    let mut seen = BTreeSet::new();
    let mut requirements = Vec::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        let requirement = if let Some(capability) = item.as_str() {
            PackageModelRequirement {
                capability: non_empty_requirement_string(
                    capability,
                    &format!("manifest.model_requirements[{index}]"),
                )?
                .to_string(),
                preferred_models: Vec::new(),
            }
        } else if let Some(object) = item.as_object() {
            let capability = object
                .get("capability")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    PackageError::invalid_resource_manifest(format!(
                        "manifest.model_requirements[{index}].capability is required"
                    ))
                })?;
            let preferred_models = parse_preferred_models(
                object.get("preferred_models"),
                &format!("manifest.model_requirements[{index}].preferred_models"),
            )?;
            PackageModelRequirement {
                capability: non_empty_requirement_string(
                    capability,
                    &format!("manifest.model_requirements[{index}].capability"),
                )?
                .to_string(),
                preferred_models,
            }
        } else {
            return Err(PackageError::invalid_resource_manifest(format!(
                "manifest.model_requirements[{index}] must be a capability string or object"
            )));
        };

        validate_package_requirement_capability(&requirement.capability)?;

        if !seen.insert(requirement.capability.clone()) {
            return Err(PackageError::invalid_resource_manifest(format!(
                "duplicate model requirement '{}'",
                requirement.capability
            )));
        }

        requirements.push(requirement);
    }

    Ok(requirements)
}

fn parse_preferred_models(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Vec<PackagePreferredModel>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        PackageError::invalid_resource_manifest(format!("{field} must be an array"))
    })?;
    let mut preferred = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();

    for (index, item) in items.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            PackageError::invalid_resource_manifest(format!(
                "{field}[{index}] must be an object with provider and model"
            ))
        })?;
        let provider = object
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                PackageError::invalid_resource_manifest(format!(
                    "{field}[{index}].provider is required"
                ))
            })?;
        let model = object
            .get("model")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                PackageError::invalid_resource_manifest(format!(
                    "{field}[{index}].model is required"
                ))
            })?;
        let provider =
            non_empty_requirement_string(provider, &format!("{field}[{index}].provider"))?
                .to_string();
        let model =
            non_empty_requirement_string(model, &format!("{field}[{index}].model"))?.to_string();
        let key = (provider.clone(), model.clone());
        if !seen.insert(key) {
            return Err(PackageError::invalid_resource_manifest(format!(
                "duplicate preferred model '{provider}/{model}' in {field}"
            )));
        }
        preferred.push(PackagePreferredModel { provider, model });
    }

    Ok(preferred)
}

fn parse_media_requirements(value: &serde_json::Value) -> Result<Vec<PackageMediaRequirement>> {
    let items = value.as_array().ok_or_else(|| {
        PackageError::invalid_resource_manifest("manifest.media must be an array")
    })?;
    let mut seen = BTreeSet::new();
    let mut requirements = Vec::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        let requirement = if let Some(capability) = item.as_str() {
            PackageMediaRequirement {
                capability: non_empty_requirement_string(
                    capability,
                    &format!("manifest.media[{index}]"),
                )?
                .to_string(),
                provider: None,
                model: None,
            }
        } else if let Some(object) = item.as_object() {
            let capability = object
                .get("capability")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    PackageError::invalid_resource_manifest(format!(
                        "manifest.media[{index}].capability is required"
                    ))
                })?;
            PackageMediaRequirement {
                capability: non_empty_requirement_string(
                    capability,
                    &format!("manifest.media[{index}].capability"),
                )?
                .to_string(),
                provider: optional_requirement_string(
                    object.get("provider"),
                    &format!("manifest.media[{index}].provider"),
                )?,
                model: optional_requirement_string(
                    object.get("model"),
                    &format!("manifest.media[{index}].model"),
                )?,
            }
        } else {
            return Err(PackageError::invalid_resource_manifest(format!(
                "manifest.media[{index}] must be a capability string or object"
            )));
        };

        if requirement.model.is_some() && requirement.provider.is_none() {
            return Err(PackageError::invalid_resource_manifest(
                "pinned media model requires a provider",
            ));
        }

        let key = (
            requirement.capability.clone(),
            requirement.provider.clone(),
            requirement.model.clone(),
        );
        if !seen.insert(key) {
            return Err(PackageError::invalid_resource_manifest(format!(
                "duplicate media requirement '{}'",
                requirement.capability
            )));
        }

        requirements.push(requirement);
    }

    Ok(requirements)
}

fn validate_package_requirement_capability(capability: &str) -> Result<()> {
    match capability.parse::<nenjo_models::ModelCapabilityId>() {
        Ok(nenjo_models::ModelCapabilityId::Chat) => Err(PackageError::invalid_resource_manifest(
            "chat is not allowed in model_requirements; agents imply a primary chat model",
        )),
        Ok(_) => Ok(()),
        Err(_) => Err(PackageError::invalid_resource_manifest(format!(
            "unknown model requirement capability '{capability}'"
        ))),
    }
}

fn non_empty_requirement_string<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PackageError::invalid_resource_manifest(format!(
            "{field} cannot be empty"
        )));
    }
    Ok(trimmed)
}

fn optional_requirement_string(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value.as_str().ok_or_else(|| {
        PackageError::invalid_resource_manifest(format!("{field} must be a string"))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

pub(crate) fn complete_package_resource_manifest(
    mut resource: ResourceManifest,
    package: &ModulePackageManifest,
) -> Result<ResourceManifest> {
    if resource.kind()? == PackageKind::Knowledge {
        if resource.root_uri.is_none() {
            resource.root_uri = knowledge_root_uri(&resource, package)?;
        }
        if resource.selector.is_none() {
            resource.selector = resource
                .manifest
                .get("selector")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string);
        }
    }
    Ok(resource)
}

fn knowledge_root_uri(
    resource: &ResourceManifest,
    package: &ModulePackageManifest,
) -> Result<Option<String>> {
    if let Some(root_uri) = resource
        .manifest
        .get("root_uri")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Some(root_uri.to_string()));
    }
    let pack_id = resource
        .manifest
        .get("pack_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(package.name.as_str());
    Ok(Some(format!("pkg://{}/", pack_id.trim().trim_matches('/'))))
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
