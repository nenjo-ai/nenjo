//! Fully configured agent instance ready for task execution.

use crate::context::ContextRenderer;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use uuid::Uuid;

use crate::agents::prompts::{self as prompts, PromptContext};
use crate::config::AgentConfig;
use crate::input::{AgentRun, AgentRunKind, render_context_from_agent_run};
use crate::manifest::{AgentManifest, PromptConfig};
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::tools::{Tool, ToolSecurity, ToolSpec};

/// The system and developer prompts ready for the turn loop.
#[derive(Debug)]
pub struct BuiltPrompts {
    /// Rendered system prompt.
    pub system: String,
    /// Rendered developer prompt.
    pub developer: String,
    /// Rendered user message for the current run.
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
pub struct AgentInstance<P: ProviderRuntime = ErasedProvider> {
    pub(crate) manifest: AgentManifest,
    pub(crate) model: AgentModel<P>,
    pub(crate) prompt: AgentPromptState,
    pub(crate) runtime: AgentRuntime<P>,
}

/// Model provider binding selected for an agent instance.
pub(crate) struct AgentModel<P: ProviderRuntime = ErasedProvider> {
    pub(crate) model_name: String,
    pub(crate) id: Uuid,
    pub(crate) temperature: f64,
    pub(crate) model_provider: Arc<P::Model<'static>>,
}

/// Prompt rendering state carried by an agent instance.
#[derive(Clone)]
pub(crate) struct AgentPromptState {
    pub(crate) context: PromptContext,
    pub(crate) renderer: ContextRenderer,
    pub(crate) memory_vars: HashMap<String, String>,
    pub(crate) artifact_vars: HashMap<String, String>,
}

/// Runtime resources attached to an agent instance.
pub(crate) struct AgentRuntime<P: ProviderRuntime = ErasedProvider> {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) security: Arc<ToolSecurity>,
    pub(crate) config: AgentConfig,
    pub(crate) provider_runtime: Option<P>,
}

impl<P: ProviderRuntime> Clone for AgentModel<P> {
    fn clone(&self) -> Self {
        Self {
            model_name: self.model_name.clone(),
            id: self.id,
            temperature: self.temperature,
            model_provider: self.model_provider.clone(),
        }
    }
}

impl<P: ProviderRuntime> Clone for AgentRuntime<P> {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            security: self.security.clone(),
            config: self.config.clone(),
            provider_runtime: self.provider_runtime.clone(),
        }
    }
}

impl<P: ProviderRuntime> Clone for AgentInstance<P> {
    fn clone(&self) -> Self {
        Self {
            manifest: self.manifest.clone(),
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

impl<P: ProviderRuntime> std::fmt::Debug for AgentInstance<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentInstance")
            .field("name", &self.manifest.name)
            .field("model_id", &self.model.id)
            .field("model", &self.model.model_name)
            .field("temperature", &self.model.temperature)
            .field("tools_count", &self.runtime.tools.len())
            .finish_non_exhaustive()
    }
}

impl<P: ProviderRuntime> AgentInstance<P> {
    /// Agent name from the manifest.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Agent description from the manifest, or an empty string if absent.
    pub fn description(&self) -> &str {
        self.manifest.description.as_deref().unwrap_or_default()
    }

    /// Agent manifest ID.
    pub fn agent_id(&self) -> Uuid {
        self.manifest.id
    }

    /// Prompt configuration from the agent manifest.
    pub fn prompt_config(&self) -> &PromptConfig {
        &self.manifest.prompt_config
    }

    /// Agent manifest used to build this instance.
    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    /// Model name selected for this instance.
    pub fn model_name(&self) -> &str {
        &self.model.model_name
    }

    /// Model manifest ID selected for this instance.
    pub fn model_id(&self) -> Uuid {
        self.model.id
    }

    /// Model temperature selected for this instance.
    pub fn temperature(&self) -> f64 {
        self.model.temperature
    }

    /// Prompt context used when rendering agent prompts.
    pub fn prompt_context(&self) -> &PromptContext {
        &self.prompt.context
    }

    /// Tools available to this agent instance.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.runtime.tools
    }

    /// Tool security policy for this instance.
    pub fn security(&self) -> &ToolSecurity {
        &self.runtime.security
    }

    /// Update the active domain session ID, returning whether a domain was active.
    pub fn set_active_domain_session_id(&mut self, session_id: Uuid) -> bool {
        let Some(active_domain) = self.prompt.context.active_domain.as_mut() else {
            return false;
        };
        active_domain.session_id = session_id;
        true
    }

