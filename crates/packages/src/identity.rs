use crate::{PackageError, Result};
use nenjo::Slug;
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

/// Canonical GitHub repository coordinate used as the global package registry
/// namespace, for example `@nenjo-ai/packages`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GitHubRepositoryRef(String);

impl GitHubRepositoryRef {
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        validate_package_name(value)?;
        if !value.starts_with('@') {
            return Err(PackageError::invalid_package_name(
                value,
                "GitHub repository references must look like @owner/repository",
            ));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn from_owner_repo(owner: &str, repository: &str) -> Result<Self> {
        Self::parse(&format!("@{owner}/{repository}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn owner(&self) -> &str {
        self.0
            .trim_start_matches('@')
            .split_once('/')
            .map(|(owner, _)| owner)
            .expect("validated GitHub repository reference has an owner")
    }

    pub fn repository(&self) -> &str {
        self.0
            .split_once('/')
            .map(|(_, repository)| repository)
            .expect("validated GitHub repository reference has a repository")
    }
}

impl std::fmt::Display for GitHubRepositoryRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GitHubRepositoryRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Authored, stable package-local resource slug.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageResourceSlug(String);

impl PackageResourceSlug {
    pub fn parse(value: &str) -> Result<Self> {
        Slug::parse(value)
            .map(|slug| Self(slug.into_string()))
            .map_err(|error| PackageError::invalid_resource_name(value, error.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageResourceSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackageResourceSlug {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Canonical authored path of one resolved package resource.
///
/// This is deliberately distinct from a manifest display name. For resources
/// selected from a multi-resource module, the path may include a `#selector`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageResourcePath(String);

impl PackageResourcePath {
    /// Parse and normalize a repository-relative resource path.
    pub fn parse(path: &str) -> Result<Self> {
        validate_source_path(path).map(Self)
    }

    /// Return the normalized authored resource path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build the canonical authored path for a resolved module map entry.
    ///
    /// Single-resource modules retain their source path. Multi-resource module
    /// entries are qualified by their authored selector so they remain unique.
    pub fn for_module(
        module_key: &str,
        module_path: &str,
        source_path: &str,
        resource_selector: &str,
    ) -> Result<Self> {
        let path = if module_key == module_path {
            source_path.to_string()
        } else {
            format!("{source_path}#{resource_selector}")
        };
        Self::parse(&path)
    }

    /// Derive the identity-safe name used inside logical and instance keys.
    pub fn identity_name(&self) -> PackageResourceIdentityName {
        let mut name = String::with_capacity(self.0.len());
        for character in self.0.chars() {
            if character == '#' {
                // Preserve the existing key format for already-valid selectors.
                name.push('.');
            } else if character.is_whitespace() || character == ':' {
                let mut bytes = [0; 4];
                for byte in character.encode_utf8(&mut bytes).as_bytes() {
                    use std::fmt::Write as _;
                    write!(name, "%{byte:02X}").expect("writing to String cannot fail");
                }
            } else {
                name.push(character);
            }
        }
        PackageResourceIdentityName(name)
    }
}

impl std::fmt::Display for PackageResourcePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated key component derived from a package resource path.
///
/// Callers cannot construct this from a display name, which prevents names
/// such as `Shop Manager` from accidentally becoming identity components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageResourceIdentityName(String);

impl PackageResourceIdentityName {
    /// Return the validated identity component.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageResourceIdentityName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical logical and exact identities for one resolved package resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageResourceIdentity {
    resource_path: PackageResourcePath,
    resource_slug: PackageResourceSlug,
    logical_ref: PackageResourceLogicalRef,
    instance_key: PackageResourceInstanceKey,
}

impl PackageResourceIdentity {
    /// Construct all package resource identities from authored graph facts.
    pub fn new(
        repository: &GitHubRepositoryRef,
        package: &str,
        package_version: &str,
        kind: PackageKind,
        resource_slug: &PackageResourceSlug,
        resource_path: &str,
    ) -> Result<Self> {
        let resource_path = PackageResourcePath::parse(resource_path)?;
        Self::from_resource_path(
            repository,
            package,
            package_version,
            kind,
            resource_slug,
            resource_path,
        )
    }

    /// Construct all identities from an already validated authored path.
    pub fn from_resource_path(
        repository: &GitHubRepositoryRef,
        package: &str,
        package_version: &str,
        kind: PackageKind,
        resource_slug: &PackageResourceSlug,
        resource_path: PackageResourcePath,
    ) -> Result<Self> {
        let logical_ref = PackageResourceLogicalRef::new(repository, package, kind, resource_slug)?;
        let instance_key = PackageResourceInstanceKey::new(
            repository,
            package,
            package_version,
            kind,
            resource_slug,
        )?;
        Ok(Self {
            resource_path,
            resource_slug: resource_slug.clone(),
            logical_ref,
            instance_key,
        })
    }

    pub fn resource_path(&self) -> &PackageResourcePath {
        &self.resource_path
    }

    pub fn resource_slug(&self) -> &PackageResourceSlug {
        &self.resource_slug
    }

    pub fn logical_key(&self) -> &PackageResourceLogicalKey {
        &self.logical_ref
    }

    pub fn logical_ref(&self) -> &PackageResourceLogicalRef {
        &self.logical_ref
    }

    pub fn instance_key(&self) -> &PackageResourceInstanceKey {
        &self.instance_key
    }
}

/// Stable logical identity for a package resource.
///
/// Logical keys intentionally omit version, install scope, org id, source id,
/// and install id. Those values belong in metadata. The logical key is the
/// dashboard/runtime identity that survives package upgrades.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageResourceLogicalRef(String);

/// Compatibility name for callers that treated logical references as keys.
pub type PackageResourceLogicalKey = PackageResourceLogicalRef;

impl PackageResourceLogicalRef {
    /// Build the canonical GitHub-repository-qualified logical reference.
    pub fn new(
        repository: &GitHubRepositoryRef,
        package: &str,
        kind: PackageKind,
        resource_slug: &PackageResourceSlug,
    ) -> Result<Self> {
        validate_package_slug(package)?;
        Ok(Self(format!(
            "pkg:{repository}:{package}:{}:{resource_slug}",
            kind.as_str(),
        )))
    }

    /// Parse and validate a canonical logical reference.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let Some(parts) = value.strip_prefix("pkg:") else {
            return Err(PackageError::invalid_resource_name(
                value,
                "logical references must start with 'pkg:'",
            ));
        };
        let components = parts.split(':').collect::<Vec<_>>();
        let [repository, package, kind, resource_slug] = components.as_slice() else {
            return Err(PackageError::invalid_resource_name(
                value,
                "logical references must look like pkg:@owner/repository:package:kind:resource-slug",
            ));
        };
        let repository = GitHubRepositoryRef::parse(repository)?;
        let kind = kind.parse::<PackageKind>()?;
        let resource_slug = PackageResourceSlug::parse(resource_slug)?;
        Self::new(&repository, package, kind, &resource_slug)
    }

    /// Construct a path-based key only for reading pre-logical-ref installs.
    pub fn legacy(
        package_name: &str,
        kind: PackageKind,
        module_path: &str,
        resource_name: &str,
    ) -> Result<Self> {
        validate_package_name(package_name)?;
        let module_path = validate_source_path(module_path)?;
        let resource_name = validate_resource_name(resource_name)?;
        Ok(Self(format!(
            "pkg:{package_name}:{}:{module_path}#{resource_name}",
            kind.as_str()
        )))
    }

    /// Return the serialized logical key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn resource_slug(&self) -> Option<&str> {
        self.canonical_components()
            .map(|(_, _, _, resource_slug)| resource_slug)
    }

    pub fn repository(&self) -> Option<&str> {
        self.canonical_components()
            .map(|(repository, _, _, _)| repository)
    }

    pub fn package(&self) -> Option<&str> {
        self.canonical_components()
            .map(|(_, package, _, _)| package)
    }

    pub fn kind(&self) -> Option<PackageKind> {
        self.canonical_components()
            .and_then(|(_, _, kind, _)| kind.parse().ok())
    }

    fn canonical_components(&self) -> Option<(&str, &str, &str, &str)> {
        if self.0.contains('#') {
            return None;
        }
        let components = self.0.strip_prefix("pkg:")?.split(':').collect::<Vec<_>>();
        let [repository, package, kind, resource_slug] = components.as_slice() else {
            return None;
        };
        Some((repository, package, kind, resource_slug))
    }

    /// Return the deterministic platform/runtime UUID for this logical key.
    pub fn resource_id(&self) -> Uuid {
        Uuid::new_v5(&PACKAGE_RESOURCE_NAMESPACE, self.0.as_bytes())
    }
}

impl<'de> Deserialize<'de> for PackageResourceLogicalRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.contains('#') {
            return Ok(Self(value));
        }
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for PackageResourceLogicalRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Exact resolved package resource instance identity.
///
/// Instance keys include the concrete package version and are used for
/// provenance, locks, cache records, and debugging.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageResourceInstanceKey(String);

impl PackageResourceInstanceKey {
    /// Build an instance key from parsed package resource parts.
    pub fn new(
        repository: &GitHubRepositoryRef,
        package: &str,
        package_version: &str,
        kind: PackageKind,
        resource_slug: &PackageResourceSlug,
    ) -> Result<Self> {
        validate_package_slug(package)?;
        let package_version = validate_package_version(package_version)?;
        Ok(Self(format!(
            "pkg:{repository}:{package}@{package_version}:{}:{resource_slug}",
            kind.as_str(),
        )))
    }

    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let Some(parts) = value.strip_prefix("pkg:") else {
            return Err(PackageError::invalid_resource_name(
                value,
                "instance references must start with 'pkg:'",
            ));
        };
        let components = parts.split(':').collect::<Vec<_>>();
        let [repository, package_version, kind, resource_slug] = components.as_slice() else {
            return Err(PackageError::invalid_resource_name(
                value,
                "instance references must look like pkg:@owner/repository:package@version:kind:resource-slug",
            ));
        };
        let repository = GitHubRepositoryRef::parse(repository)?;
        let (package, version) = package_version.rsplit_once('@').ok_or_else(|| {
            PackageError::invalid_resource_name(value, "instance reference is missing version")
        })?;
        let kind = kind.parse::<PackageKind>()?;
        let resource_slug = PackageResourceSlug::parse(resource_slug)?;
        Self::new(&repository, package, version, kind, &resource_slug)
    }

    pub fn legacy(
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

impl<'de> Deserialize<'de> for PackageResourceInstanceKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.contains('#') {
            return Ok(Self(value));
        }
        Self::parse(&value).map_err(serde::de::Error::custom)
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

/// Return whether a package version satisfies an exact or semantic-version requirement.
pub fn version_satisfies(actual: &str, required: &str) -> bool {
    let Ok(actual) = semver::Version::parse(actual.trim().trim_start_matches('v')) else {
        return false;
    };
    let required = required.trim();
    let requirement = if let Some(required) = required.strip_prefix('^') {
        format!("^{}", required.trim_start_matches('v'))
    } else if required.starts_with(['=', '>', '<', '~', '*']) {
        required.to_string()
    } else {
        format!("={}", required.trim_start_matches('v'))
    };
    semver::VersionReq::parse(&requirement).is_ok_and(|required| required.matches(&actual))
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
