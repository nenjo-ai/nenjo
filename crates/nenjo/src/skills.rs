//! Skill activation tools and filesystem-agnostic skill provider contracts.
//!
//! Core owns the model-facing tools and activation state. Concrete runtimes
//! provide skill content through [`SkillProvider`], which keeps package cache
//! layout, filesystem access, and path policy outside this crate.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use crate::Slug;
use crate::agents::runner::turn_loop;
use crate::hooks::ActiveHookScope;
use crate::manifest::SkillManifest;
use crate::tools::{Tool, ToolCategory, ToolOrigin, ToolResult};

pub const LIST_INSTALLED_SKILLS_TOOL_NAME: &str = "list_installed_skills";
pub const USE_SKILL_TOOL_NAME: &str = "use_skill";
pub const CALL_SKILL_MCP_TOOL_NAME: &str = "call_skill_mcp_tool";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSkill {
    pub name: String,
    pub env: Vec<(String, String)>,
    pub mcp_servers: Vec<Slug>,
}

#[derive(Debug, Default)]
pub struct SkillRuntimeState {
    active_skill: Mutex<Option<ActiveSkill>>,
}

impl SkillRuntimeState {
    pub fn activate(&self, skill: ActiveSkill) {
        *mutex_lock(&self.active_skill) = Some(skill);
    }

    pub fn clear(&self) {
        *mutex_lock(&self.active_skill) = None;
    }

    pub fn active_skill(&self) -> Option<ActiveSkill> {
        mutex_lock(&self.active_skill).clone()
    }

    pub fn shell_env(&self) -> Vec<(String, String)> {
        self.active_skill()
            .map(|skill| skill.env)
            .unwrap_or_default()
    }

    pub fn active_mcp_servers(&self) -> Vec<Slug> {
        self.active_skill()
            .map(|skill| skill.mcp_servers)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct SkillMcpToolInfo {
    pub server: Slug,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub context: String,
    pub activation_env: Vec<(String, String)>,
    pub mcp_servers: Vec<Slug>,
    pub mcp_tools: Vec<SkillMcpToolInfo>,
    pub hook_scopes: Vec<ActiveHookScope>,
}

#[async_trait]
pub trait SkillProvider: Send + Sync {
    fn list_skills(&self) -> Vec<SkillManifest>;

    fn resolve_skill(&self, selector: &str) -> Option<SkillManifest>;

    async fn load_skill(&self, skill: &SkillManifest) -> Result<LoadedSkill>;

    fn unknown_skill_message(&self) -> String {
        available_skills_message(&self.list_skills())
    }
}

pub struct UseSkillTool {
    provider: Arc<dyn SkillProvider>,
    runtime_state: Arc<SkillRuntimeState>,
}

impl UseSkillTool {
    pub fn new(provider: Arc<dyn SkillProvider>, runtime_state: Arc<SkillRuntimeState>) -> Self {
        Self {
            provider,
            runtime_state,
        }
    }
}

#[async_trait]
impl Tool for UseSkillTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    fn name(&self) -> &str {
        USE_SKILL_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Activate an installed skill. This reads the skill's instructions into context and marks the skill active for this agent run. Use list_installed_skills to see available skill names and aliases before activating one. If the skill exposes MCP tools, call them with call_skill_mcp_tool after activation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Installed skill name, slug, alias, or id to activate"
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let name = args
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

        let Some(skill) = self.provider.resolve_skill(name) else {
            return Ok(ToolResult {
                success: false,
                output: self.provider.unknown_skill_message(),
                error: Some(format!("Unknown skill: {name}")),
            });
        };

        let loaded = match self.provider.load_skill(&skill).await {
            Ok(loaded) => loaded,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };

        self.runtime_state.activate(ActiveSkill {
            name: loaded.name.clone(),
            env: loaded.activation_env.clone(),
            mcp_servers: loaded.mcp_servers.clone(),
        });
        activate_skill_hooks(&loaded.hook_scopes);

        Ok(ToolResult {
            success: true,
            output: render_loaded_skill_context(&loaded),
            error: None,
        })
    }
}

fn render_loaded_skill_context(loaded: &LoadedSkill) -> String {
    if loaded.mcp_tools.is_empty() {
        return loaded.context.clone();
    }

    let mut out = loaded.context.trim_end().to_string();
    out.push_str("\n\n--- ACTIVE SKILL MCP TOOLS ---\n");
    out.push_str(&format!(
        "Use `{CALL_SKILL_MCP_TOOL_NAME}` to call these MCP tools while this skill is active.\n"
    ));
    for tool in &loaded.mcp_tools {
        out.push_str(&format!(
            "- server: `{}`, tool: `{}`",
            tool.server.as_str(),
            tool.name
        ));
        if let Some(description) = tool
            .description
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            out.push_str(&format!(" - {description}"));
        }
        out.push('\n');
        let schema = serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".into());
        out.push_str(&format!("  arguments_schema: `{schema}`\n"));
    }
    out.push_str("--- END ACTIVE SKILL MCP TOOLS ---");
    out
}

