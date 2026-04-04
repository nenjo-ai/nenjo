//! Fully configured agent instance ready for task execution.

use crate::context::ContextRenderer;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use uuid::Uuid;

use nenjo_models::ModelProvider;
use nenjo_tools::security::SecurityPolicy;
use nenjo_tools::{Tool, ToolSpec};

use super::prompts::PromptConfig;
use crate::agents::prompts::{self as prompts, PromptContext};
use crate::config::AgentConfig;
use crate::types::{RenderContextExt, RenderContextVars, TaskType};

/// The system and developer prompts ready for the turn loop.
#[derive(Debug)]
pub struct BuiltPrompts {
    pub system: String,
    pub developer: String,
    pub user_message: String,
}

impl Display for BuiltPrompts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f)?;
        writeln!(f, "=== System Prompt ===")?;
        writeln!(f, "{}", self.system)?;
        writeln!(f)?;
        writeln!(f, "=== Developer Prompt ===")?;
        writeln!(f, "{}", self.developer)?;
        writeln!(f)?;
        writeln!(f, "=== User Message ===")?;
        write!(f, "{}", self.user_message)
    }
}

/// A fully configured agent instance ready for task execution.
#[derive(Clone)]
pub struct AgentInstance {
    pub name: String,
    pub description: String,
    pub agent_id: Option<Uuid>,
    pub model: String,
    pub model_id: Uuid,
    pub temperature: f64,
    pub prompt_config: PromptConfig,
    pub prompt_context: PromptContext,
    pub provider: Arc<dyn ModelProvider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub security: Arc<SecurityPolicy>,
    pub agent_config: AgentConfig,
    pub context_renderer: ContextRenderer,
    pub memory_vars: HashMap<String, String>,
    pub resource_vars: HashMap<String, String>,
    pub documents_xml: String,
}

impl std::fmt::Debug for AgentInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentInstance")
            .field("name", &self.name)
            .field("model_id", &self.model_id)
            .field("model", &self.model)
            .field("temperature", &self.temperature)
            .field("tools_count", &self.tools.len())
            .finish_non_exhaustive()
    }
}

