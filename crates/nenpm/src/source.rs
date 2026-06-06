use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;
use anyhow::Context;
use flate2::read::GzDecoder;
use nenjo_packages::{
    ClaudePluginCommand, ClaudePluginHook, ModulePackageManifest, PackageModule,
    claude_plugin_resources, detect_unsupported_claude_plugin_components,
    parse_claude_plugin_command, parse_claude_plugin_hooks, parse_claude_plugin_manifest,
    parse_claude_plugin_mcp_servers, parse_claude_plugin_skill, sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;

/// Source location for a registry package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
    /// Package content comes from a git repository.
    Git {
        /// Git remote URL.
        url: String,
        /// Branch, tag, or commit reference.
        reference: String,
        /// Repository-relative package manifest path.
        manifest_path: String,
    },
    /// Package content comes from an immutable package artifact.
    Artifact {
        /// Artifact URL.
        url: String,
        /// Expected artifact checksum.
        checksum: String,
        /// Artifact-relative package manifest path.
        manifest_path: String,
    },
    /// Package content comes from a direct remote package manifest.
    Remote {
        /// Remote package manifest URL.
        url: String,
        /// Expected manifest checksum when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        checksum: Option<String>,
    },
    /// Package content comes from a local repository checkout.
    Local {
        /// Local repository root.
        root: PathBuf,
        /// Repository-relative package manifest path.
        manifest_path: String,
        /// Package scope used when the local source points at a registry manifest.
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

impl PackageSource {
    /// Return the package manifest path when the source has one.
    pub fn manifest_path(&self) -> Option<&str> {
        match self {
            Self::Git { manifest_path, .. }
            | Self::Artifact { manifest_path, .. }
            | Self::Local { manifest_path, .. } => Some(manifest_path),
            Self::Remote { .. } => None,
        }
    }
}

/// Locally available package source fetched from a registry source record.
#[derive(Debug)]
pub struct FetchedPackageSource {
    /// Local root containing package files.
    pub root: PathBuf,
    /// Root-relative package manifest path.
    pub manifest_path: String,
    _temp_dir: Option<TempDir>,
}

impl FetchedPackageSource {
    fn local(root: PathBuf, manifest_path: String) -> Self {
        Self {
            root,
            manifest_path,
            _temp_dir: None,
        }
    }

    fn temporary(root: PathBuf, manifest_path: String, temp_dir: TempDir) -> Self {
        Self {
            root,
            manifest_path,
            _temp_dir: Some(temp_dir),
        }
    }
}

/// Fetches package sources into local roots for manifest/module resolution.
pub trait PackageSourceFetcher {
    /// Fetch a source record into a local root.
    fn fetch(&self, source: &PackageSource) -> Result<FetchedPackageSource>;
}

/// Default package source fetcher for local, git, artifact, and remote sources.
#[derive(Debug, Clone, Default)]
pub struct DefaultPackageSourceFetcher;

impl DefaultPackageSourceFetcher {
    /// Create the default source fetcher.
    pub fn new() -> Self {
        Self
    }
}

impl PackageSourceFetcher for DefaultPackageSourceFetcher {
    fn fetch(&self, source: &PackageSource) -> Result<FetchedPackageSource> {
        let fetched = match source {
            PackageSource::Local {
                root,
                manifest_path,
                ..
            } => Ok(FetchedPackageSource::local(
                root.clone(),
                manifest_path.clone(),
            )),
            PackageSource::Git {
                url,
                reference,
                manifest_path,
            } => fetch_git_source(url, reference, manifest_path),
            PackageSource::Artifact {
                url,
                checksum,
                manifest_path,
            } => fetch_artifact_source(url, checksum, manifest_path),
            PackageSource::Remote { url, checksum } => fetch_remote_manifest(url, checksum),
        }?;
        adapt_claude_plugin_source(fetched)
    }
}

pub(crate) fn normalize_source_paths(source: PackageSource, manifest_dir: &Path) -> PackageSource {
    match source {
        PackageSource::Local {
            root,
            manifest_path,
            scope,
        } => {
            let root = if root.is_absolute() {
                root
            } else {
                manifest_dir.join(root)
            };
            PackageSource::Local {
                root,
                manifest_path,
                scope,
            }
        }
        other => other,
    }
}

pub(crate) fn normalize_fetch_url(source: &mut PackageSource, base_dir: &Path) {
    match source {
        PackageSource::Artifact { url, .. } | PackageSource::Remote { url, .. } => {
            if is_relative_file_reference(url) {
                *url = base_dir.join(&url).to_string_lossy().to_string();
            }
        }
        PackageSource::Git { .. } | PackageSource::Local { .. } => {}
    }
}

pub(crate) fn validate_package_source(source: &PackageSource) -> Result<()> {
    if let Some(manifest_path) = source.manifest_path() {
        nenjo_packages::validate_source_path(manifest_path)
            .context("package source manifest path is invalid")?;
    }
    match source {
        PackageSource::Git { url, reference, .. } => {
            if url.trim().is_empty() {
                bail!("git source url cannot be empty");
            }
            if reference.trim().is_empty() {
                bail!("git source reference cannot be empty");
            }
        }
        PackageSource::Artifact { url, checksum, .. } => {
            if url.trim().is_empty() {
                bail!("artifact source url cannot be empty");
            }
            if checksum.trim().is_empty() {
                bail!("artifact source checksum cannot be empty");
            }
        }
        PackageSource::Remote { url, .. } => {
            if url.trim().is_empty() {
                bail!("remote source url cannot be empty");
            }
        }
        PackageSource::Local { root, scope, .. } => {
            if root.as_os_str().is_empty() {
                bail!("local source root cannot be empty");
            }
            if let Some(scope) = scope {
                validate_local_scope(scope)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn source_fetch_key(source: &PackageSource) -> String {
    match source {
        PackageSource::Git { url, reference, .. } => format!("git:{url}#{reference}"),
        PackageSource::Artifact { url, checksum, .. } => format!("artifact:{url}#{checksum}"),
        PackageSource::Remote { url, checksum } => {
            format!("remote:{url}#{}", checksum.as_deref().unwrap_or(""))
        }
        PackageSource::Local { root, .. } => format!("local:{}", root.display()),
    }
}

pub fn package_source_scope(source: &PackageSource) -> Option<String> {
    match source {
        PackageSource::Git { url, .. } => github_org_from_url(url).map(github_org_to_scope),
        PackageSource::Artifact { .. } | PackageSource::Remote { .. } => None,
        PackageSource::Local { scope, .. } => scope.clone(),
    }
}

fn validate_local_scope(scope: &str) -> Result<()> {
    if !scope.starts_with('@') || scope.contains('/') {
        bail!("local registry scope must look like @scope");
    }
    nenjo_packages::validate_package_name(&format!("{scope}/package"))
        .context("local registry scope is invalid")?;
    Ok(())
}

fn github_org_to_scope(org: String) -> String {
    format!("@{org}")
}

fn github_org_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };
    let org = path.split('/').next()?.trim();
    if org.is_empty() || org == "." || org == ".." {
        return None;
    }
    Some(org.to_string())
}

pub(crate) fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(fs::read(path).with_context(|| format!("failed to read {url}"))?);
    }
    if !url.contains("://") {
        return Ok(fs::read(url).with_context(|| format!("failed to read {url}"))?);
    }
    let response = reqwest::blocking::get(url)
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?;
    Ok(response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .with_context(|| format!("failed to read response body for {url}"))?)
}

fn is_relative_file_reference(value: &str) -> bool {
    !value.contains("://") && !Path::new(value).is_absolute()
}

fn fetch_git_source(
    url: &str,
    reference: &str,
    manifest_path: &str,
) -> Result<FetchedPackageSource> {
    let temp_dir = tempfile::tempdir().context("failed to create git fetch temp dir")?;
    let checkout = temp_dir.path().join("repo");
    let shallow_status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(reference)
        .arg(url)
        .arg(&checkout)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run git clone for {url}"))?;

    if !shallow_status.success() {
        if checkout.exists() {
            fs::remove_dir_all(&checkout).context("failed to clean failed git checkout")?;
        }
        let clone_status = Command::new("git")
            .arg("clone")
            .arg(url)
            .arg(&checkout)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to run git clone for {url}"))?;
        if !clone_status.success() {
            bail!("git clone failed for {url}");
        }
        let checkout_status = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .arg("checkout")
            .arg(reference)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to run git checkout {reference} for {url}"))?;
        if !checkout_status.success() {
            bail!("git checkout failed for {url} at {reference}");
        }
    }
    Ok(FetchedPackageSource::temporary(
        checkout,
        manifest_path.to_string(),
        temp_dir,
    ))
}

fn fetch_artifact_source(
    url: &str,
    checksum: &str,
    manifest_path: &str,
) -> Result<FetchedPackageSource> {
    let bytes = fetch_bytes(url).with_context(|| format!("failed to fetch artifact {url}"))?;
    let actual = sha256_hex(&bytes);
    if actual != checksum {
        bail!("artifact checksum mismatch for {url}: expected {checksum}, got {actual}");
    }

    let temp_dir = tempfile::tempdir().context("failed to create artifact temp dir")?;
    let root = temp_dir.path().join("artifact");
    fs::create_dir_all(&root).context("failed to create artifact extraction dir")?;
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&root)
        .with_context(|| format!("failed to unpack artifact {url}"))?;
    Ok(FetchedPackageSource::temporary(
        root,
        manifest_path.to_string(),
        temp_dir,
    ))
}

fn fetch_remote_manifest(url: &str, checksum: &Option<String>) -> Result<FetchedPackageSource> {
    let bytes =
        fetch_bytes(url).with_context(|| format!("failed to fetch remote manifest {url}"))?;
    if let Some(expected) = checksum {
        let actual = sha256_hex(&bytes);
        if &actual != expected {
            bail!("remote manifest checksum mismatch for {url}: expected {expected}, got {actual}");
        }
    }
    let temp_dir = tempfile::tempdir().context("failed to create remote manifest temp dir")?;
    let path = temp_dir.path().join("remote.package.yaml");
    let mut file = fs::File::create(&path).context("failed to create remote package manifest")?;
    file.write_all(&bytes)
        .context("failed to write remote package manifest")?;
    Ok(FetchedPackageSource::temporary(
        temp_dir.path().to_path_buf(),
        "remote.package.yaml".to_string(),
        temp_dir,
    ))
}

fn adapt_claude_plugin_source(fetched: FetchedPackageSource) -> Result<FetchedPackageSource> {
    let Some(plugin_root_path) = claude_plugin_root_path(&fetched.manifest_path) else {
        return Ok(fetched);
    };
    let plugin_root = if plugin_root_path == "." {
        fetched.root.clone()
    } else {
        fetched.root.join(&plugin_root_path)
    };
    if !plugin_root.is_dir() {
        bail!(
            "Claude plugin root {} does not exist",
            plugin_root.display()
        );
    }

    let temp_dir = tempfile::tempdir().context("failed to create Claude plugin temp dir")?;
    let synthetic_root = temp_dir.path().join("plugin");
    copy_dir_all(&plugin_root, &synthetic_root).with_context(|| {
        format!(
            "failed to copy Claude plugin root {}",
            plugin_root.display()
        )
    })?;
    write_synthetic_claude_plugin_package(&synthetic_root)?;
    Ok(FetchedPackageSource::temporary(
        synthetic_root,
        "package.yaml".to_string(),
        temp_dir,
    ))
}

fn claude_plugin_root_path(manifest_path: &str) -> Option<String> {
    let path = Path::new(manifest_path);
    if path.file_name()? != "plugin.json" {
        return None;
    }
    let claude_plugin_dir = path.parent()?;
    if claude_plugin_dir.file_name()? != ".claude-plugin" {
        return None;
    }
    let root = claude_plugin_dir.parent().unwrap_or_else(|| Path::new(""));
    if root.as_os_str().is_empty() {
        Some(".".to_string())
    } else {
        Some(root.to_string_lossy().replace('\\', "/"))
    }
}

fn write_synthetic_claude_plugin_package(root: &Path) -> Result<()> {
    let plugin_json_path = root.join(".claude-plugin").join("plugin.json");
    let plugin_json = fs::read_to_string(&plugin_json_path)
        .with_context(|| format!("failed to read {}", plugin_json_path.display()))?;
    let plugin = parse_claude_plugin_manifest(&plugin_json)?;
    let skills = discover_claude_plugin_skills(root)?;
    let commands = discover_claude_plugin_commands(root)?;
    let hooks = discover_claude_plugin_hooks(root)?;
    let mcp_servers = discover_claude_plugin_mcp_servers(root)?;
    let scripts = discover_claude_plugin_scripts(root)?;
    let unsupported = detect_unsupported_claude_plugin_components(top_level_component_paths(root)?);
    let resources = claude_plugin_resources(
        &plugin,
        &skills,
        &commands,
        &hooks,
        &mcp_servers,
        &unsupported,
        ".",
    )?;

    let mut modules = Vec::with_capacity(resources.len());
    for resource in resources {
        let path = resource.path;
        let output_path = root.join(&path);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let content = serde_yaml::to_string(&resource.manifest)
            .context("failed to serialize generated Claude plugin resource")?;
        fs::write(&output_path, content)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        modules.push(PackageModule {
            path,
            metadata: serde_json::Value::Null,
        });
    }

    let package = ModulePackageManifest {
        schema: "nenjo.package.v1".to_string(),
        name: plugin.slug.clone(),
        version: plugin
            .version
            .clone()
            .unwrap_or_else(|| "0.1.0".to_string()),
        description: plugin.description.clone(),
        dependencies: Default::default(),
        modules,
        metadata: json!({
            "adapter": "claude_plugin",
            "claude": {
                "plugin": plugin.raw,
                "dependencies": plugin.dependencies,
                "components": {
                    "skills": skills,
                    "commands": commands,
                    "hooks": hooks,
                    "scripts": scripts,
                    "mcp_servers": mcp_servers,
                    "unsupported": unsupported
                },
                "unsupported_components": unsupported
            }
        }),
    };
    let content =
        serde_yaml::to_string(&package).context("failed to serialize Claude plugin package")?;
    fs::write(root.join("package.yaml"), content)
        .with_context(|| format!("failed to write {}", root.join("package.yaml").display()))?;
    Ok(())
}

fn discover_claude_plugin_skills(root: &Path) -> Result<Vec<nenjo_packages::ClaudePluginSkill>> {
    let root_skill_path = root.join("SKILL.md");
    let mut skills = Vec::new();
    if root_skill_path.is_file() {
        let content = fs::read_to_string(&root_skill_path)
            .with_context(|| format!("failed to read {}", root_skill_path.display()))?;
        skills.push(parse_claude_plugin_skill(&content, "SKILL.md")?);
    }

    let skills_dir = root.join("skills");
    if !skills_dir.is_dir() {
        skills.sort_by(|left, right| left.slug.cmp(&right.slug));
        return Ok(skills);
    }
    for entry in fs::read_dir(&skills_dir)
        .with_context(|| format!("failed to read {}", skills_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let skill_name = entry.file_name().to_string_lossy().to_string();
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&skill_path)
            .with_context(|| format!("failed to read {}", skill_path.display()))?;
        let source_path = format!("skills/{skill_name}/SKILL.md");
        skills.push(parse_claude_plugin_skill(&content, &source_path)?);
    }
    skills.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(skills)
}

fn discover_claude_plugin_commands(root: &Path) -> Result<Vec<ClaudePluginCommand>> {
    let commands_dir = root.join("commands");
    if !commands_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut commands = Vec::new();
    for entry in fs::read_dir(&commands_dir)
        .with_context(|| format!("failed to read {}", commands_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let command_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("command.md")
            .to_string();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let source_path = format!("commands/{command_name}");
        commands.push(parse_claude_plugin_command(&content, &source_path)?);
    }
    commands.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(commands)
}

fn discover_claude_plugin_hooks(root: &Path) -> Result<Vec<ClaudePluginHook>> {
    let path = root.join("hooks").join("hooks.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_claude_plugin_hooks(&content)?)
}

fn discover_claude_plugin_mcp_servers(
    root: &Path,
) -> Result<Vec<nenjo_packages::ClaudePluginMcpServer>> {
    let path = root.join(".mcp.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_claude_plugin_mcp_servers(&content)?)
}

fn discover_claude_plugin_scripts(root: &Path) -> Result<Vec<String>> {
    let scripts_dir = root.join("scripts");
    if !scripts_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut scripts = Vec::new();
    collect_relative_files(&scripts_dir, "scripts", &mut scripts)?;
    scripts.sort();
    Ok(scripts)
}

fn collect_relative_files(dir: &Path, relative_dir: &str, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let relative_path = format!("{relative_dir}/{file_name}");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_relative_files(&entry.path(), &relative_path, out)?;
        } else if file_type.is_file() {
            out.push(relative_path);
        }
    }
    Ok(())
}

fn top_level_component_paths(root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let mut path = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            path.push('/');
        }
        paths.push(path);
    }
    Ok(paths)
}

