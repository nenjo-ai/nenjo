use crate::{PackageError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::{Uuid, uuid};

use crate::{PackageKind, module_reference_is_directory};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageLock {
    /// Root package descriptor path requested by the user.
    pub root_path: String,
    /// Branch, tag, or commit reference requested by the user.
    pub requested_ref: String,
    /// Resolved Git commit SHA used for installation.
    pub resolved_commit_sha: String,
    /// Locked resources in install order.
    pub resources: Vec<PackageLockResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Lockfile entry for one resolved package resource.
pub struct PackageLockResource {
    /// Repository-relative descriptor path.
    pub path: String,
    /// Stable resource kind identifier.
    #[serde(rename = "type")]
    pub kind: String,
    /// Resource name from the manifest body.
    pub name: String,
    /// Optional package descriptor version.
    pub version: Option<String>,
    /// Platform resource identifier created or updated by install.
    pub resource_id: String,
    /// SHA-256 hash of the descriptor and manifest content.
    pub hash: String,
    /// Optional source selector used for source-managed replacement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// Validate a repository-relative package path and return its normalized form.
pub fn validate_source_path(path: &str) -> Result<String> {
    let raw = path.trim();
    if raw.is_empty() || raw.starts_with('/') || raw.contains("..") {
        return Err(PackageError::invalid_path(
            path,
            "paths must be non-empty, relative, and must not contain '..'",
        ));
    }
    let trimmed = raw.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(PackageError::invalid_path(path, "path resolved to empty"));
    }
    Ok(trimmed.to_string())
}

/// Validate a registry package name.
pub fn validate_package_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return Err(PackageError::invalid_package_name(
            name,
            "names must be non-empty and cannot contain whitespace",
        ));
    }
    if trimmed.contains("..") || trimmed.contains('#') || trimmed.contains(':') {
        return Err(PackageError::invalid_package_name(
            name,
            "names cannot contain '..', '#', or ':'",
        ));
    }
    if trimmed.starts_with('@') {
        let Some((scope, package)) = trimmed.split_once('/') else {
            return Err(PackageError::invalid_package_name(
                name,
                "scoped names must include a package segment",
            ));
        };
        if scope.len() <= 1 || package.is_empty() || package.contains('/') {
            return Err(PackageError::invalid_package_name(
                name,
                "scoped names must look like @scope/package",
            ));
        }
    } else if trimmed.contains('/') {
        return Err(PackageError::invalid_package_name(
            name,
            "unscoped names must not include '/'",
        ));
    }
    Ok(())
}

/// Validate an unscoped repository package name.
pub fn validate_package_slug(name: &str) -> Result<()> {
    validate_package_name(name)?;
    let trimmed = name.trim();
    if trimmed.starts_with('@') || trimmed.contains('/') {
        return Err(PackageError::invalid_package_name(
            name,
            "repository package names must be unscoped",
        ));
    }
    Ok(())
}

const PACKAGE_RESOURCE_NAMESPACE: Uuid = uuid!("a2b4e19a-9f34-5cc7-8fc7-c0f2f5f60f1c");

/// Stable logical identity for a package resource.
///
/// Logical keys intentionally omit version, install scope, org id, source id,
/// and install id. Those values belong in metadata. The logical key is the
/// dashboard/runtime identity that survives package upgrades.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageResourceLogicalKey(String);

impl PackageResourceLogicalKey {
    /// Build a logical key from parsed package resource parts.
    pub fn new(
        logical_package_name: &str,
        kind: PackageKind,
        module_path: &str,
        resource_name: &str,
    ) -> Result<Self> {
        validate_package_name(logical_package_name)?;
        let module_path = validate_source_path(module_path)?;
        let resource_name = validate_resource_name(resource_name)?;
        Ok(Self(format!(
            "pkg:{logical_package_name}:{}:{module_path}#{resource_name}",
            kind.as_str()
        )))
    }

    /// Return the serialized logical key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the deterministic platform/runtime UUID for this logical key.
    pub fn resource_id(&self) -> Uuid {
        Uuid::new_v5(&PACKAGE_RESOURCE_NAMESPACE, self.0.as_bytes())
    }
}

impl std::fmt::Display for PackageResourceLogicalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Exact resolved package resource instance identity.
///
/// Instance keys include the concrete package version and are used for
/// provenance, locks, cache records, and debugging.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageResourceInstanceKey(String);

impl PackageResourceInstanceKey {
    /// Build an instance key from parsed package resource parts.
    pub fn new(
        package_name: &str,
        package_version: &str,
        kind: PackageKind,
        module_path: &str,
        resource_name: &str,
    ) -> Result<Self> {
        validate_package_name(package_name)?;
        let package_version = validate_package_version(package_version)?;
        let module_path = validate_source_path(module_path)?;
        let resource_name = validate_resource_name(resource_name)?;
        Ok(Self(format!(
            "pkg:{package_name}@{package_version}:{}:{module_path}#{resource_name}",
            kind.as_str()
        )))
    }