    /// Get tool specs for LLM function calling registration.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.runtime.tools.iter().map(|t| t.spec()).collect()
    }

    /// Build the system, developer, and user prompts for an execution.
    ///
    /// All three prompts are Jinja templates rendered with the same
    /// `HashMap<String, String>` of template variables. Context blocks
    /// (from the DB) are rendered first, then merged into the vars so
    /// `{{ context.* }}` references resolve in the final prompts.
    pub fn build_prompts(&self, run: &AgentRun) -> BuiltPrompts {
        self.build_prompts_with_vars(run, None, None)
    }

    pub(crate) fn build_prompts_with_vars(
        &self,
        run: &AgentRun,
        memory_vars: Option<&HashMap<String, String>>,
        artifact_vars: Option<&HashMap<String, String>>,
    ) -> BuiltPrompts {
        // 1. Build the render context from the run input + extras
        let mut ctx = render_context_from_agent_run(run);
        let ex = &self.prompt.context.render_ctx_extra;

        // Project — merge from extras, derive working_dir from workspace/slug
        if !ex.project.name.is_empty() {
            ctx.project = ex.project.clone();
        }
        if !ex.project.slug.is_empty() {
            ctx.project.working_dir = self
                .runtime
                .security
                .workspace_dir
                .join(&ex.project.slug)
                .to_string_lossy()
                .to_string();
        }

        // Runtime git/worktree context takes priority over project-level git.
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
        ctx._self.id = self.agent_id();
        ctx._self.role = self.name().to_string();
        ctx._self.display_name = self.name().to_string();
        ctx._self.model_name = self.model.model_name.clone();
        ctx._self.description = Some(self.description().to_string());

        // Global
        ctx.timestamp = chrono::Utc::now().to_rfc3339();

        // Memory profile
        let prompt_config = self.prompt_config();
        ctx.memory_profile = crate::context::MemoryProfileContext {
            core_focus: if prompt_config.memory_profile.core_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: prompt_config.memory_profile.core_focus.clone(),
                })
            },
            project_focus: if prompt_config.memory_profile.project_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: prompt_config.memory_profile.project_focus.clone(),
                })
            },
            shared_focus: if prompt_config.memory_profile.shared_focus.is_empty() {
                None
            } else {
                Some(crate::context::FocusListContext {
                    items: prompt_config.memory_profile.shared_focus.clone(),
                })
            },
        };

        // 2. Populate available collections (exclude self from agents)
        let self_id = self.agent_id();
        ctx.available_agents = self
            .prompt
            .context
            .available_agents
            .iter()
            .filter(|a| a.id != self_id)
            .map(prompts::render_agent)
            .collect();
        ctx.available_abilities = self
            .prompt
            .context
            .available_abilities
            .iter()
            .map(prompts::render_ability)
            .collect();
        ctx.available_domains = self
            .prompt
            .context
            .available_domains
            .iter()
            .map(prompts::render_domain)
            .collect();

        // Memories and artifacts
        ctx.memory_vars = memory_vars
            .cloned()
            .unwrap_or_else(|| self.prompt.memory_vars.clone());
        ctx.artifact_vars = artifact_vars
            .cloned()
            .unwrap_or_else(|| self.prompt.artifact_vars.clone());
        ctx.knowledge_vars = ex.knowledge_vars.clone();

        // 3. Build the vars HashMap once
        let mut vars = ctx.to_vars();

        // 4. Render context blocks and merge into vars
        let rendered_blocks = self.prompt.renderer.render_all(&vars);
        vars.extend(rendered_blocks);

        if !ctx.project.context.is_empty() {
            let mut project_context_vars = vars.clone();
            project_context_vars.remove("project");
            project_context_vars.remove("project.context");
            let rendered_context =
                nenjo_xml::template::render_template(&ctx.project.context, &project_context_vars);
            ctx.project.context = rendered_context.clone();
            if rendered_context.is_empty() {
                vars.remove("project.context");
            } else {
                vars.insert("project.context".into(), rendered_context);
            }
            if !ctx.project.is_empty() {
                vars.insert("project".into(), nenjo_xml::to_xml_pretty(&ctx.project, 2));
            }
        }

        // 5. Assemble developer prompt
        // Domain developer prompt addon is appended when a domain session is active.
        let mut developer = prompt_config.developer_prompt.clone();
        if self.prompt.context.append_active_domain_addon
            && let Some(ref domain) = self.prompt.context.active_domain
            && let Some(ref addon) = domain.manifest.prompt_config.developer_prompt_addon
            && !addon.is_empty()
        {
            if !developer.is_empty() {
                developer.push_str("\n\n");
            }
            developer.push_str(addon);
        }

        // 6. Select the user message template based on task type
        let (task_type_name, task_template) = match &run.kind {
            AgentRunKind::Task { .. } => ("Task", &prompt_config.templates.task_execution),
            AgentRunKind::Chat { .. } => ("Chat", &prompt_config.templates.chat_task),
            AgentRunKind::Gate { .. } => ("Gate", &prompt_config.templates.gate_eval),
            AgentRunKind::CouncilSubtask { .. } => {
                ("CouncilSubtask", &prompt_config.templates.chat_task)
            }
            AgentRunKind::Cron { .. } => ("Cron", &prompt_config.templates.cron_task),
            AgentRunKind::Heartbeat { .. } => {
                ("Heartbeat", &prompt_config.templates.heartbeat_task)
            }
        };
        tracing::debug!(
            agent = %self.name(),
            task_type = task_type_name,
            template_len = task_template.len(),
            "Selected task template"
        );

        // 7. Render all three prompts with the same vars
        let system = nenjo_xml::template::render_template(&prompt_config.system_prompt, &vars);
        let developer = nenjo_xml::template::render_template(&developer, &vars);
        let user_message = nenjo_xml::template::render_template(task_template, &vars);

        BuiltPrompts {
            system,
            developer,
            user_message,
        }
    }
}
