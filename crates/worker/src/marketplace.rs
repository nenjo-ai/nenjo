//! Marketplace package hydration.
//!
//! The platform stores install metadata only. Workers materialize packages into
//! local runtime-ready directories from GitHub archives or source tarballs.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use nenjo::client::KnowledgePackSyncMeta;
use nenjo::manifest::{AbilityManifest, AbilityPromptConfig, McpServerManifest};
use nenjo_platform::library_knowledge::{
    LIBRARY_KNOWLEDGE_MANIFEST_FILENAME, LibraryKnowledgePackManifest,
    write_library_knowledge_manifest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracing::{info, warn};

const INSTALL_MARKER_FILENAME: &str = ".nenjo-install.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct InstallMetadata {
    #[serde(default)]
    source: SourceMetadata,
    #[serde(default)]
    version: VersionMetadata,
    #[serde(default)]
    distribution: DistributionMetadata,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct SourceMetadata {
    provider: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    path: Option<String>,
    package: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct VersionMetadata {
    ref_: Option<String>,
    requested_ref: Option<String>,
    resolved_commit_sha: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct DistributionMetadata {
    #[serde(rename = "type")]
    kind: Option<String>,
    url: Option<String>,
    sha256: Option<String>,
    path: Option<String>,
    manifest_path: Option<String>,
    entrypoint: Option<String>,
}

pub async fn hydrate_github_knowledge_pack(
    pack: &KnowledgePackSyncMeta,
    nenjo_home: &Path,
) -> Result<()> {
    let metadata: InstallMetadata = serde_json::from_value(pack.metadata.clone())
        .context("failed to parse knowledge pack install metadata")?;
    let package_dir = knowledge_package_dir(nenjo_home, pack, &metadata)?;
    hydrate_package(&metadata, &package_dir)
        .await
        .with_context(|| {
            format!(
                "failed to hydrate knowledge pack {}",
                pack.selector.as_deref().unwrap_or(&pack.slug)
            )
        })?;
    info!(
        pack_slug = %pack.slug,
        selector = ?pack.selector,
        "Hydrated GitHub knowledge pack"
    );
    Ok(())
}

fn knowledge_package_dir(
    nenjo_home: &Path,
    pack: &KnowledgePackSyncMeta,
    metadata: &InstallMetadata,
) -> Result<PathBuf> {
    let owner = metadata.source.owner.as_deref().unwrap_or("local");
    let repo = metadata.source.repo.as_deref().unwrap_or("knowledge");
    let package = metadata
        .source
        .package
        .as_deref()
        .or_else(|| {
            metadata
                .source
                .path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
        })
        .unwrap_or(&pack.slug);
    let version = metadata
        .version
        .resolved_commit_sha
        .as_deref()
        .or(metadata.version.requested_ref.as_deref())
        .or(metadata.version.ref_.as_deref())
        .unwrap_or("unversioned");
    Ok(nenjo_home
        .join("library")
        .join("repos")
        .join("github")
        .join(validate_relative_path(Path::new(owner))?)
        .join(validate_relative_path(Path::new(repo))?)
        .join(validate_relative_path(Path::new(package))?)
        .join(validate_relative_path(Path::new(version))?))
}

pub async fn hydrate_skill_ability(
    ability: AbilityManifest,
    nenjo_home: &Path,
) -> Result<AbilityManifest> {
    if ability.source_type != "skill" {
        return Ok(ability);
    }
    let metadata: InstallMetadata = serde_json::from_value(ability.metadata.clone())
        .context("failed to parse skill install metadata")?;
    let package_dir = skill_package_dir(nenjo_home, &ability, &metadata)?;
    let downloaded = hydrate_raw_package(&metadata, &package_dir)
        .await
        .with_context(|| {
            format!(
                "failed to hydrate skill {}",
                ability
                    .metadata
                    .pointer("/install/selector")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&ability.name)
            )
        })?;
    if downloaded {
        info!(
            ability = %ability.name,
            package_dir = %package_dir.display(),
            "Hydrated marketplace skill"
        );
    }

    let entrypoint = metadata
        .distribution
        .entrypoint
        .as_deref()
        .or(metadata.distribution.manifest_path.as_deref())
        .unwrap_or("SKILL.md");
    let skill_path = safe_join(&package_dir, Path::new(entrypoint))?;
    let skill = std::fs::read_to_string(&skill_path)
        .with_context(|| format!("failed to read {}", skill_path.display()))?;
    let parsed = parse_skill_markdown(&skill);
    let skill_root = skill_path.parent().unwrap_or(package_dir.as_path());
    let references_dir = skill_root.join("references");
    let scripts_dir = skill_root.join("scripts");
    let mut file_guidance = vec![
        format!("Root: {}", skill_root.display()),
        format!("SKILL.md: {}", skill_path.display()),
    ];
    if references_dir.is_dir() {
        file_guidance.push(format!("References: {}", references_dir.display()));
    }
    if scripts_dir.is_dir() {
        file_guidance.push(format!("Scripts: {}", scripts_dir.display()));
    }

    let mut hydrated = ability;
    if hydrated.display_name.is_none() {
        hydrated.display_name = parsed.name.clone();
    }
    if hydrated.description.is_none() {
        hydrated.description = parsed.description.clone();
    }
    if hydrated.activation_condition.trim().is_empty()
        && let Some(description) = parsed.description.as_deref()
    {
        hydrated.activation_condition = description.to_string();
    }
    hydrated.prompt_config = AbilityPromptConfig {
        developer_prompt: format!(
            "{}\n\n<Skill Files>\nInstalled read-only skill support files are available locally.\n{}\n</Skill Files>\n",
            parsed.body.trim(),
            file_guidance.join("\n")
        ),
    };
    Ok(hydrated)
}

pub async fn uninstall_skill_ability(
    ability_id: uuid::Uuid,
    metadata: serde_json::Value,
    nenjo_home: &Path,
) -> Result<()> {
    let metadata: InstallMetadata =
        serde_json::from_value(metadata).context("failed to parse skill uninstall metadata")?;
    let package_dir = skill_package_dir_from_metadata(nenjo_home, &metadata, None)?;
    remove_marked_package_dir(&package_dir, "skill").await?;

    let legacy_dir = legacy_skill_package_dir_from_metadata(nenjo_home, &metadata, None)?;
    if legacy_dir != package_dir {
        remove_marked_package_dir(&legacy_dir, "skill").await?;
    }

    info!(
        %ability_id,
        package_dir = %package_dir.display(),
        "Uninstalled marketplace skill files"
    );
    Ok(())
}

pub async fn uninstall_plugin_mcp_server(
    server_id: uuid::Uuid,
    metadata: serde_json::Value,
    nenjo_home: &Path,
) -> Result<()> {
    let metadata: InstallMetadata =
        serde_json::from_value(metadata).context("failed to parse plugin uninstall metadata")?;
    let package_dir = plugin_package_dir_from_metadata(nenjo_home, &metadata, None)?;
    remove_marked_package_dir(&package_dir, "plugin").await?;

    info!(
        %server_id,
        package_dir = %package_dir.display(),
        "Uninstalled marketplace plugin files"
    );
    Ok(())
}

pub async fn hydrate_plugin_mcp_server(
    server: McpServerManifest,
    nenjo_home: &Path,
) -> Result<McpServerManifest> {
    if server.source_type != "plugin" {
        return Ok(server);
    }
    let metadata: InstallMetadata = serde_json::from_value(server.metadata.clone())
        .context("failed to parse plugin MCP install metadata")?;
    let package_dir = plugin_package_dir(nenjo_home, &server, &metadata)?;
    let downloaded = hydrate_raw_package(&metadata, &package_dir)
        .await
        .with_context(|| {
            format!(
                "failed to hydrate plugin MCP {}",
                server
                    .metadata
                    .pointer("/install/selector")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&server.name)
            )
        })?;
    if downloaded {
        info!(
            server = %server.name,
            package_dir = %package_dir.display(),
            "Hydrated marketplace plugin MCP"
        );
    }

    let mut hydrated = server;
    hydrated.command = hydrated
        .command
        .map(|value| substitute_plugin_paths(&value, &package_dir, nenjo_home));
    hydrated.args = hydrated.args.map(|args| {
        args.into_iter()
            .map(|value| substitute_plugin_paths(&value, &package_dir, nenjo_home))
            .collect()
    });
    hydrated.url = hydrated
        .url
        .map(|value| substitute_plugin_paths(&value, &package_dir, nenjo_home));
    hydrated.metadata =
        substitute_metadata_plugin_paths(hydrated.metadata, &package_dir, nenjo_home);
    Ok(hydrated)
}

async fn hydrate_package(metadata: &InstallMetadata, package_dir: &Path) -> Result<()> {
    let temp = TempDir::new().context("failed to create marketplace temp dir")?;
    let archive = download_package_archive(metadata).await?;
    extract_package_archive(&archive, metadata, temp.path())?;
    normalize_knowledge_package(temp.path(), package_dir, metadata)?;
    Ok(())
}

async fn hydrate_raw_package(metadata: &InstallMetadata, package_dir: &Path) -> Result<bool> {
    let marker = install_marker(metadata)?;
    if installed_marker_matches(package_dir, &marker)? {
        return Ok(false);
    }
    let temp = TempDir::new().context("failed to create marketplace temp dir")?;
    let archive = download_package_archive(metadata).await?;
    extract_package_archive(&archive, metadata, temp.path())?;

    let next_dir = package_dir.with_extension("next");
    if next_dir.exists() {
        std::fs::remove_dir_all(&next_dir)
            .with_context(|| format!("failed to remove {}", next_dir.display()))?;
    }
    copy_dir(temp.path(), &next_dir)?;
    std::fs::write(
        next_dir.join(INSTALL_MARKER_FILENAME),
        serde_json::to_vec_pretty(&marker).context("failed to serialize install marker")?,
    )
    .with_context(|| format!("failed to write install marker in {}", next_dir.display()))?;
    if package_dir.exists() {
        std::fs::remove_dir_all(package_dir)
            .with_context(|| format!("failed to replace {}", package_dir.display()))?;
    }
    std::fs::rename(&next_dir, package_dir).with_context(|| {
        format!(
            "failed to move {} to {}",
            next_dir.display(),
            package_dir.display()
        )
    })?;
    Ok(true)
}

fn installed_marker_matches(package_dir: &Path, marker: &serde_json::Value) -> Result<bool> {
    if !package_dir.exists() {
        return Ok(false);
    }
    let marker_path = package_dir.join(INSTALL_MARKER_FILENAME);
    if !marker_path.exists() {
        return Ok(false);
    }
    let existing = std::fs::read_to_string(&marker_path)
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    let existing: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("failed to parse {}", marker_path.display()))?;
    Ok(&existing == marker)
}

