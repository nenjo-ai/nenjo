use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::{Context, anyhow};
use nenjo_packages::{
    LocalPackageResolver, ModuleImport, PackageKind, PackageRegistryManifest, ResolvedModule,
    ResolvedPackage, validate_package_name,
};
use serde::{Deserialize, Serialize};

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
    let root = options.root;
    let registry_path = match options.registry {
        Some(path) => normalize_registry_path(&path)?,
        None => discover_registry_path(&root)?,
    };
    let resolver = LocalPackageResolver::with_registry_path(&root, &registry_path);
    let registry = resolver.load_registry()?;
    let mut packages = BTreeMap::new();

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
        validate_package(&package)
            .with_context(|| format!("failed to validate package {}", package.name))?;
        packages.insert(name.clone(), package);
    }

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

fn validate_package(package: &ResolvedPackage) -> Result<()> {
    let current_selector = package_selector(&package.name)?;
    let dependency_selectors = package
        .dependencies()
        .keys()
        .map(|name| package_selector(name))
        .collect::<Result<BTreeSet<_>>>()?;

    for module in unique_modules(package) {
        validate_module_imports(package, module)?;
        validate_prompt_selectors(module, &current_selector, &dependency_selectors)?;
        validate_knowledge_selectors(module)?;
    }
    validate_context_graph(package)?;
    Ok(())
}

fn validate_module_imports(package: &ResolvedPackage, module: &ResolvedModule) -> Result<()> {
    for import in &module.imports {
        import
            .validate()
            .with_context(|| format!("{} import {}", module.path, import.reference))?;
        if import.surface == "context" {
            let target = resolve_local_import_target(&module.path, &import.reference)
                .with_context(|| {
                    format!("failed to resolve context import {}", import.reference)
                })?;
            let resolved = package.modules.get(&target).ok_or_else(|| {
                anyhow!(
                    "{} imports missing local context module {}",
                    module.path,
                    import.reference
                )
            })?;
            if resolved.kind != PackageKind::ContextBlock {
                bail!(
                    "{} imports {} as context, but target is {}",
                    module.path,
                    import.reference,
                    resolved.kind.as_str()
                );
            }
        }
    }
    Ok(())
}

fn validate_prompt_selectors(
    module: &ResolvedModule,
    package_selector: &str,
    dependency_selectors: &BTreeSet<String>,
) -> Result<()> {
    let mut strings = Vec::new();
    collect_strings(&module.manifest.manifest, &mut strings);
    let imported_context = module
        .imports
        .iter()
        .filter(|import| import.surface == "context")
        .map(|import| context_import_name(&module.path, &import.reference))
        .collect::<Result<BTreeSet<_>>>()?;

    for value in strings {
        for selector in scan_pkg_selectors(value) {
            if !pkg_selector_is_allowed(&selector, package_selector, dependency_selectors) {
                bail!(
                    "{} references pkg selector {}, but {} is not the current package or a package dependency",
                    module.path,
                    selector,
                    selector_to_package_name(&selector)
                );
            }
        }
        for context in scan_context_selectors(value) {
            if !imported_context.contains(&context) {
                bail!(
                    "{} references context.{context}, but it is not declared in wrapper imports.context",
                    module.path
                );
            }
        }
    }
    Ok(())
}