fn copy_dir_all(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&source, &target)?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_packages::{PackageKind, ResourceManifest};

    fn git_source(url: &str) -> PackageSource {
        PackageSource::Git {
            url: url.to_string(),
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        }
    }

    #[test]
    fn derives_scope_from_github_org() {
        assert_eq!(
            package_source_scope(&git_source("https://github.com/nenjo-ai/packages.git")),
            Some("@nenjo-ai".to_string())
        );
        assert_eq!(
            package_source_scope(&git_source("git@github.com:acme/packages.git")),
            Some("@acme".to_string())
        );
    }

    #[test]
    fn non_github_git_sources_do_not_have_scope() {
        assert_eq!(package_source_scope(&git_source("../packages")), None);
        assert_eq!(
            package_source_scope(&git_source("https://gitlab.com/acme/packages.git")),
            None
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_supports_metadata_only_layout() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{
              "name": "Docs Only",
              "version": "0.2.0",
              "description": "Documentation-only plugin"
            }"#,
        );
        write_fixture_file(temp.path(), "README.md", "# Docs Only\n");

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "docs_only");
        assert_eq!(package.version, "0.2.0");
        assert_eq!(package.modules.len(), 1);
        assert_eq!(
            package.modules[0].path,
            ".nenjo/generated/claude-plugin/plugin.yaml"
        );
        assert!(
            package.metadata["claude"]["components"]["skills"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            package.metadata["claude"]["components"]["commands"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            package.metadata["claude"]["components"]["mcp_servers"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        assert_eq!(plugin.kind().unwrap(), PackageKind::Plugin);
        assert!(
            plugin.manifest["unsupported_components"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_supports_command_only_layout() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Command Pack","description":"Slash commands only"}"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/help.md",
            r#"---
description: Show package-specific help.
argument-hint: TOPIC
---
Show help for the requested topic.
"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "command_pack");
        assert_eq!(package.modules.len(), 2);
        assert!(
            package
                .modules
                .iter()
                .any(|module| module.path == ".nenjo/generated/claude-plugin/commands/help.yaml")
        );

        let command = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/commands/help.yaml",
        );
        assert_eq!(command.kind().unwrap(), PackageKind::Command);
        assert_eq!(command.manifest["name"], "command_pack__help");
        assert_eq!(command.manifest["command"], "/help");
        assert_eq!(command.manifest["root_path"], "commands");
        assert_eq!(command.manifest["entry_path"], "help.md");
        assert!(command.manifest["hooks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn synthetic_claude_plugin_package_maps_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{
              "name": "Deploy Kit",
              "dependencies": [
                "audit-logger",
                { "name": "secrets-vault", "version": "~2.1.0" },
                { "name": "shared-kit", "marketplace": "acme-shared", "version": "^1.0" }
              ]
            }"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/deploy.md",
            r#"---
description: Deploy safely.
---
Run the deploy workflow.
"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert!(package.dependencies.is_empty());
        assert_eq!(
            package.metadata["claude"]["dependencies"],
            json!([
                {
                    "kind": "same_marketplace",
                    "name": "audit-logger",
                    "version": null
                },
                {
                    "kind": "same_marketplace",
                    "name": "secrets-vault",
                    "version": "~2.1.0"
                },
                {
                    "kind": "cross_marketplace",
                    "name": "shared-kit",
                    "marketplace": "acme-shared",
                    "version": "^1.0"
                }
            ])
        );

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        assert_eq!(
            plugin.manifest["dependencies"],
            package.metadata["claude"]["dependencies"]
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_discovers_root_skill_and_mcp() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Root Skill Tools","description":"Root skill plus MCP"}"#,
        );
        write_fixture_file(
            temp.path(),
            "SKILL.md",
            r#"---
name: Root Skill
description: Use the root skill.
---
Use `scripts/root.sh` for the workflow.
"#,
        );
        write_fixture_file(
            temp.path(),
            ".mcp.json",
            r#"{
              "mcpServers": {
                "review-server": {
                  "command": "node",
                  "args": ["servers/review.js"],
                  "env": {
                    "MODE": "local",
                    "TOKEN": "$TOKEN"
                  }
                }
              }
            }"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "root_skill_tools");
        assert_eq!(
            package
                .modules
                .iter()
                .map(|module| module.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                ".nenjo/generated/claude-plugin/plugin.yaml",
                ".nenjo/generated/claude-plugin/skills/root_skill.yaml",
                ".nenjo/generated/claude-plugin/mcp/review_server.yaml",
            ]
        );

        let skill = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/skills/root_skill.yaml",
        );
        assert_eq!(skill.kind().unwrap(), PackageKind::Skill);
        assert_eq!(skill.manifest["name"], "root_skill_tools__root_skill");
        assert_eq!(skill.manifest["root_path"], ".");
        assert_eq!(skill.manifest["entry_path"], "SKILL.md");

        let server = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/mcp/review_server.yaml",
        );
        assert_eq!(server.kind().unwrap(), PackageKind::McpServer);
        assert_eq!(server.manifest["name"], "root_skill_tools__review_server");
        assert_eq!(server.manifest["metadata"]["runtime"]["cwd_path"], ".");
        assert_eq!(
            server.manifest["metadata"]["runtime"]["env"]["MODE"],
            "local"
        );
        assert!(server.manifest["metadata"]["runtime"]["env"]["TOKEN"].is_null());
    }

    #[test]
    fn synthetic_claude_plugin_package_discovers_nested_skills_command_and_hooks() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Workflow Pack","description":"Skills, command, hooks"}"#,
        );
        write_fixture_file(
            temp.path(),
            "skills/review/SKILL.md",
            r#"---
name: Review
description: Review code.
---
Use scripts/review.sh.
"#,
        );
        write_fixture_file(
            temp.path(),
            "skills/deploy/SKILL.md",
            r#"---
name: Deploy
description: Deploy safely.
hooks:
  - PreToolUse deploy-pre
---
Use scripts/deploy.sh.
"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/runbook.md",
            r#"---
description: Run the operational runbook.
---
Follow the runbook.
"#,
        );
        write_fixture_file(
            temp.path(),
            "hooks/hooks.json",
            r#"{
              "hooks": {
                "PreToolUse": [
                  {
                    "matcher": "shell",
                    "hooks": [
                      { "type": "command", "command": "scripts/deploy-pre.sh" }
                    ]
                  }
                ],
                "Stop": [
                  {
                    "matcher": "*",
                    "hooks": [
                      { "type": "command", "command": "scripts/runbook-stop.sh" }
                    ]
                  }
                ]
              }
            }"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "workflow_pack");
        assert_eq!(package.modules.len(), 6);

        let deploy = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/skills/deploy.yaml",
        );
        assert_eq!(deploy.kind().unwrap(), PackageKind::Skill);
        assert_eq!(
            deploy.manifest["hooks"],
            json!(["workflow_pack__pretooluse_deploy_pre"])
        );

        let command = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/commands/runbook.yaml",
        );
        assert_eq!(command.kind().unwrap(), PackageKind::Command);
        assert_eq!(
            command.manifest["hooks"],
            json!([
                "workflow_pack__pretooluse_deploy_pre",
                "workflow_pack__stop_runbook_stop"
            ])
        );

        let pre_hook = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/hooks/pretooluse_deploy_pre.yaml",
        );
        assert_eq!(pre_hook.kind().unwrap(), PackageKind::Hook);
        assert_eq!(pre_hook.manifest["event"], "PreToolUse");
        assert_eq!(pre_hook.manifest["matcher"], "shell");
    }

    #[test]
    fn synthetic_claude_plugin_package_records_unsupported_layout_components() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Unsupported Pack","description":"Unsupported components"}"#,
        );
        write_fixture_file(temp.path(), "README.md", "# Unsupported Pack\n");
        write_fixture_file(temp.path(), "agents/reviewer.md", "# Reviewer agent\n");
        write_fixture_file(temp.path(), "lsp/server.json", "{}\n");

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        let unsupported = package.metadata["claude"]["unsupported_components"]
            .as_array()
            .unwrap();
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported.iter().any(|item| item["kind"] == "agents"));
        assert!(unsupported.iter().any(|item| item["kind"] == "lsp"));

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        let unsupported = plugin.manifest["unsupported_components"]
            .as_array()
            .unwrap();
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported.iter().any(|item| item["kind"] == "agents"));
        assert!(unsupported.iter().any(|item| item["kind"] == "lsp"));
    }

    fn write_fixture_file(root: &Path, relative_path: &str, content: &str) {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn read_package_manifest(root: &Path) -> ModulePackageManifest {
        let content = fs::read_to_string(root.join("package.yaml")).unwrap();
        serde_yaml::from_str(&content).unwrap()
    }

    fn read_resource_manifest(root: &Path, relative_path: &str) -> ResourceManifest {
        let content = fs::read_to_string(root.join(relative_path)).unwrap();
        serde_yaml::from_str(&content).unwrap()
    }
}
