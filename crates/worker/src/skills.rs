use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use nenjo::hooks::{ActiveHookScope, resolve_skill_hooks};
use nenjo::manifest::HookManifest;
use nenjo::manifest::SkillManifest;
use nenjo::skills::{LoadedSkill, SkillProvider, available_skills_message, skill_matches_selector};
use parking_lot::RwLock;

use crate::external_mcp::ExternalMcpPool;
use crate::tools::SecurityPolicy;

#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: RwLock<Vec<SkillManifest>>,
    hooks: RwLock<Vec<HookManifest>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillToolSet {
    None,
    Activation,
    ActivationWithMcp,
}

impl SkillRegistry {
    pub fn reconcile(&self, skills: &[SkillManifest], hooks: &[HookManifest]) {
        let mut next = skills.to_vec();
        next.sort_by(|left, right| left.name.cmp(&right.name));
        *self.skills.write() = next;

        let mut hooks = hooks.to_vec();
        hooks.sort_by(|left, right| left.name.cmp(&right.name));
        *self.hooks.write() = hooks;
    }

    pub fn is_empty(&self) -> bool {
        self.skills.read().is_empty()
    }

    pub(crate) fn tool_set(&self) -> SkillToolSet {
        let skills = self.skills.read();
        if skills.is_empty() {
            SkillToolSet::None
        } else if skills.iter().any(|skill| !skill.mcp_servers.is_empty()) {
            SkillToolSet::ActivationWithMcp
        } else {
            SkillToolSet::Activation
        }
    }

    pub fn list(&self) -> Vec<SkillManifest> {
        self.skills.read().clone()
    }

    pub fn resolve(&self, selector: &str) -> Option<SkillManifest> {
        self.skills
            .read()
            .iter()
            .find(|skill| skill_matches_selector(skill, selector))
            .cloned()
    }

    pub fn resolve_hooks_for_skill(&self, skill: &SkillManifest) -> Vec<ActiveHookScope> {
        let hooks = resolve_skill_hooks(&self.hooks.read(), skill);
        if hooks.is_empty() {
            Vec::new()
        } else {
            vec![ActiveHookScope::skill(skill, hooks)]
        }
    }
}

pub struct LocalSkillProvider {
    registry: Arc<SkillRegistry>,
    security: Arc<SecurityPolicy>,
    external_mcp: Option<Arc<ExternalMcpPool>>,
}

impl LocalSkillProvider {
    pub fn new(registry: Arc<SkillRegistry>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            registry,
            security,
            external_mcp: None,
        }
    }

    pub fn with_mcp_pool(
        registry: Arc<SkillRegistry>,
        security: Arc<SecurityPolicy>,
        external_mcp: Arc<ExternalMcpPool>,
    ) -> Self {
        Self {
            registry,
            security,
            external_mcp: Some(external_mcp),
        }
    }
}

#[async_trait]
impl SkillProvider for LocalSkillProvider {
    fn list_skills(&self) -> Vec<SkillManifest> {
        self.registry.list()
    }

    fn resolve_skill(&self, selector: &str) -> Option<SkillManifest> {
        self.registry.resolve(selector)
    }

    async fn load_skill(&self, skill: &SkillManifest) -> Result<LoadedSkill> {
        let skill_root = canonical_skill_root(skill)?;
        if !self.security.is_resolved_path_allowed(&skill_root) {
            bail!(
                "Skill root is outside allowed runtime roots: {}",
                skill_root.display()
            );
        }

        let plugin_root = canonical_plugin_root(skill)?;
        if let Some(plugin_root) = &plugin_root
            && !self.security.is_resolved_path_allowed(plugin_root)
        {
            bail!(
                "Skill plugin root is outside allowed runtime roots: {}",
                plugin_root.display()
            );
        }

        let entry_path = safe_skill_entry_path(&skill.entry_path)?;
        let entry_file = skill_root.join(entry_path);
        let content = tokio::fs::read_to_string(&entry_file)
            .await
            .with_context(|| format!("failed to read skill entry {}", entry_file.display()))?;

        let inventory = skill_inventory(&skill_root).await;
        let name = skill.name.clone();
        let context = render_skill_context(
            skill,
            &self.security.workspace_dir,
            &skill_root,
            &content,
            inventory,
        );
        Ok(LoadedSkill {
            activation_env: skill_activation_env(&name, &skill_root, plugin_root.as_deref()),
            mcp_servers: skill.mcp_servers.clone(),
            mcp_tools: match &self.external_mcp {
                Some(external_mcp) => {
                    external_mcp
                        .skill_mcp_tool_inventory(&skill.mcp_servers)
                        .await
                }
                None => Vec::new(),
            },
            hook_scopes: self.registry.resolve_hooks_for_skill(skill),
            name,
            context,
        })
    }

