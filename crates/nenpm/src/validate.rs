use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use nenjo_packages::{
    LocalPackageResolver, ModuleImport, PackageKind, PackageRegistryManifest,
    PackageRegistryReference, ResolvedModule, ResolvedPackage,
    validation::{PackageRuntimeValidationStage, validate_registry_runtime_with_progress},
};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::dependency::RegistryReference;
use crate::install::resolve_registry_records_parallel;
use crate::registry::{RegistryIndex, RegistryPackageVersion};
use crate::source::DefaultPackageSourceFetcher;

const PREPARED_SCHEMA: &str = "nenjo.prepared_registry.v1";

/// Options for validating a package registry source.
#[derive(Debug, Clone)]
pub struct ValidateOptions {
    /// Registry root directory.
    pub root: PathBuf,
    /// Optional registry manifest path relative to root.
    pub registry: Option<String>,
}

impl ValidateOptions {
    /// Create validation options for a registry root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            registry: None,
        }
    }

    /// Use an explicit registry manifest path.
    pub fn registry(mut self, registry: impl Into<String>) -> Self {
        self.registry = Some(registry.into());
        self
    }
}

/// Validation report for a registry source.
#[derive(Debug, Clone)]
pub struct ValidateReport {
    /// Registry root directory.
    pub root: PathBuf,
    /// Registry manifest path relative to root.
    pub registry_path: String,
    /// Validated registry manifest.
    pub registry: PackageRegistryManifest,
    /// Validated packages keyed by package name.
    pub packages: BTreeMap<String, ResolvedPackage>,
}

/// High-level stages performed by publisher-side registry validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateStage {
    /// Locate or normalize the registry manifest path.
    DiscoverRegistry,
    /// Parse and validate the registry manifest.
    LoadRegistry,
    /// Resolve every package manifest declared by the registry.
    ResolvePackages,
    /// Run shared runtime-readiness validation for the resolved package graph.
    Runtime(PackageRuntimeValidationStage),
}

impl ValidateStage {
    /// Human-readable stage label for console output.
    pub fn label(self) -> &'static str {
        match self {
            Self::DiscoverRegistry => "discovering registry manifest",
            Self::LoadRegistry => "loading registry manifest",
            Self::ResolvePackages => "resolving package graph",
            Self::Runtime(stage) => stage.label(),
        }
    }
}

/// Options for preparing a package registry source.
#[derive(Debug, Clone)]
pub struct PrepareOptions {
    /// Validation options.
    pub validate: ValidateOptions,
    /// Output path for prepared metadata. Defaults to `.nenpm/registry-compiled.json`.
    pub output: Option<PathBuf>,
}

impl PrepareOptions {
    /// Create prepare options for a registry root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            validate: ValidateOptions::new(root),
            output: None,
        }
    }

    /// Use an explicit registry manifest path.
    pub fn registry(mut self, registry: impl Into<String>) -> Self {
        self.validate = self.validate.registry(registry);
        self
    }

    /// Use an explicit output path.
    pub fn output(mut self, output: impl Into<PathBuf>) -> Self {
        self.output = Some(output.into());
        self
    }
}

