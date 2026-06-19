use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{PackageError, PackageKind, ResourceManifest, Result, validate_source_path};

/// Parsed `.claude-plugin/plugin.json` metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudePluginManifest {
    /// Safe internal plugin slug used for generated resource names.
    pub slug: String,
    /// Human-authored plugin name or the slug when the manifest omits one.
    pub name: String,
    /// Optional display label from `display_name`, `displayName`, or `title`.
    pub display_name: Option<String>,
    /// Optional plugin version.
    pub version: Option<String>,
    /// Optional plugin description.
    pub description: Option<String>,
    /// Plugin dependencies declared in `.claude-plugin/plugin.json`.
    pub dependencies: Vec<ClaudePluginDependency>,
    /// Original plugin JSON for provenance and UI metadata.
    pub raw: Value,
}

/// Parsed Claude plugin dependency declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudePluginDependency {
    SameMarketplace {
        name: String,
        version: Option<String>,
    },
    CrossMarketplace {
        name: String,
        marketplace: String,
        version: Option<String>,
    },
}

impl ClaudePluginDependency {
    pub fn name(&self) -> &str {
        match self {
            Self::SameMarketplace { name, .. } | Self::CrossMarketplace { name, .. } => name,
        }
    }

    pub fn version(&self) -> Option<&str> {
        match self {
            Self::SameMarketplace { version, .. } | Self::CrossMarketplace { version, .. } => {
                version.as_deref()
            }
        }
    }

    pub fn marketplace(&self) -> Option<&str> {
        match self {
            Self::SameMarketplace { .. } => None,
            Self::CrossMarketplace { marketplace, .. } => Some(marketplace),
        }
    }
}

/// Best-effort parsed Claude marketplace catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudeMarketplaceManifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub plugins: Vec<ClaudeMarketplacePlugin>,
    pub raw: Value,
}

/// One plugin entry from a Claude marketplace catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudeMarketplacePlugin {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub path: Option<String>,
    pub repository: Option<String>,
    pub metadata: Value,
}

/// Parsed `skills/*/SKILL.md` metadata from a Claude plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudePluginSkill {
    /// Safe internal skill slug derived from frontmatter `name` or folder name.
    pub slug: String,
    /// Claude-facing skill name from frontmatter or folder name.
    pub name: String,
    pub description: Option<String>,
    /// Repository/package-relative path to the skill directory.
    pub root_path: String,
    /// Path relative to `root_path`, normally `SKILL.md`.
    pub entry_path: String,
    /// Repository/package-relative path to the `SKILL.md` file.
    pub source_path: String,
    /// Optional hook refs declared by Claude skill frontmatter.
    pub hooks: Vec<String>,
    pub frontmatter: Value,
}

/// Parsed `commands/*.md` metadata from a Claude plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudePluginCommand {
    pub slug: String,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub root_path: String,
    pub entry_path: String,
    pub source_path: String,
    pub frontmatter: Value,
}

/// Parsed hook declaration from a Claude plugin `hooks/hooks.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudePluginHook {
    pub slug: String,
    pub name: String,
    pub event: String,
    pub matcher: Option<String>,
    pub hook_type: String,
    pub command: Option<String>,
    pub raw: Value,
}

/// Parsed MCP server declaration from a plugin `.mcp.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaudePluginMcpServer {
    /// Safe internal server slug derived from the `.mcp.json` key.
    pub slug: String,
    /// Original `.mcp.json` server key.
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub raw: Value,
}

/// Plugin component Nenjo detected but does not execute yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudePluginUnsupportedComponent {
    pub kind: String,
    pub path: String,
    pub reason: String,
}

/// Native resource generated from a Claude plugin component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudePluginResource {
    pub path: String,
    pub source_path: String,
    pub kind: PackageKind,
    pub manifest: ResourceManifest,
}

/// Parse a Claude marketplace catalog containing top-level `plugins`.
pub fn parse_claude_marketplace_manifest(content: &str) -> Result<ClaudeMarketplaceManifest> {
    let raw: Value = serde_json::from_str(content)?;
    let object = raw.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("Claude marketplace manifest must be an object")
    })?;
    let plugins = object
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PackageError::invalid_resource_manifest(
                "Claude marketplace manifest is missing plugins array",
            )
        })?
        .iter()
        .map(parse_marketplace_plugin)
        .collect::<Result<Vec<_>>>()?;
    Ok(ClaudeMarketplaceManifest {
        name: string_field(object, &["name", "title"]),
        description: string_field(object, &["description"]),
        plugins,
        raw,
    })
}