fn install_marker(metadata: &InstallMetadata) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "source": metadata.source,
        "version": metadata.version,
        "distribution": metadata.distribution,
    }))
}

async fn download_package_archive(metadata: &InstallMetadata) -> Result<Vec<u8>> {
    let url = match metadata.distribution.kind.as_deref() {
        Some("github_archive") => metadata
            .distribution
            .url
            .as_deref()
            .ok_or_else(|| anyhow!("github_archive distribution requires url"))?
            .to_string(),
        Some("github_directory") | None => github_tarball_url(metadata)?,
        Some(kind) => bail!("unsupported marketplace distribution type '{kind}'"),
    };

    let bytes = reqwest::get(&url)
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?
        .bytes()
        .await
        .with_context(|| format!("failed to read archive from {url}"))?
        .to_vec();

    if let Some(expected) = metadata.distribution.sha256.as_deref() {
        let digest = Sha256::digest(&bytes);
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if !expected.eq_ignore_ascii_case(&actual) {
            bail!("archive sha256 mismatch: expected {expected}, got {actual}");
        }
    }

    Ok(bytes)
}

fn github_tarball_url(metadata: &InstallMetadata) -> Result<String> {
    let provider = metadata.source.provider.as_deref().unwrap_or("github");
    if provider != "github" {
        bail!("unsupported source provider '{provider}'");
    }
    let owner = required(metadata.source.owner.as_deref(), "source.owner")?;
    let repo = required(metadata.source.repo.as_deref(), "source.repo")?;
    let reference = metadata
        .version
        .resolved_commit_sha
        .as_deref()
        .or(metadata.version.requested_ref.as_deref())
        .or(metadata.version.ref_.as_deref())
        .unwrap_or("HEAD");
    Ok(format!(
        "https://codeload.github.com/{owner}/{repo}/tar.gz/{reference}"
    ))
}