    fn unknown_skill_message(&self) -> String {
        available_skills_message(&self.registry.list())
    }
}

fn skill_activation_env(
    name: &str,
    root_dir: &Path,
    plugin_root_dir: Option<&Path>,
) -> Vec<(String, String)> {
    let root = root_dir.to_string_lossy().into_owned();
    let mut env = vec![
        ("CLAUDE_SKILL_DIR".to_string(), root.clone()),
        ("NENJO_SKILL_DIR".to_string(), root),
        ("NENJO_ACTIVE_SKILLS".to_string(), name.to_string()),
    ];
    if let Some(plugin_root) = plugin_root_dir {
        let plugin_root = plugin_root.to_string_lossy().into_owned();
        env.push(("CLAUDE_PLUGIN_ROOT".to_string(), plugin_root.clone()));
        env.push(("NENJO_PLUGIN_ROOT".to_string(), plugin_root));
    }
    env
}

fn canonical_skill_root(skill: &SkillManifest) -> Result<PathBuf> {
    if skill.root_dir.as_os_str().is_empty() {
        bail!("skill '{}' does not declare root_dir", skill.name);
    }
    Ok(skill
        .root_dir
        .canonicalize()
        .unwrap_or_else(|_| skill.root_dir.clone()))
}

fn canonical_plugin_root(skill: &SkillManifest) -> Result<Option<PathBuf>> {
    let Some(root_dir) = &skill.plugin_root_dir else {
        return Ok(None);
    };
    if root_dir.as_os_str().is_empty() {
        bail!("skill '{}' declares an empty plugin_root_dir", skill.name);
    }
    Ok(Some(
        root_dir.canonicalize().unwrap_or_else(|_| root_dir.clone()),
    ))
}

fn safe_skill_entry_path(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("skill entry_path must be relative and must not contain '..'");
    }
    Ok(path)
}

#[derive(Debug, Default)]
struct SkillInventory {
    scripts: Vec<String>,
    references: Vec<String>,
    assets: Vec<String>,
}

async fn skill_inventory(root: &Path) -> SkillInventory {
    SkillInventory {
        scripts: list_relative_files(root, "scripts").await,
        references: list_relative_files(root, "references").await,
        assets: list_relative_files(root, "assets").await,
    }
}

async fn list_relative_files(root: &Path, dir_name: &str) -> Vec<String> {
    let dir = root.join(dir_name);
    let mut out = BTreeSet::new();
    collect_relative_files(root, &dir, &mut out);
    out.into_iter().collect()
}