/// Parse `.claude-plugin/plugin.json`.
pub fn parse_claude_plugin_manifest(content: &str) -> Result<ClaudePluginManifest> {
    let raw: Value = serde_json::from_str(content)?;
    let object = raw.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest(".claude-plugin/plugin.json must be an object")
    })?;
    let name = string_field(object, &["name", "title", "id"]).unwrap_or_else(|| "plugin".into());
    let slug = string_field(object, &["slug", "id"])
        .map(|value| safe_identifier(&value, "plugin"))
        .unwrap_or_else(|| safe_identifier(&name, "plugin"));
    Ok(ClaudePluginManifest {
        slug,
        name,
        display_name: string_field(object, &["display_name", "displayName", "title"]),
        version: string_field(object, &["version"]),
        description: string_field(object, &["description"]),
        dependencies: parse_claude_plugin_dependencies(object.get("dependencies"))?,
        raw,
    })
}

/// Parse Claude plugin dependencies from `.claude-plugin/plugin.json`.
pub fn parse_claude_plugin_dependencies(
    value: Option<&Value>,
) -> Result<Vec<ClaudePluginDependency>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        PackageError::invalid_resource_manifest("plugin dependencies must be an array")
    })?;
    items
        .iter()
        .map(parse_claude_plugin_dependency)
        .collect::<Result<Vec<_>>>()
}

/// Parse a Claude plugin skill file. Unlike native package skills, plugin skills
/// may omit frontmatter `name`; the containing directory then becomes the name.
pub fn parse_claude_plugin_skill(content: &str, source_path: &str) -> Result<ClaudePluginSkill> {
    let source_path = validate_source_path(source_path)?;
    let (root_path, entry_path) = skill_paths(&source_path)?;
    let fallback_name = root_path
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("skill")
        .to_string();
    let frontmatter = skill_frontmatter(content)?
        .map(serde_yaml::from_str::<Value>)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let object = frontmatter.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("skill frontmatter must be an object")
    })?;
    let name = string_field(object, &["name"]).unwrap_or(fallback_name);
    Ok(ClaudePluginSkill {
        slug: safe_identifier(&name, "skill"),
        name,
        description: string_field(object, &["description"]),
        root_path,
        entry_path,
        source_path,
        hooks: string_array_field(object, &["hooks"]),
        frontmatter,
    })
}

/// Parse a Claude plugin command markdown file.
pub fn parse_claude_plugin_command(
    content: &str,
    source_path: &str,
) -> Result<ClaudePluginCommand> {
    let source_path = validate_source_path(source_path)?;
    let (root_path, entry_path) = command_paths(&source_path)?;
    let fallback_name = entry_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&entry_path)
        .to_string();
    let frontmatter = skill_frontmatter(content)?
        .map(serde_yaml::from_str::<Value>)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let object = frontmatter.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("command frontmatter must be an object")
    })?;
    let name = string_field(object, &["name"]).unwrap_or(fallback_name);
    let slug = safe_identifier(&name, "command");
    Ok(ClaudePluginCommand {
        command: format!("/{name}"),
        slug,
        name,
        description: string_field(object, &["description"]),
        argument_hint: string_field(object, &["argument-hint", "argument_hint"]),
        root_path,
        entry_path,
        source_path,
        frontmatter,
    })
}

/// Parse Claude plugin hooks from `hooks/hooks.json`.
pub fn parse_claude_plugin_hooks(content: &str) -> Result<Vec<ClaudePluginHook>> {
    let raw: Value = serde_json::from_str(content)?;
    let object = raw.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("hooks/hooks.json must be an object")
    })?;
    let hooks_by_event = object
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PackageError::invalid_resource_manifest("hooks/hooks.json is missing hooks object")
        })?;

    let mut hooks = Vec::new();
    for (event, entries) in hooks_by_event {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for (entry_index, entry) in entries.iter().enumerate() {
            let matcher = entry
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(inner_hooks) = entry.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for (hook_index, hook) in inner_hooks.iter().enumerate() {
                let Some(hook_object) = hook.as_object() else {
                    continue;
                };
                let hook_type =
                    string_field(hook_object, &["type"]).unwrap_or_else(|| "command".to_string());
                let command = string_field(hook_object, &["command"]);
                let name = hook_name(event, entry_index, hook_index, command.as_deref());
                hooks.push(ClaudePluginHook {
                    slug: safe_identifier(&name, "hook"),
                    name,
                    event: event.clone(),
                    matcher: matcher.clone(),
                    hook_type,
                    command,
                    raw: hook.clone(),
                });
            }
        }
    }
    hooks.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(hooks)
}