fn extract_package_archive(
    bytes: &[u8],
    metadata: &InstallMetadata,
    destination: &Path,
) -> Result<()> {
    let source_prefix = metadata
        .distribution
        .path
        .as_deref()
        .or(metadata.source.path.as_deref())
        .or(metadata.source.package.as_deref())
        .unwrap_or("")
        .trim_matches('/');
    let strip_source_prefix = matches!(
        metadata.distribution.kind.as_deref(),
        Some("github_directory") | None
    );

    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().context("failed to read tar entry path")?;
        let relative = tar_relative_path(&path, strip_source_prefix, source_prefix)?;
        let Some(relative) = relative else {
            continue;
        };
        let target = safe_join(destination, &relative)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .context("failed to read tar entry content")?;
        std::fs::write(&target, content)
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    Ok(())
}

fn tar_relative_path(
    path: &Path,
    strip_source_prefix: bool,
    source_prefix: &str,
) -> Result<Option<PathBuf>> {
    let mut components = path.components();
    let _top_level = components.next();
    let without_top = components.as_path();
    let relative = if strip_source_prefix && !source_prefix.is_empty() {
        match without_top.strip_prefix(source_prefix) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        }
    } else {
        without_top
    };
    validate_relative_path(relative).map(Some)
}

fn normalize_knowledge_package(
    extracted_root: &Path,
    package_dir: &Path,
    metadata: &InstallMetadata,
) -> Result<()> {
    let manifest_path = metadata
        .distribution
        .manifest_path
        .as_deref()
        .unwrap_or("manifest.yaml");
    let manifest_path = safe_join(extracted_root, Path::new(manifest_path))?;
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: LibraryKnowledgePackManifest =
        if manifest_path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            serde_json::from_str(&manifest_content)
                .with_context(|| format!("failed to parse {}", manifest_path.display()))?
        } else {
            serde_yaml::from_str(&manifest_content)
                .with_context(|| format!("failed to parse {}", manifest_path.display()))?
        };

    let next_dir = package_dir.with_extension("next");
    if next_dir.exists() {
        std::fs::remove_dir_all(&next_dir)
            .with_context(|| format!("failed to remove {}", next_dir.display()))?;
    }
    std::fs::create_dir_all(&next_dir)
        .with_context(|| format!("failed to create {}", next_dir.display()))?;

    for doc in &manifest.docs {
        let relative = validate_relative_path(Path::new(&doc.source_path))?;
        let source = safe_join(extracted_root, &relative)?;
        if !source.exists() {
            warn!(
                source_path = %doc.source_path,
                "Knowledge package manifest referenced missing file"
            );
            continue;
        }
        let target = safe_join(&next_dir, &relative)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::copy(&source, &target).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                target.display()
            )
        })?;
    }

    write_library_knowledge_manifest(&next_dir, &manifest)?;
    let final_manifest = next_dir.join(LIBRARY_KNOWLEDGE_MANIFEST_FILENAME);
    if !final_manifest.exists() {
        bail!("normalized knowledge package is missing manifest");
    }

    if package_dir.exists() {
        std::fs::remove_dir_all(package_dir)
            .with_context(|| format!("failed to replace {}", package_dir.display()))?;
    }
    std::fs::rename(&next_dir, package_dir).with_context(|| {
        format!(
            "failed to move {} to {}",
            next_dir.display(),
            package_dir.display()
        )
    })?;
    Ok(())
}

fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{field} is required"))
}

fn skill_package_dir(
    nenjo_home: &Path,
    ability: &AbilityManifest,
    metadata: &InstallMetadata,
) -> Result<PathBuf> {
    skill_package_dir_from_metadata(nenjo_home, metadata, Some(&ability.name))
}

fn skill_package_dir_from_metadata(
    nenjo_home: &Path,
    metadata: &InstallMetadata,
    fallback_name: Option<&str>,
) -> Result<PathBuf> {
    let owner = metadata.source.owner.as_deref().unwrap_or("local");
    let repo = metadata.source.repo.as_deref().unwrap_or("skills");
    let package = metadata
        .source
        .package
        .as_deref()
        .or_else(|| {
            metadata
                .source
                .path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
        })
        .or(fallback_name)
        .unwrap_or("skill");
    Ok(nenjo_home
        .join("skills")
        .join(validate_relative_path(Path::new(owner))?)
        .join(validate_relative_path(Path::new(repo))?)
        .join(validate_relative_path(Path::new(package))?))
}

fn legacy_skill_package_dir_from_metadata(
    nenjo_home: &Path,
    metadata: &InstallMetadata,
    fallback_name: Option<&str>,
) -> Result<PathBuf> {
    let owner = metadata.source.owner.as_deref().unwrap_or("local");
    let repo = metadata.source.repo.as_deref().unwrap_or("skills");
    let package = metadata
        .source
        .package
        .as_deref()
        .or_else(|| {
            metadata
                .source
                .path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
        })
        .or(fallback_name)
        .unwrap_or("skill");
    Ok(nenjo_home
        .join("skills")
        .join("repos")
        .join("github")
        .join(validate_relative_path(Path::new(owner))?)
        .join(validate_relative_path(Path::new(repo))?)
        .join(validate_relative_path(Path::new(package))?))
}