fn activate_skill_hooks(hook_scopes: &[ActiveHookScope]) {
    if hook_scopes.is_empty() {
        return;
    }
    let events_tx = turn_loop::current_events_tx();
    for scope in hook_scopes {
        let activated = turn_loop::activate_current_hook_scope(scope.clone());
        if !activated {
            continue;
        }
        for hook in &scope.hooks {
            if let Some(tx) = &events_tx {
                let _ = tx.send(crate::TurnEvent::HookActivated {
                    hook: hook.label().to_string(),
                    hook_event: hook.event.as_str().to_string(),
                    hook_type: hook.hook_type.clone(),
                    source: scope.source.kind().to_string(),
                });
            }
        }
    }
}

pub struct ListInstalledSkillsTool {
    provider: Arc<dyn SkillProvider>,
}

impl ListInstalledSkillsTool {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for ListInstalledSkillsTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    fn name(&self) -> &str {
        LIST_INSTALLED_SKILLS_TOOL_NAME
    }

    fn description(&self) -> &str {
        "List installed skills available to activate with use_skill, including names, aliases, descriptions, and read-only package paths."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: installed_skills_message(&self.provider.list_skills()),
            error: None,
        })
    }
}

pub fn available_skills_message(skills: &[SkillManifest]) -> String {
    skills
        .iter()
        .map(|skill| {
            let name = skill_label(skill);
            let description = skill.description.as_deref().unwrap_or("");
            if description.is_empty() {
                format!("- {name}")
            } else {
                format!("- {name}: {description}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn installed_skills_message(skills: &[SkillManifest]) -> String {
    if skills.is_empty() {
        return "No installed skills are available. Installed Claude plugins only appear here when they include one or more skills/*/SKILL.md files.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("Installed skills: {}", skills.len()));
    for skill in skills {
        lines.push(render_installed_skill(skill));
    }
    lines.join("\n")
}

pub fn render_installed_skill(skill: &SkillManifest) -> String {
    let label = skill_label(skill);
    let mut line = format!("- {label}");
    if skill.name != label {
        line.push_str(&format!(" (name: {})", skill.name));
    }
    if let Some(description) = skill
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        line.push_str(&format!(": {description}"));
    }
    if !skill.aliases.is_empty() {
        line.push_str(&format!("; aliases: {}", skill.aliases.join(", ")));
    }
    if !skill.root_path.is_empty() {
        line.push_str(&format!("; root_path: {}", skill.root_path));
    }
    if !skill.entry_path.is_empty() {
        line.push_str(&format!("; entry_path: {}", skill.entry_path));
    }
    line
}

pub fn skill_label(skill: &SkillManifest) -> &str {
    skill.display_name.as_deref().unwrap_or(&skill.name)
}

pub fn skill_matches_selector(skill: &SkillManifest, selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() {
        return false;
    }
    let selector_slug = Slug::derive(selector);
    if skill.id.to_string() == selector {
        return true;
    }
    std::iter::once(skill.name.as_str())
        .chain(skill.display_name.as_deref())
        .chain(skill.aliases.iter().map(String::as_str))
        .any(|candidate| candidate == selector || Slug::derive(candidate) == selector_slug)
}

fn mutex_lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct TestSkillProvider {
        skills: Vec<SkillManifest>,
    }

    #[async_trait]
    impl SkillProvider for TestSkillProvider {
        fn list_skills(&self) -> Vec<SkillManifest> {
            self.skills.clone()
        }

        fn resolve_skill(&self, selector: &str) -> Option<SkillManifest> {
            self.skills
                .iter()
                .find(|skill| skill_matches_selector(skill, selector))
                .cloned()
        }

        async fn load_skill(&self, skill: &SkillManifest) -> Result<LoadedSkill> {
            Ok(LoadedSkill {
                name: skill_label(skill).to_string(),
                context: format!("# Skill: {}", skill_label(skill)),
                activation_env: vec![
                    ("CLAUDE_SKILL_DIR".to_string(), "/virtual/skill".to_string()),
                    (
                        "NENJO_ACTIVE_SKILLS".to_string(),
                        skill_label(skill).to_string(),
                    ),
                ],
                mcp_servers: skill.mcp_servers.clone(),
                mcp_tools: skill
                    .mcp_servers
                    .iter()
                    .map(|server| SkillMcpToolInfo {
                        server: server.clone(),
                        name: "review".to_string(),
                        description: Some("Review with the active skill MCP server".to_string()),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" }
                            }
                        }),
                    })
                    .collect(),
                hook_scopes: Vec::new(),
            })
        }
    }

    #[test]
    fn skill_selector_matches_aliases_without_filesystem() {
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "name": "acme__review",
            "display_name": "acme:review",
            "aliases": ["review", "code-review"],
            "root_dir": "/tmp/acme/skills/review"
        }))
        .unwrap();

        assert!(skill_matches_selector(&skill, "acme:review"));
        assert!(skill_matches_selector(&skill, "review"));
        assert!(skill_matches_selector(&skill, "code-review"));
    }

    #[test]
    fn skill_runtime_exports_provider_activation_env() {
        let state = SkillRuntimeState::default();
        state.activate(ActiveSkill {
            name: "acme:review".to_string(),
            env: vec![
                ("CLAUDE_SKILL_DIR".to_string(), "/virtual/skill".to_string()),
                ("NENJO_ACTIVE_SKILLS".to_string(), "acme:review".to_string()),
            ],
            mcp_servers: vec![Slug::derive("acme-mcp")],
        });

        let env = state.shell_env();
        assert!(env.contains(&("CLAUDE_SKILL_DIR".to_string(), "/virtual/skill".to_string())));
        assert!(env.contains(&("NENJO_ACTIVE_SKILLS".to_string(), "acme:review".to_string())));
        assert_eq!(state.active_mcp_servers(), vec![Slug::derive("acme-mcp")]);
    }

    #[tokio::test]
    async fn use_skill_activates_loaded_skill() {
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "name": "acme__review",
            "display_name": "acme:review",
            "aliases": ["review"],
            "description": "Review code changes.",
            "root_path": "skills/review",
            "entry_path": "SKILL.md",
            "root_dir": "/tmp/acme/skills/review"
        }))
        .unwrap();
        let provider = Arc::new(TestSkillProvider {
            skills: vec![skill],
        });
        let runtime = Arc::new(SkillRuntimeState::default());

        let result = UseSkillTool::new(provider, runtime.clone())
            .execute(json!({ "name": "review" }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("# Skill: acme:review"));
        assert!(
            runtime
                .shell_env()
                .contains(&("CLAUDE_SKILL_DIR".to_string(), "/virtual/skill".to_string()))
        );
    }

    #[tokio::test]
    async fn use_skill_output_lists_active_skill_mcp_tools() {
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "name": "acme__review",
            "display_name": "acme:review",
            "root_dir": "/tmp/acme/skills/review",
            "mcp_servers": ["acme_review_mcp"]
        }))
        .unwrap();
        let provider = Arc::new(TestSkillProvider {
            skills: vec![skill],
        });
        let runtime = Arc::new(SkillRuntimeState::default());

        let result = UseSkillTool::new(provider, runtime)
            .execute(json!({ "name": "acme:review" }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("ACTIVE SKILL MCP TOOLS"));
        assert!(result.output.contains("call_skill_mcp_tool"));
        assert!(
            result
                .output
                .contains("server: `acme_review_mcp`, tool: `review`")
        );
        assert!(result.output.contains("arguments_schema"));
    }

    #[test]
    fn use_skill_is_runtime_mutating() {
        let tool = UseSkillTool::new(
            Arc::new(TestSkillProvider::default()),
            Arc::new(SkillRuntimeState::default()),
        );

        assert_eq!(tool.category(), ToolCategory::ReadWrite);
    }

    #[tokio::test]
    async fn list_installed_skills_explains_empty_registry() {
        let result = ListInstalledSkillsTool::new(Arc::new(TestSkillProvider::default()))
            .execute(json!({}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("No installed skills are available"));
        assert!(result.output.contains("skills/*/SKILL.md"));
    }
}