/// Parse a plugin `.mcp.json` file using the common `mcpServers` shape.
pub fn parse_claude_plugin_mcp_servers(content: &str) -> Result<Vec<ClaudePluginMcpServer>> {
    let raw: Value = serde_json::from_str(content)?;
    let object = raw
        .as_object()
        .ok_or_else(|| PackageError::invalid_resource_manifest(".mcp.json must be an object"))?;
    let servers = object
        .get("mcpServers")
        .or_else(|| object.get("mcp_servers"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PackageError::invalid_resource_manifest(".mcp.json is missing mcpServers object")
        })?;

    servers
        .iter()
        .map(|(name, value)| parse_mcp_server(name, value))
        .collect()
}

/// Detect plugin components Nenjo does not execute yet from a list of
/// repository-relative paths.
pub fn detect_unsupported_claude_plugin_components<I, S>(
    paths: I,
) -> Vec<ClaudePluginUnsupportedComponent>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = BTreeMap::<(String, String), ClaudePluginUnsupportedComponent>::new();
    for raw_path in paths {
        let path = raw_path.as_ref().trim().trim_start_matches("./");
        if path.is_empty() {
            continue;
        }
        if let Some(kind) = unsupported_component_kind(path) {
            let path = path.trim_end_matches('/').to_string();
            out.entry((kind.to_string(), path.clone()))
                .or_insert_with(|| ClaudePluginUnsupportedComponent {
                    kind: kind.to_string(),
                    path,
                    reason: format!("{kind} components are detected but not executed by Nenjo yet"),
                });
        }
    }
    out.into_values().collect()
}

/// Generate a native plugin resource manifest.
pub fn claude_plugin_resource_manifest(
    plugin: &ClaudePluginManifest,
    unsupported: &[ClaudePluginUnsupportedComponent],
) -> ResourceManifest {
    ResourceManifest {
        schema: "nenjo.plugin.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: Some(format!("claude-plugin:{}", plugin.slug)),
        imports: BTreeMap::new(),
        manifest: json!({
            "name": plugin.slug,
            "display_name": plugin.display_name.as_deref().unwrap_or(&plugin.name),
            "description": plugin.description,
            "version": plugin.version,
            "dependencies": plugin.dependencies,
            "unsupported_components": unsupported,
            "metadata": {
                "claude": {
                    "plugin": plugin.raw
                }
            }
        }),
    }
}

/// Generate a native skill resource manifest for one plugin skill.
pub fn claude_skill_resource_manifest(
    plugin: &ClaudePluginManifest,
    skill: &ClaudePluginSkill,
    hooks: &[ClaudePluginHook],
    plugin_root_path: &str,
) -> Result<ResourceManifest> {
    let plugin_root_path = normalize_plugin_root_path(plugin_root_path)?;
    let internal_name = format!("{}__{}", plugin.slug, skill.slug);
    let display_name = format!("{}:{}", plugin.slug, skill.slug);
    let hook_refs = skill_hook_refs(plugin, skill, hooks);
    Ok(ResourceManifest {
        schema: "nenjo.skill.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: Some(format!(
            "claude-plugin:{}:skill:{}",
            plugin.slug, skill.slug
        )),
        imports: BTreeMap::new(),
        manifest: json!({
            "name": internal_name,
            "display_name": display_name,
            "aliases": skill_aliases(plugin, skill),
            "description": skill.description,
            "entry_path": skill.entry_path,
            "root_path": skill.root_path,
            "plugin_root_path": plugin_root_path,
            "hooks": hook_refs,
            "metadata": {
                "claude": {
                    "plugin": {
                        "slug": plugin.slug,
                        "name": plugin.name,
                        "display_name": plugin.display_name,
                        "version": plugin.version
                    },
                    "skill": {
                        "name": skill.name,
                        "slug": skill.slug,
                        "source_path": skill.source_path,
                        "frontmatter": skill.frontmatter
                    }
                }
            }
        }),
    })
}