fn validate_knowledge_selectors(module: &ResolvedModule) -> Result<()> {
    if module.kind != PackageKind::Knowledge {
        return Ok(());
    }
    let docs = module
        .manifest
        .manifest
        .get("docs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("{} knowledge manifest must define docs", module.path))?;
    let mut selectors = BTreeSet::new();
    for (index, doc) in docs.iter().enumerate() {
        let selector = doc
            .get("selector")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "{} knowledge doc at index {} must define selector",
                    module.path,
                    index
                )
            })?;
        validate_jinja_selector(selector).with_context(|| {
            format!(
                "{} knowledge doc selector '{}' is not Jinja-compatible",
                module.path, selector
            )
        })?;
        if !selectors.insert(selector.to_string()) {
            bail!(
                "{} declares duplicate knowledge selector '{}'",
                module.path,
                selector
            );
        }
        for edge in doc
            .get("related")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(target) = edge.get("target").and_then(serde_json::Value::as_str) {
                validate_jinja_selector(target).with_context(|| {
                    format!(
                        "{} knowledge doc selector '{}' has invalid related target '{}'",
                        module.path, selector, target
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn validate_jinja_selector(selector: &str) -> Result<()> {
    let selector = selector.trim();
    if selector.is_empty() {
        bail!("selector cannot be empty");
    }
    for segment in selector.split('.') {
        if segment.is_empty() {
            bail!("selector cannot contain empty segments");
        }
        let mut chars = segment.chars();
        let first = chars.next().expect("segment is not empty");
        if !(first == '_' || first.is_ascii_alphabetic()) {
            bail!("selector segment '{segment}' must start with a letter or underscore");
        }
        if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
            bail!(
                "selector segment '{segment}' may contain only letters, numbers, and underscores"
            );
        }
    }
    Ok(())
}

fn validate_context_graph(package: &ResolvedPackage) -> Result<()> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for module in unique_modules(package) {
        if module.kind != PackageKind::ContextBlock {
            continue;
        }
        let key = module.key();
        let mut deps = Vec::new();
        for import in module
            .imports
            .iter()
            .filter(|import| import.surface == "context")
        {
            deps.push(resolve_local_import_target(
                &module.path,
                &import.reference,
            )?);
        }
        graph.insert(key, deps);
    }

    let mut temporary = BTreeSet::new();
    let mut permanent = BTreeSet::new();
    for key in graph.keys() {
        visit_context(key, &graph, &mut temporary, &mut permanent, &mut Vec::new())?;
    }
    Ok(())
}

fn visit_context(
    key: &str,
    graph: &BTreeMap<String, Vec<String>>,
    temporary: &mut BTreeSet<String>,
    permanent: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
) -> Result<()> {
    if permanent.contains(key) {
        return Ok(());
    }
    if !temporary.insert(key.to_string()) {
        stack.push(key.to_string());
        bail!("context import cycle: {}", stack.join(" -> "));
    }
    stack.push(key.to_string());
    for dependency in graph.get(key).into_iter().flatten() {
        if graph.contains_key(dependency) {
            visit_context(dependency, graph, temporary, permanent, stack)?;
        }
    }
    stack.pop();
    temporary.remove(key);
    permanent.insert(key.to_string());
    Ok(())
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
        packages,
    }
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

fn package_selector(package: &str) -> Result<String> {
    validate_package_name(package)?;
    if let Some((scope, name)) = package
        .strip_prefix('@')
        .and_then(|value| value.split_once('/'))
    {
        Ok(format!("pkg.{scope}.{name}"))
    } else {
        Ok(format!("pkg.{package}"))
    }
}

fn pkg_selector_is_allowed(
    selector: &str,
    package_selector: &str,
    dependency_selectors: &BTreeSet<String>,
) -> bool {
    selector == package_selector
        || dependency_selectors.contains(selector)
        || selector_package_slug(selector).is_some_and(|slug| {
            selector_matches_unscoped_package(package_selector, slug)
                || dependency_selectors
                    .iter()
                    .any(|dependency| selector_matches_unscoped_package(dependency, slug))
        })
}

fn selector_matches_unscoped_package(package_selector: &str, slug: &str) -> bool {
    package_selector
        .strip_prefix("pkg.")
        .is_some_and(|package| !package.contains('.') && package == slug)
}

fn selector_package_slug(selector: &str) -> Option<&str> {
    let mut parts = selector.split('.');
    if parts.next()? != "pkg" {
        return None;
    }
    let first = parts.next()?;
    let second = parts.next();
    second.or(Some(first))
}

fn selector_to_package_name(selector: &str) -> String {
    let parts = selector.split('.').collect::<Vec<_>>();
    if parts.len() >= 3 {
        format!("@{}/{}", parts[1], parts[2])
    } else {
        selector.to_string()
    }
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
    scan_selector(value, "pkg.", 2)
        .into_iter()
        .map(|segments| format!("pkg.{}.{}", segments[0], segments[1]))
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

fn context_import_name(module_path: &str, reference: &str) -> Result<String> {
    if let Some(fragment) = reference.trim().strip_prefix('#') {
        return Ok(fragment.to_string());
    }
    let target = resolve_local_import_target(module_path, reference)?;
    let (_, resource) = target
        .rsplit_once('#')
        .ok_or_else(|| anyhow!("context import {reference} did not resolve to a resource"))?;
    Ok(resource.to_string())
}

fn resolve_local_import_target(module_path: &str, reference: &str) -> Result<String> {
    let reference = reference.trim();
    if let Some(fragment) = reference.strip_prefix('#') {
        return Ok(format!("{module_path}#{fragment}"));
    }

    let (path, fragment) = reference
        .split_once('#')
        .map_or((reference, None), |(path, fragment)| (path, Some(fragment)));
    if fragment.is_some_and(str::is_empty) {
        bail!("local import {reference} has empty fragment");
    }
    let base_dir = module_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let normalized = normalize_relative_path(base_dir, path)?;
    let fragment = fragment
        .map(str::to_string)
        .unwrap_or_else(|| inferred_resource_name(&normalized));
    Ok(format!("{normalized}#{fragment}"))
}

fn inferred_resource_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.').map(|(stem, _)| stem))
        .unwrap_or(path)
        .to_string()
}

fn normalize_relative_path(base_dir: &str, path: &str) -> Result<String> {
    let mut components = base_dir
        .split('/')
        .filter(|component| !component.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    bail!("local import path {path} escapes package root");
                }
            }
            value if value.contains('\\') => bail!("invalid local import path {path}"),
            value => components.push(value.to_string()),
        }
    }
    if components.is_empty() {
        bail!("local import path {path} resolved to package root");
    }
    Ok(components.join("/"))
}