/// Prepare report with emitted metadata path.
#[derive(Debug, Clone)]
pub struct PrepareReport {
    /// Validation report.
    pub validate: ValidateReport,
    /// Prepared metadata path.
    pub output_path: PathBuf,
    /// Prepared metadata.
    pub compiled: PreparedRegistry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Prepared publisher-side registry metadata.
pub struct PreparedRegistry {
    pub schema: String,
    pub registry_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registries: Vec<PackageRegistryReference>,
    pub packages: Vec<PreparedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedPackage {
    pub name: String,
    pub version: String,
    pub dependencies: BTreeMap<String, String>,
    pub modules: Vec<PreparedModule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedModule {
    pub path: String,
    pub source_path: String,
    pub kind: PackageKind,
    pub name: String,
    pub imports: Vec<ModuleImport>,
    pub prompt_package_selectors: Vec<String>,
    pub prompt_context_selectors: Vec<String>,
}

/// Validate a package registry source.
pub fn validate(options: ValidateOptions) -> Result<ValidateReport> {
    validate_with_progress(options, |_| {})
}

/// Validate a package registry source and report each validation stage.
pub fn validate_with_progress(
    options: ValidateOptions,
    mut progress: impl FnMut(ValidateStage),
) -> Result<ValidateReport> {
    let root = options.root;
    progress(ValidateStage::DiscoverRegistry);
    let registry_path = match options.registry {
        Some(path) => normalize_registry_path(&path)?,
        None => discover_registry_path(&root)?,
    };
    let resolver = LocalPackageResolver::with_registry_path(&root, &registry_path);
    progress(ValidateStage::LoadRegistry);
    let registry = resolver.load_registry()?;
    let mut packages = BTreeMap::new();

    progress(ValidateStage::ResolvePackages);
    for (name, manifest_path) in &registry.packages {
        let package = resolver
            .resolve_package_manifest(manifest_path)
            .with_context(|| format!("failed to resolve registry package {name}"))?;
        if package.name != *name {
            bail!(
                "registry maps {name} to {manifest_path}, but package manifest declares {}",
                package.name
            );
        }
        packages.insert(name.clone(), package);
    }
    let mut validation_packages = packages.clone();
    resolve_external_registry_dependencies(&root, &registry, &mut validation_packages)
        .with_context(|| "failed to resolve external registry dependencies")?;
    validate_registry_runtime_with_progress(&registry, &validation_packages, |stage| {
        progress(ValidateStage::Runtime(stage));
    })
    .with_context(|| "failed package runtime validation")?;

    Ok(ValidateReport {
        root,
        registry_path,
        registry,
        packages,
    })
}

/// Validate and write prepared registry metadata.
pub fn prepare(options: PrepareOptions) -> Result<PrepareReport> {
    let report = validate(options.validate)?;
    let output_path = options
        .output
        .unwrap_or_else(|| report.root.join(".nenpm").join("registry-compiled.json"));
    let compiled = compile_registry(&report);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        &output_path,
        serde_json::to_vec_pretty(&compiled).context("failed to serialize prepared registry")?,
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;

    Ok(PrepareReport {
        validate: report,
        output_path,
        compiled,
    })
}

fn compile_registry(report: &ValidateReport) -> PreparedRegistry {
    let packages = report
        .packages
        .values()
        .map(|package| PreparedPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            dependencies: package.dependencies().clone(),
            modules: unique_modules(package)
                .map(|module| {
                    let prompt_strings = {
                        let mut strings = Vec::new();
                        collect_strings(&module.manifest.manifest, &mut strings);
                        strings
                    };
                    PreparedModule {
                        path: module.path.clone(),
                        source_path: module.source_path.clone(),
                        kind: module.kind,
                        name: module.name().to_string(),
                        imports: module.imports.clone(),
                        prompt_package_selectors: unique_scans(&prompt_strings, scan_pkg_selectors),
                        prompt_context_selectors: unique_scans(
                            &prompt_strings,
                            scan_context_selectors,
                        ),
                    }
                })
                .collect(),
        })
        .collect();
    PreparedRegistry {
        schema: PREPARED_SCHEMA.to_string(),
        registry_path: report.registry_path.clone(),
        registries: report.registry.registries.clone(),
        packages,
    }
}

fn resolve_external_registry_dependencies(
    root: &Path,
    registry: &PackageRegistryManifest,
    packages: &mut BTreeMap<String, ResolvedPackage>,
) -> Result<()> {
    let mut stack = packages
        .values()
        .flat_map(|package| package.dependencies().iter())
        .filter(|(name, _)| !packages.contains_key(*name))
        .map(|(name, requirement)| (name.clone(), requirement.clone()))
        .collect::<Vec<_>>();
    if stack.is_empty() {
        return Ok(());
    }
    if registry.registries.is_empty() {
        let name = stack
            .last()
            .map(|(name, _)| name.as_str())
            .unwrap_or("<unknown>");
        bail!(
            "{name} is required by the package registry, but no external registries are declared"
        );
    }

    let source_fetcher = DefaultPackageSourceFetcher::new();
    let registries = registry
        .registries
        .iter()
        .map(|reference| {
            let reference = RegistryReference::from(reference);
            RegistryIndex::load_reference_with_fetcher(&reference, root, &source_fetcher)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut registry_records = BTreeMap::<String, RegistryPackageVersion>::new();

    while let Some((name, requirement)) = stack.pop() {
        if let Some(existing) = packages.get(&name) {
            ensure_version_satisfies(&name, &existing.version, &requirement)?;
            continue;
        }
        if let Some(existing) = registry_records.get(&name) {
            ensure_version_satisfies(&name, &existing.version, &requirement)?;
            continue;
        }

        let registry = registries
            .iter()
            .find(|registry| registry.packages.contains_key(&name))
            .ok_or_else(|| {
                crate::NenpmError::Message(format!(
                    "{name} is required by the package registry, but no external registry contains it"
                ))
            })?;
        let record = registry
            .resolve_version_matching_all(&name, std::slice::from_ref(&requirement))
            .with_context(|| format!("failed to resolve {name} from external registry"))?;
        for (dependency, requirement) in &record.dependencies {
            stack.push((dependency.clone(), requirement.clone()));
        }
        registry_records.insert(name, record);
    }

    let resolved = resolve_registry_records_parallel(registry_records, &source_fetcher)?;
    merge_validation_packages(packages, resolved.packages)
}

fn ensure_version_satisfies(name: &str, version: &str, requirement: &str) -> Result<()> {
    if !nenjo_packages::version_satisfies(version, requirement) {
        bail!("{name} resolved to {version}, which does not satisfy {requirement}");
    }
    Ok(())
}

fn merge_validation_packages(
    target: &mut BTreeMap<String, ResolvedPackage>,
    source: BTreeMap<String, ResolvedPackage>,
) -> Result<()> {
    for (name, package) in source {
        if let Some(existing) = target.get(&name) {
            if existing.version != package.version {
                bail!(
                    "{name} resolved to both {} and {}",
                    existing.version,
                    package.version
                );
            }
            continue;
        }
        target.insert(name, package);
    }
    Ok(())
}

fn unique_modules(package: &ResolvedPackage) -> impl Iterator<Item = &ResolvedModule> {
    package
        .modules
        .iter()
        .filter(|(key, module)| *key == &module.key())
        .map(|(_, module)| module)
}

fn discover_registry_path(root: &Path) -> Result<String> {
    for candidate in [
        "nenjo.registry.yml",
        "nenjo.registry.yaml",
        "packages.yml",
        "packages.yaml",
    ] {
        if root.join(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    bail!(
        "missing package registry manifest; expected nenjo.registry.yml, nenjo.registry.yaml, packages.yml, or packages.yaml"
    );
}

fn normalize_registry_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains("..") {
        bail!("invalid registry path '{path}'");
    }
    Ok(trimmed.to_string())
}

fn collect_strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(value) => out.push(value),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_strings(value, out);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn unique_scans(values: &[&str], scanner: impl Fn(&str) -> Vec<String>) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| scanner(value))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scan_pkg_selectors(value: &str) -> Vec<String> {
    scan_selector_path(value, "pkg.")
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| {
            let package_segment_count = if segments.len() >= 3 && segments[1] == "packages" {
                3
            } else {
                segments.len().min(2)
            };
            format!(
                "pkg.{}",
                segments
                    .into_iter()
                    .take(package_segment_count)
                    .collect::<Vec<_>>()
                    .join(".")
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scan_context_selectors(value: &str) -> Vec<String> {
    scan_selector(value, "context.", 1)
        .into_iter()
        .map(|segments| segments[0].clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn scan_selector_path(value: &str, prefix: &str) -> Vec<Vec<String>> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(offset) = value[index..].find(prefix) {
        let start = index + offset;
        if start > 0 {
            let previous = bytes[start - 1] as char;
            if is_ident_continue(previous) || previous == '.' {
                index = start + prefix.len();
                continue;
            }
        }
        let mut cursor = start + prefix.len();
        let mut segments = Vec::new();
        while let Some((segment, next_cursor)) = read_ident(value, cursor) {
            segments.push(segment.to_string());
            cursor = next_cursor;
            if value[cursor..].starts_with('.') {
                cursor += 1;
            } else {
                break;
            }
        }
        if !segments.is_empty() {
            out.push(segments);
        }
        index = (start + prefix.len()).min(value.len());
    }
    out
}

fn scan_selector(value: &str, prefix: &str, segment_count: usize) -> Vec<Vec<String>> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(offset) = value[index..].find(prefix) {
        let start = index + offset;
        if start > 0 {
            let previous = bytes[start - 1] as char;
            if is_ident_continue(previous) || previous == '.' {
                index = start + prefix.len();
                continue;
            }
        }
        let mut cursor = start + prefix.len();
        let mut segments = Vec::new();
        for segment_index in 0..segment_count {
            let Some((segment, next_cursor)) = read_ident(value, cursor) else {
                break;
            };
            segments.push(segment.to_string());
            cursor = next_cursor;
            if segment_index + 1 < segment_count {
                if value[cursor..].starts_with('.') {
                    cursor += 1;
                } else {
                    break;
                }
            }
        }
        if segments.len() == segment_count {
            out.push(segments);
        }
        index = (start + prefix.len()).min(value.len());
    }
    out
}

fn read_ident(value: &str, start: usize) -> Option<(&str, usize)> {
    let mut chars = value[start..].char_indices();
    let (_, first) = chars.next()?;
    if !is_ident_start(first) {
        return None;
    }
    let mut end = start + first.len_utf8();
    for (offset, ch) in chars {
        if !is_ident_continue(ch) {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    Some((&value[start..end], end))
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}