fn collect_relative_files(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_relative_files(root, &path, out);
        } else if file_type.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn render_skill_context(
    skill: &SkillManifest,
    workspace: &Path,
    root: &Path,
    skill_md: &str,
    inventory: SkillInventory,
) -> String {
    let root = root.to_string_lossy();
    let workspace = workspace.to_string_lossy();
    let description = skill.description.as_deref().unwrap_or("");
    let plugin_root = skill
        .plugin_root_dir
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let plugin_env = plugin_root
        .as_ref()
        .map(|root| format!("CLAUDE_PLUGIN_ROOT={root}\nNENJO_PLUGIN_ROOT={root}\n"))
        .unwrap_or_default();
    format!(
        "# Skill: {name}\n\
         \n\
         Description: {description}\n\
         CLAUDE_SKILL_DIR={root}\n\
         NENJO_SKILL_DIR={root}\n\
         {plugin_env}\
         WORKSPACE_DIR={workspace}\n\
         \n\
         The agent working directory remains WORKSPACE_DIR. Package skill files are read-only runtime resources. \
         Use $CLAUDE_SKILL_DIR/scripts/... for bundled scripts and $CLAUDE_SKILL_DIR/references/... for bundled references.\n\
         \n\
         Scripts:\n{scripts}\n\
         \n\
         References:\n{references}\n\
         \n\
         Assets:\n{assets}\n\
         \n\
         --- SKILL.md ---\n{skill_md}",
        name = skill.name,
        description = description,
        root = root,
        plugin_env = plugin_env,
        workspace = workspace,
        scripts = render_inventory_list(&inventory.scripts),
        references = render_inventory_list(&inventory.references),
        assets = render_inventory_list(&inventory.assets),
        skill_md = skill_md
    )
}

fn render_inventory_list(items: &[String]) -> String {
    if items.is_empty() {
        return "- none".to_string();
    }
    items
        .iter()
        .map(|item| format!("- $CLAUDE_SKILL_DIR/{item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_derives_tool_set_from_installed_skill_capabilities() {
        let registry = SkillRegistry::default();
        assert_eq!(registry.tool_set(), SkillToolSet::None);

        let plain_skill: SkillManifest = serde_json::from_value(json!({
            "slug": "review",
            "name": "review",
            "root_dir": "/tmp/skills/review"
        }))
        .unwrap();
        registry.reconcile(std::slice::from_ref(&plain_skill), &[]);
        assert_eq!(registry.tool_set(), SkillToolSet::Activation);

        let mcp_skill: SkillManifest = serde_json::from_value(json!({
            "slug": "browser",
            "name": "browser",
            "root_dir": "/tmp/skills/browser",
            "mcp_servers": ["browser-server"]
        }))
        .unwrap();
        registry.reconcile(&[plain_skill, mcp_skill], &[]);
        assert_eq!(registry.tool_set(), SkillToolSet::ActivationWithMcp);
    }

    #[test]
    fn registry_resolves_plugin_skill_aliases() {
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "slug": "acme-review",
            "name": "acme:review",
            "aliases": ["review", "code-review"],
            "root_dir": "/tmp/acme/skills/review"
        }))
        .unwrap();
        let registry = SkillRegistry::default();
        registry.reconcile(&[skill], &[]);

        assert_eq!(registry.resolve("acme:review").unwrap().name, "acme:review");
        assert_eq!(registry.resolve("review").unwrap().name, "acme:review");
        assert_eq!(registry.resolve("code-review").unwrap().name, "acme:review");
    }

    #[test]
    fn skill_activation_env_exports_plugin_roots() {
        let env = skill_activation_env(
            "acme:review",
            Path::new("/tmp/acme/skills/review"),
            Some(Path::new("/tmp/acme")),
        );

        assert!(env.contains(&(
            "CLAUDE_SKILL_DIR".to_string(),
            "/tmp/acme/skills/review".to_string()
        )));
        assert!(env.contains(&("CLAUDE_PLUGIN_ROOT".to_string(), "/tmp/acme".to_string())));
        assert!(env.contains(&("NENJO_ACTIVE_SKILLS".to_string(), "acme:review".to_string())));
    }

    #[test]
    fn registry_resolves_skill_hook_scopes() {
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "slug": "acme-review",
            "name": "Acme Review",
            "root_dir": "/tmp/acme/skills/review",
            "hooks": ["acme-stop-review"]
        }))
        .unwrap();
        let hook: HookManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "slug": "acme-stop-review",
            "name": "Acme Stop Review",
            "event": "Stop",
            "type": "command",
            "command": { "path": "scripts/stop.sh" }
        }))
        .unwrap();
        let registry = SkillRegistry::default();
        registry.reconcile(std::slice::from_ref(&skill), &[hook]);

        let scopes = registry.resolve_hooks_for_skill(&skill);

        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].hooks.len(), 1);
        assert_eq!(scopes[0].source.kind(), "skill");
    }
}