/// Generate a native command resource manifest for one plugin command.
pub fn claude_command_resource_manifest(
    plugin: &ClaudePluginManifest,
    command: &ClaudePluginCommand,
    hooks: &[ClaudePluginHook],
    plugin_root_path: &str,
) -> Result<ResourceManifest> {
    let plugin_root_path = normalize_plugin_root_path(plugin_root_path)?;
    let internal_name = format!("{}__{}", plugin.slug, command.slug);
    let hook_refs = hooks
        .iter()
        .map(|hook| format!("{}__{}", plugin.slug, hook.slug))
        .collect::<Vec<_>>();
    Ok(ResourceManifest {
        schema: "nenjo.command.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: Some(format!(
            "claude-plugin:{}:command:{}",
            plugin.slug, command.slug
        )),
        imports: BTreeMap::new(),
        manifest: json!({
            "name": internal_name,
            "path": format!("plugins/{}", plugin.slug.replace('-', "_")),
            "display_name": format!("{}:{}", plugin.slug, command.slug),
            "command": command.command,
            "description": command.description,
            "entry_path": command.entry_path,
            "root_path": command.root_path,
            "plugin_root_path": plugin_root_path,
            "hooks": hook_refs,
            "metadata": {
                "claude": {
                    "plugin": {
                        "slug": plugin.slug,
                        "name": plugin.name,
                        "display_name": plugin.display_name,
                        "version": plugin.version
                    },
                    "command": {
                        "name": command.name,
                        "slug": command.slug,
                        "source_path": command.source_path,
                        "argument_hint": command.argument_hint,
                        "frontmatter": command.frontmatter
                    }
                }
            }
        }),
    })
}

/// Generate a native hook resource manifest for one plugin hook.
pub fn claude_hook_resource_manifest(
    plugin: &ClaudePluginManifest,
    hook: &ClaudePluginHook,
    plugin_root_path: &str,
) -> Result<ResourceManifest> {
    let plugin_root_path = normalize_plugin_root_path(plugin_root_path)?;
    let internal_name = format!("{}__{}", plugin.slug, hook.slug);
    Ok(ResourceManifest {
        schema: "nenjo.hook.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: Some(format!("claude-plugin:{}:hook:{}", plugin.slug, hook.slug)),
        imports: BTreeMap::new(),
        manifest: json!({
            "name": internal_name,
            "display_name": format!("{}:{}", plugin.slug, hook.slug),
            "event": hook.event,
            "matcher": hook.matcher,
            "type": hook.hook_type,
            "command": hook.command.as_ref().map(|command| json!({ "path": command })),
            "plugin_root_path": plugin_root_path,
            "metadata": {
                "claude": {
                    "plugin": {
                        "slug": plugin.slug,
                        "name": plugin.name,
                        "display_name": plugin.display_name,
                        "version": plugin.version
                    },
                    "hook": {
                        "name": hook.name,
                        "slug": hook.slug,
                        "raw": hook.raw
                    }
                }
            }
        }),
    })
}

/// Generate a native MCP server resource manifest for one plugin MCP server.
pub fn claude_mcp_server_resource_manifest(
    plugin: &ClaudePluginManifest,
    server: &ClaudePluginMcpServer,
    plugin_root_path: &str,
) -> Result<ResourceManifest> {
    let plugin_root_path = normalize_plugin_root_path(plugin_root_path)?;
    let internal_name = format!("{}__{}", plugin.slug, server.slug);
    let display_name = format!("{}:{}", plugin.slug, server.slug);
    let (env_schema, runtime_env) = mcp_env_fields(&server.env);
    Ok(ResourceManifest {
        schema: "nenjo.mcp_server.v1".to_string(),
        slug: None,
        root_uri: None,
        selector: Some(format!("claude-plugin:{}:mcp:{}", plugin.slug, server.slug)),
        imports: BTreeMap::new(),
        manifest: json!({
            "name": internal_name,
            "display_name": display_name,
            "description": plugin.description,
            "transport": server.transport,
            "command": server.command,
            "args": server.args,
            "url": server.url,
            "env_schema": env_schema,
            "metadata": {
                "runtime": {
                    "cwd_path": plugin_root_path,
                    "env": runtime_env
                },
                "claude": {
                    "plugin": {
                        "slug": plugin.slug,
                        "name": plugin.name,
                        "display_name": plugin.display_name,
                        "version": plugin.version
                    },
                    "mcp": {
                        "name": server.name,
                        "slug": server.slug,
                        "raw": server.raw
                    }
                }
            }
        }),
    })
}