async fn remove_marked_package_dir(package_dir: &Path, package_type: &str) -> Result<()> {
    if !package_dir.exists() {
        return Ok(());
    }
    if !package_dir.join(INSTALL_MARKER_FILENAME).exists() {
        bail!(
            "refusing to remove unmarked {package_type} package directory {}",
            package_dir.display()
        );
    }
    tokio::fs::remove_dir_all(package_dir)
        .await
        .with_context(|| format!("failed to remove {}", package_dir.display()))?;
    Ok(())
}

fn plugin_package_dir(
    nenjo_home: &Path,
    server: &McpServerManifest,
    metadata: &InstallMetadata,
) -> Result<PathBuf> {
    plugin_package_dir_from_metadata(nenjo_home, metadata, Some(&server.name))
}

fn plugin_package_dir_from_metadata(
    nenjo_home: &Path,
    metadata: &InstallMetadata,
    fallback_name: Option<&str>,
) -> Result<PathBuf> {
    let owner = metadata.source.owner.as_deref().unwrap_or("local");
    let repo = metadata.source.repo.as_deref().unwrap_or("plugins");
    let package = metadata
        .source
        .package
        .as_deref()
        .or_else(|| {
            metadata
                .source
                .path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
        })
        .or(fallback_name)
        .unwrap_or("plugin");
    Ok(nenjo_home
        .join("plugins")
        .join("repos")
        .join("github")
        .join(validate_relative_path(Path::new(owner))?)
        .join(validate_relative_path(Path::new(repo))?)
        .join(validate_relative_path(Path::new(package))?))
}

fn substitute_metadata_plugin_paths(
    value: serde_json::Value,
    plugin_root: &Path,
    nenjo_home: &Path,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(substitute_plugin_paths(&text, plugin_root, nenjo_home))
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| substitute_metadata_plugin_paths(item, plugin_root, nenjo_home))
                .collect(),
        ),
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        substitute_metadata_plugin_paths(value, plugin_root, nenjo_home),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

fn substitute_plugin_paths(value: &str, plugin_root: &Path, nenjo_home: &Path) -> String {
    value
        .replace("${CLAUDE_PLUGIN_ROOT}", &plugin_root.display().to_string())
        .replace(
            "${CLAUDE_PLUGIN_DATA}",
            &nenjo_home
                .join("plugins")
                .join("data")
                .display()
                .to_string(),
        )
}

struct ParsedSkill {
    name: Option<String>,
    description: Option<String>,
    body: String,
}

#[derive(Default, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn parse_skill_markdown(content: &str) -> ParsedSkill {
    let Some(rest) = content.strip_prefix("---\n") else {
        return ParsedSkill {
            name: None,
            description: None,
            body: content.to_string(),
        };
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        return ParsedSkill {
            name: None,
            description: None,
            body: content.to_string(),
        };
    };
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(frontmatter).unwrap_or_default();
    ParsedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        body: body.to_string(),
    }
}

fn copy_dir(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry.context("failed to read directory entry")?;
        let file_type = entry.file_type().context("failed to read file type")?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("failed to copy {}", target.display()))?;
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    Ok(root.join(validate_relative_path(relative)?))
}

fn validate_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("empty package path");
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("package path escapes root: {}", path.display())
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("empty package path");
    }
    Ok(clean)
}