impl AgentInstance {
    /// Get tool specs for LLM function calling registration.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// Build the system, developer, and user prompts for an execution.
    ///
    /// All three prompts are Jinja templates rendered with the same
    /// `HashMap<String, String>` of template variables. Context blocks
    /// (from the DB) are rendered first, then merged into the vars so
    /// `{{ context.* }}` references resolve in the final prompts.
    pub fn build_prompts(&self, task: &TaskType) -> BuiltPrompts {
        // 1. Build the render context from task + extras
        let mut ctx = RenderContextVars::from_task(task);
        let ex = &self.prompt_context.render_ctx_extra;

        // Project — merge from extras, derive working_dir from workspace/slug
        if !ex.project.name.is_empty() {
            ctx.project = ex.project.clone();
        }
        if !ex.project.slug.is_empty() {
            ctx.project.working_dir = self
                .security
                .workspace_dir
                .join(&ex.project.slug)
                .to_string_lossy()
                .to_string();
        }

        // Git — task-level git (worktree) takes priority over project-level git (repo).
        // from_task() already set ctx.git if the task had git context.
        // Only fall back to project-level git if the task didn't provide one.
        if ctx.git.is_empty() && !ex.git.is_empty() {
            ctx.git = ex.git.clone();
        }

        // Routine — merge from extras
        if !ex.routine.name.is_empty() {
            ctx.routine = ex.routine.clone();
        }
        if !ex.routine.step.is_empty() {
            ctx.routine.step = ex.routine.step.clone();
        }

        // Agent (self)
        ctx._self.id = self.agent_id.unwrap_or_default();
        ctx._self.role = self.name.clone();
        ctx._self.display_name = self.name.clone();
        ctx._self.model_name = self.model.clone();
        ctx._self.description = Some(self.description.clone());

        // Global
        ctx.timestamp = chrono::Utc::now().to_rfc3339();

        // Memory profile
        ctx.memory_profile = crate::context::MemoryProfileContext {
            core_focus: if self.prompt_config.memory_profile.core_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: self.prompt_config.memory_profile.core_focus.clone(),
                })
            },
            project_focus: if self.prompt_config.memory_profile.project_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: self.prompt_config.memory_profile.project_focus.clone(),
                })
            },
            shared_focus: if self.prompt_config.memory_profile.shared_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: self.prompt_config.memory_profile.shared_focus.clone(),
                })
            },
        };

        // 2. Populate available collections (exclude self from agents)
        let self_id = self.agent_id;
        ctx.available_agents = self
            .prompt_context
            .available_agents
            .iter()
            .filter(|a| self_id.is_none_or(|id| a.id != id))
            .map(prompts::render_agent)
            .collect();
        ctx.available_abilities = self
            .prompt_context
            .available_abilities
            .iter()
            .map(prompts::render_ability)
            .collect();
        ctx.available_domains = self
            .prompt_context
            .available_domains
            .iter()
            .map(prompts::render_domain)
            .collect();
        ctx.available_skills = self
            .prompt_context
            .skills
            .iter()
            .map(prompts::render_skill)
            .collect();

        // Memories, resources, and documents
        ctx.memory_vars = self.memory_vars.clone();
        ctx.resource_vars = self.resource_vars.clone();
        ctx.documents_xml = self.documents_xml.clone();

        // 3. Build the vars HashMap once
        let mut vars = ctx.to_vars();

        // 4. Render context blocks and merge into vars
        let rendered_blocks = self.context_renderer.render_all(&vars);
        vars.extend(rendered_blocks);

        // 5. Assemble developer prompt
        // Domain system_addon is appended when a domain session is active.
        let mut developer = self.prompt_config.developer_prompt.clone();
        if let Some(ref domain) = self.prompt_context.active_domain
            && let Some(ref addon) = domain.manifest.prompt.system_addon
            && !addon.is_empty()
        {
            if !developer.is_empty() {
                developer.push_str("\n\n");
            }
            developer.push_str(addon);
        }

        // 6. Select the user message template based on task type
        let (task_type_name, task_template) = match task {
            TaskType::Task { .. } => ("Task", &self.prompt_config.templates.task_execution),
            TaskType::Chat { .. } => ("Chat", &self.prompt_config.templates.chat_task),
            TaskType::Gate { .. } => ("Gate", &self.prompt_config.templates.gate_eval),
            TaskType::CouncilSubtask { .. } => {
                ("CouncilSubtask", &self.prompt_config.templates.chat_task)
            }
            TaskType::Cron { .. } => ("Cron", &self.prompt_config.templates.cron_task),
        };
        tracing::debug!(
            agent = %self.name,
            task_type = task_type_name,
            template_len = task_template.len(),
            "Selected task template"
        );

        // 7. Render all three prompts with the same vars
        let system = nenjo_xml::template::render_template(&self.prompt_config.system_prompt, &vars);
        let developer = nenjo_xml::template::render_template(&developer, &vars);
        let user_message = nenjo_xml::template::render_template(task_template, &vars);

        BuiltPrompts {
            system,
            developer,
            user_message,
        }
    }
}

/// Document manifest entry (mirrors harness doc_sync).
#[derive(Debug, Clone, serde::Deserialize)]
struct ManifestEntry {
    filename: String,
    size_bytes: i64,
}

/// Document manifest (mirrors harness doc_sync).
#[derive(Debug, Clone, serde::Deserialize)]
struct DocumentManifest {
    documents: Vec<ManifestEntry>,
}

/// Build a compact XML listing of project documents from a manifest file.
///
/// Returns empty string if no manifest exists or no documents are present.
pub fn build_document_listing(docs_base_dir: &std::path::Path, project_slug: &str) -> String {
    let project_dir = docs_base_dir.join(project_slug);
    let manifest_path = project_dir.join("manifest.json");
    let manifest: DocumentManifest = match std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(m) => m,
        None => return String::new(),
    };

    if manifest.documents.is_empty() {
        return String::new();
    }

    let ctx = crate::context::ProjectDocumentsContext {
        path: project_slug.to_string(),
        documents: manifest
            .documents
            .iter()
            .map(|doc| crate::context::DocumentContext {
                name: doc.filename.clone(),
                size: format_size(doc.size_bytes),
            })
            .collect(),
    };

    nenjo_xml::to_xml_pretty(&ctx, 2)
}

/// Format bytes into a human-readable size string.
fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(2048), "2KB");
        assert_eq!(format_size(1_500_000), "1.4MB");
    }

    // Agent prompt building tests live in harness (they need AgentBuilder
    // and doc_sync which depend on harness infrastructure).
}