/// Generate all native resources for parsed plugin components.
pub fn claude_plugin_resources(
    plugin: &ClaudePluginManifest,
    skills: &[ClaudePluginSkill],
    commands: &[ClaudePluginCommand],
    hooks: &[ClaudePluginHook],
    mcp_servers: &[ClaudePluginMcpServer],
    unsupported: &[ClaudePluginUnsupportedComponent],
    plugin_root_path: &str,
) -> Result<Vec<ClaudePluginResource>> {
    let mut resources = vec![ClaudePluginResource {
        path: ".nenjo/generated/claude-plugin/plugin.yaml".to_string(),
        source_path: ".nenjo/generated/claude-plugin/plugin.yaml".to_string(),
        kind: PackageKind::Plugin,
        manifest: claude_plugin_resource_manifest(plugin, unsupported),
    }];
    for skill in skills {
        let path = format!(".nenjo/generated/claude-plugin/skills/{}.yaml", skill.slug);
        resources.push(ClaudePluginResource {
            path: path.clone(),
            source_path: path,
            kind: PackageKind::Skill,
            manifest: claude_skill_resource_manifest(plugin, skill, hooks, plugin_root_path)?,
        });
    }
    for command in commands {
        let path = format!(
            ".nenjo/generated/claude-plugin/commands/{}.yaml",
            command.slug
        );
        resources.push(ClaudePluginResource {
            path: path.clone(),
            source_path: path,
            kind: PackageKind::Command,
            manifest: claude_command_resource_manifest(plugin, command, hooks, plugin_root_path)?,
        });
    }
    for hook in hooks {
        let path = format!(".nenjo/generated/claude-plugin/hooks/{}.yaml", hook.slug);
        resources.push(ClaudePluginResource {
            path: path.clone(),
            source_path: path,
            kind: PackageKind::Hook,
            manifest: claude_hook_resource_manifest(plugin, hook, plugin_root_path)?,
        });
    }
    for server in mcp_servers {
        let path = format!(".nenjo/generated/claude-plugin/mcp/{}.yaml", server.slug);
        resources.push(ClaudePluginResource {
            path: path.clone(),
            source_path: path,
            kind: PackageKind::McpServer,
            manifest: claude_mcp_server_resource_manifest(plugin, server, plugin_root_path)?,
        });
    }
    Ok(resources)
}

fn parse_marketplace_plugin(value: &Value) -> Result<ClaudeMarketplacePlugin> {
    let object = value.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("Claude marketplace plugin must be an object")
    })?;
    let name = string_field(object, &["name", "title", "id"]).unwrap_or_else(|| "plugin".into());
    let slug = string_field(object, &["slug", "id"])
        .map(|value| safe_identifier(&value, "plugin"))
        .unwrap_or_else(|| safe_identifier(&name, "plugin"));
    Ok(ClaudeMarketplacePlugin {
        slug,
        name,
        description: string_field(object, &["description"]),
        path: string_field(
            object,
            &["path", "plugin_path", "root_path", "manifest_path"],
        ),
        repository: string_field(object, &["repository", "repo", "url"]),
        metadata: value.clone(),
    })
}

fn parse_claude_plugin_dependency(value: &Value) -> Result<ClaudePluginDependency> {
    if let Some(name) = value.as_str() {
        let name = normalized_dependency_name(name)?;
        return Ok(ClaudePluginDependency::SameMarketplace {
            name,
            version: None,
        });
    }

    let object = value.as_object().ok_or_else(|| {
        PackageError::invalid_resource_manifest("plugin dependency must be a string or an object")
    })?;
    let name = string_field(object, &["name"]).ok_or_else(|| {
        PackageError::invalid_resource_manifest("plugin dependency object requires name")
    })?;
    let name = normalized_dependency_name(&name)?;
    let version = string_field(object, &["version"]);
    match string_field(object, &["marketplace"]) {
        Some(marketplace) => Ok(ClaudePluginDependency::CrossMarketplace {
            name,
            marketplace,
            version,
        }),
        None => Ok(ClaudePluginDependency::SameMarketplace { name, version }),
    }
}

fn normalized_dependency_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(PackageError::invalid_resource_manifest(
            "plugin dependency name cannot be empty",
        ));
    }
    Ok(value.to_string())
}

fn parse_mcp_server(name: &str, value: &Value) -> Result<ClaudePluginMcpServer> {
    let object = value
        .as_object()
        .ok_or_else(|| PackageError::invalid_resource_manifest("MCP server must be an object"))?;
    let command = string_field(object, &["command"]);
    let url = string_field(object, &["url"]);
    let transport = string_field(object, &["transport", "type"]).unwrap_or_else(|| {
        if url.is_some() {
            "http".to_string()
        } else {
            "stdio".to_string()
        }
    });
    let args = object.get("args").and_then(|args| {
        args.as_array().map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
    });
    let env = object
        .get("env")
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    Ok(ClaudePluginMcpServer {
        slug: safe_identifier(name, "mcp"),
        name: name.to_string(),
        transport,
        command,
        args,
        url,
        env,
        raw: value.clone(),
    })
}