    /// Return the serialized instance key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageResourceInstanceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn validate_package_version(version: &str) -> Result<&str> {
    let trimmed = version.trim();
    if trimmed.is_empty()
        || trimmed.contains(char::is_whitespace)
        || trimmed.contains('#')
        || trimmed.contains(':')
    {
        return Err(PackageError::invalid_package_version(
            version,
            "versions must be non-empty and cannot contain whitespace, '#', or ':'",
        ));
    }
    Ok(trimmed)
}

pub(crate) fn validate_resource_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains(char::is_whitespace)
        || trimmed.contains('#')
        || trimmed.contains(':')
    {
        return Err(PackageError::invalid_resource_name(
            name,
            "resource names must be non-empty and cannot contain whitespace, '#', or ':'",
        ));
    }
    Ok(trimmed)
}

pub(crate) fn validate_relative_module_import_path(path: &str) -> Result<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(PackageError::invalid_module_import(
            "module",
            path,
            "path is empty",
        ));
    }
    if trimmed.starts_with('/') || trimmed.contains('\0') {
        return Err(PackageError::invalid_module_import(
            "module",
            path,
            "path must be relative and cannot contain NUL",
        ));
    }
    let mut depth = 0usize;
    for component in trimmed.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                depth = depth.saturating_sub(1);
            }
            value if value.contains('\\') => {
                return Err(PackageError::invalid_module_import(
                    "module",
                    path,
                    "path cannot contain backslashes",
                ));
            }
            _ => depth += 1,
        }
    }
    Ok(trimmed)
}

pub(crate) fn local_import_module_path(module_path: &str, reference: &str) -> Result<String> {
    let reference = reference.trim();
    if reference.starts_with('#') {
        return Err(PackageError::invalid_module_import(
            "module",
            reference,
            "fragment-only import does not reference another module",
        ));
    }
    let (path, _) = reference
        .split_once('#')
        .map_or((reference, None), |(path, fragment)| (path, Some(fragment)));
    validate_relative_module_import_path(path)?;

    let directory = module_path.rsplit_once('/').map(|(dir, _)| dir);
    normalize_relative_module_path(directory, path)
}

fn normalize_relative_module_path(base_dir: Option<&str>, path: &str) -> Result<String> {
    let mut components = Vec::new();
    if let Some(base_dir) = base_dir {
        for component in base_dir.split('/') {
            if !component.is_empty() {
                components.push(component.to_string());
            }
        }
    }

    let is_directory = module_reference_is_directory(path);
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(PackageError::invalid_module_import(
                        "module",
                        path,
                        "path escapes the package root",
                    ));
                }
            }
            value => components.push(value.to_string()),
        }
    }

    if components.is_empty() {
        return Err(PackageError::invalid_module_import(
            "module",
            path,
            "path resolves to the package root",
        ));
    }
    let normalized = components.join("/");
    if is_directory {
        Ok(format!("{normalized}/"))
    } else {
        Ok(normalized)
    }
}

/// Resolve a package descriptor's entry filename to a repository-relative path.
pub fn package_entry_path(package_path: &str, entry: &str) -> Result<String> {
    let package_path = validate_source_path(package_path)?;
    let entry = validate_source_path(entry)?;
    if entry.contains('/') {
        return Err(PackageError::invalid_path(
            entry,
            "package entry must be relative to the package directory",
        ));
    }
    let Some((dir, _)) = package_path.rsplit_once('/') else {
        return Err(PackageError::invalid_path(
            package_path,
            "package descriptor path must include a directory",
        ));
    };
    validate_source_path(&format!("{dir}/{entry}"))
}

/// Resolve a package-relative module path to a repository-relative source path.
pub fn package_module_source_path(package_path: &str, module_path: &str) -> Result<String> {
    let package_path = validate_source_path(package_path)?;
    let module_path = validate_source_path(module_path)?;
    let Some((dir, _)) = package_path.rsplit_once('/') else {
        return Ok(module_path);
    };
    validate_source_path(&format!("{dir}/{module_path}"))
}

/// Return whether a package version satisfies an exact or caret major requirement.
pub fn version_satisfies(actual: &str, required: &str) -> bool {
    let required = required.trim();
    if let Some(prefix) = required.strip_prefix('^') {
        let actual_major = actual.trim_start_matches('v').split('.').next();
        let required_major = prefix.trim_start_matches('v').split('.').next();
        return actual_major == required_major;
    }
    actual.trim_start_matches('v') == required.trim_start_matches('v')
}

/// Parse JSON or YAML content as a generic JSON value.
pub fn parse_json_or_yaml(content: &str) -> Result<serde_json::Value> {
    serde_json::from_str(content)
        .or_else(|_| serde_yaml::from_str(content))
        .map_err(|error| PackageError::Parse {
            format: "JSON or YAML",
            reason: error.to_string(),
        })
}

/// Parse JSON or YAML content as a concrete deserializable type.
pub fn parse_json_or_yaml_as<T>(content: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(content)
        .or_else(|_| serde_yaml::from_str(content))
        .map_err(|error| PackageError::Parse {
            format: "JSON or YAML",
            reason: error.to_string(),
        })
}

/// Return a `sha256:<hex>` digest string for the provided bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}