fn skill_paths(source_path: &str) -> Result<(String, String)> {
    let Some((root_path, entry_path)) = source_path.rsplit_once('/') else {
        if source_path == "SKILL.md" {
            return Ok((".".to_string(), "SKILL.md".to_string()));
        }
        return Err(PackageError::invalid_path(
            source_path,
            "Claude skill path must point to SKILL.md",
        ));
    };
    if entry_path != "SKILL.md" {
        return Err(PackageError::invalid_path(
            source_path,
            "Claude skill path must point to SKILL.md",
        ));
    }
    Ok((root_path.to_string(), entry_path.to_string()))
}

fn command_paths(source_path: &str) -> Result<(String, String)> {
    let Some((root_path, entry_path)) = source_path.rsplit_once('/') else {
        return Err(PackageError::invalid_path(
            source_path,
            "Claude command path must point to commands/*.md",
        ));
    };
    if !entry_path.ends_with(".md") {
        return Err(PackageError::invalid_path(
            source_path,
            "Claude command path must point to a Markdown file",
        ));
    }
    Ok((root_path.to_string(), entry_path.to_string()))
}

fn hook_name(event: &str, entry_index: usize, hook_index: usize, command: Option<&str>) -> String {
    command
        .and_then(|command| {
            command
                .split('/')
                .next_back()
                .map(|value| value.trim_matches('"').trim_matches('\''))
                .filter(|value| !value.is_empty())
        })
        .map(|value| value.trim_end_matches(".sh").trim_end_matches(".py"))
        .map(|value| format!("{}_{}", event, value))
        .unwrap_or_else(|| format!("{}_{}_{}", event, entry_index, hook_index))
}

fn skill_frontmatter(content: &str) -> Result<Option<&str>> {
    let Some(rest) = content.strip_prefix("---") else {
        return Ok(None);
    };
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest);
    let Some((frontmatter, _body)) = rest.split_once("\n---") else {
        return Err(PackageError::invalid_resource_manifest(
            "skill frontmatter is missing closing marker",
        ));
    };
    Ok(Some(frontmatter.trim()))
}

fn skill_aliases(plugin: &ClaudePluginManifest, skill: &ClaudePluginSkill) -> Vec<String> {
    let mut aliases = BTreeSet::new();
    aliases.insert(skill.name.clone());
    aliases.insert(skill.slug.clone());
    aliases.insert(format!("{}:{}", plugin.slug, skill.slug));
    aliases.into_iter().collect()
}

fn skill_hook_refs(
    plugin: &ClaudePluginManifest,
    skill: &ClaudePluginSkill,
    hooks: &[ClaudePluginHook],
) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for hook_ref in &skill.hooks {
        let normalized_ref = safe_identifier(hook_ref, "hook");
        let hook_slug = hooks
            .iter()
            .find(|hook| {
                hook.name == *hook_ref
                    || hook.slug == hook_ref.as_str()
                    || hook.slug == normalized_ref.as_str()
            })
            .map(|hook| hook.slug.as_str())
            .unwrap_or(normalized_ref.as_str());
        refs.insert(format!("{}__{}", plugin.slug, hook_slug));
    }
    refs.into_iter().collect()
}

fn normalize_plugin_root_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path == "." {
        return Ok(".".to_string());
    }
    validate_source_path(path)
}

fn mcp_env_fields(env: &BTreeMap<String, String>) -> (Value, Value) {
    let mut schema = Vec::new();
    let mut runtime_env = Map::new();
    for (key, value) in env {
        schema.push(json!({
            "key": key,
            "label": key,
            "required": false
        }));
        if !value.contains('$') {
            runtime_env.insert(key.clone(), Value::String(value.clone()));
        }
    }
    (Value::Array(schema), Value::Object(runtime_env))
}

fn unsupported_component_kind(path: &str) -> Option<&'static str> {
    let normalized = path.trim_start_matches("./");
    let first = normalized.split('/').next().unwrap_or(normalized);
    match first {
        "agents" => Some("agents"),
        "monitors" => Some("monitors"),
        "bin" => Some("bin"),
        ".lsp.json" | "lsp" => Some("lsp"),
        "settings.json" => Some("settings"),
        _ => None,
    }
}

fn string_field(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_array_field(object: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(|value| match value {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ),
            Value::String(value) => Some(vec![value.clone()]),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

fn safe_identifier(input: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_was_separator = false;
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if matches!(ch, '-' | '_' | '.' | '/' | ' ' | ':') && !last_was_separator {
            out.push('_');
            last_was_separator = true;
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plugin_dependencies_from_string_and_object_entries() {
        let plugin = parse_claude_plugin_manifest(
            r#"{
              "name": "Deploy Kit",
              "dependencies": [
                "audit-logger",
                { "name": "secrets-vault", "version": "~2.1.0" },
                { "name": "shared-kit", "marketplace": "acme-shared", "version": "^1.0" }
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            plugin.dependencies,
            vec![
                ClaudePluginDependency::SameMarketplace {
                    name: "audit-logger".to_string(),
                    version: None,
                },
                ClaudePluginDependency::SameMarketplace {
                    name: "secrets-vault".to_string(),
                    version: Some("~2.1.0".to_string()),
                },
                ClaudePluginDependency::CrossMarketplace {
                    name: "shared-kit".to_string(),
                    marketplace: "acme-shared".to_string(),
                    version: Some("^1.0".to_string()),
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_plugin_dependency_entries() {
        parse_claude_plugin_manifest(r#"{"name":"Bad","dependencies":{}}"#)
            .expect_err("dependencies must be an array");
        parse_claude_plugin_manifest(r#"{"name":"Bad","dependencies":[""]}"#)
            .expect_err("empty dependency names are rejected");
        parse_claude_plugin_manifest(r#"{"name":"Bad","dependencies":[{"version":"^1"}]}"#)
            .expect_err("object dependency requires name");
    }

    #[test]
    fn generated_plugin_resource_includes_dependencies() {
        let plugin = parse_claude_plugin_manifest(
            r#"{
              "name": "Deploy Kit",
              "dependencies": [
                "audit-logger",
                { "name": "secrets-vault", "version": "~2.1.0" }
              ]
            }"#,
        )
        .unwrap();

        let manifest = claude_plugin_resource_manifest(&plugin, &[]);

        assert_eq!(
            manifest.manifest["dependencies"],
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
                }
            ])
        );
    }

    #[test]
    fn parses_plugin_skill_without_frontmatter_name() {
        let skill = parse_claude_plugin_skill(
            r#"---
description: Review code changes.
---
Use scripts/review.sh.
"#,
            "skills/review/SKILL.md",
        )
        .unwrap();

        assert_eq!(skill.name, "review");
        assert_eq!(skill.slug, "review");
        assert_eq!(skill.root_path, "skills/review");
        assert_eq!(skill.description.as_deref(), Some("Review code changes."));
    }

    #[test]
    fn generates_native_skill_resource_with_aliases() {
        let plugin = parse_claude_plugin_manifest(
            r#"{"name":"Acme Tools","version":"1.2.3","description":"Useful tools"}"#,
        )
        .unwrap();
        let skill = parse_claude_plugin_skill(
            r#"---
name: Code Review
description: Review code.
---
Use $CLAUDE_SKILL_DIR/scripts/review.sh.
"#,
            "skills/review/SKILL.md",
        )
        .unwrap();

        let manifest = claude_skill_resource_manifest(&plugin, &skill, &[], ".").unwrap();

        assert_eq!(manifest.schema, "nenjo.skill.v1");
        assert_eq!(manifest.manifest["name"], "acme_tools__code_review");
        assert_eq!(manifest.manifest["display_name"], "acme_tools:code_review");
        assert_eq!(manifest.manifest["root_path"], "skills/review");
        assert_eq!(manifest.manifest["plugin_root_path"], ".");
        assert!(
            manifest.manifest["aliases"]
                .as_array()
                .unwrap()
                .contains(&json!("Code Review"))
        );
    }

    #[test]
    fn generates_native_skill_resource_with_hook_refs() {
        let plugin = parse_claude_plugin_manifest(r#"{"name":"Acme Tools"}"#).unwrap();
        let skill = parse_claude_plugin_skill(
            r#"---
name: Code Review
hooks:
  - Stop review-stop
---
Use $CLAUDE_SKILL_DIR/scripts/review.sh.
"#,
            "skills/review/SKILL.md",
        )
        .unwrap();
        let hooks = vec![ClaudePluginHook {
            slug: "stop_review_stop".to_string(),
            name: "Stop review-stop".to_string(),
            event: "Stop".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some("scripts/review-stop.sh".to_string()),
            raw: json!({ "type": "command", "command": "scripts/review-stop.sh" }),
        }];

        let manifest = claude_skill_resource_manifest(&plugin, &skill, &hooks, ".").unwrap();

        assert_eq!(skill.hooks, vec!["Stop review-stop"]);
        assert_eq!(
            manifest.manifest["hooks"][0],
            "acme_tools__stop_review_stop"
        );
    }

    #[test]
    fn parses_mcp_json_and_generates_native_server() {
        let plugin = parse_claude_plugin_manifest(r#"{"name":"Acme"}"#).unwrap();
        let servers = parse_claude_plugin_mcp_servers(
            r#"{
              "mcpServers": {
                "review-server": {
                  "command": "node",
                  "args": ["servers/review.js"],
                  "env": {"MODE":"local","TOKEN":"$TOKEN"}
                }
              }
            }"#,
        )
        .unwrap();

        let manifest = claude_mcp_server_resource_manifest(&plugin, &servers[0], ".").unwrap();

        assert_eq!(manifest.schema, "nenjo.mcp_server.v1");
        assert_eq!(manifest.manifest["name"], "acme__review_server");
        assert_eq!(manifest.manifest["transport"], "stdio");
        assert_eq!(manifest.manifest["metadata"]["runtime"]["cwd_path"], ".");
        assert_eq!(
            manifest.manifest["metadata"]["runtime"]["env"]["MODE"],
            "local"
        );
        assert!(manifest.manifest["metadata"]["runtime"]["env"]["TOKEN"].is_null());
        assert_eq!(manifest.manifest["env_schema"][1]["key"], "TOKEN");
    }

    #[test]
    fn detects_unsupported_components() {
        let unsupported = detect_unsupported_claude_plugin_components([
            "commands/summarize.md",
            "hooks/hooks.json",
            "agents/reviewer.md",
            "lsp/server.json",
            "skills/review/SKILL.md",
        ]);
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported.iter().any(|item| item.kind == "agents"));
        assert!(unsupported.iter().any(|item| item.kind == "lsp"));
    }

    #[test]
    fn parses_command_markdown_and_generates_native_command() {
        let plugin = parse_claude_plugin_manifest(r#"{"name":"Ralph Loop"}"#).unwrap();
        let command = parse_claude_plugin_command(
            r#"---
description: Run the loop.
argument-hint: TASK
---
Run scripts/ralph-loop.sh.
"#,
            "commands/ralph-loop.md",
        )
        .unwrap();
        let hooks = parse_claude_plugin_hooks(
            r#"{
              "hooks": {
                "Stop": [
                  {
                    "matcher": "*",
                    "hooks": [
                      { "type": "command", "command": "scripts/ralph-loop-stop.sh" }
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        let manifest = claude_command_resource_manifest(&plugin, &command, &hooks, ".").unwrap();

        assert_eq!(command.name, "ralph-loop");
        assert_eq!(manifest.schema, "nenjo.command.v1");
        assert_eq!(manifest.manifest["name"], "ralph_loop__ralph_loop");
        assert_eq!(manifest.manifest["path"], "plugins/ralph_loop");
        assert_eq!(manifest.manifest["command"], "/ralph-loop");
        assert_eq!(manifest.manifest["entry_path"], "ralph-loop.md");
        assert_eq!(manifest.manifest["root_path"], "commands");
        assert_eq!(
            manifest.manifest["hooks"][0],
            "ralph_loop__stop_ralph_loop_stop"
        );
    }

    #[test]
    fn parses_hooks_json_and_generates_native_hook() {
        let plugin = parse_claude_plugin_manifest(r#"{"name":"Ralph Loop"}"#).unwrap();
        let hooks = parse_claude_plugin_hooks(
            r#"{
              "hooks": {
                "Stop": [
                  {
                    "matcher": "*",
                    "hooks": [
                      { "type": "command", "command": "scripts/ralph-loop-stop.sh" }
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        let manifest = claude_hook_resource_manifest(&plugin, &hooks[0], ".").unwrap();

        assert_eq!(hooks[0].slug, "stop_ralph_loop_stop");
        assert_eq!(manifest.schema, "nenjo.hook.v1");
        assert_eq!(
            manifest.manifest["name"],
            "ralph_loop__stop_ralph_loop_stop"
        );
        assert_eq!(manifest.manifest["event"], "Stop");
        assert_eq!(manifest.manifest["matcher"], "*");
        assert_eq!(
            manifest.manifest["command"]["path"],
            "scripts/ralph-loop-stop.sh"
        );
    }
}
