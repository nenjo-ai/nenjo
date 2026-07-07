//! Fully configured agent instance ready for task execution.

use crate::context::ContextRenderer;
use nenjo_models::NativeModelToolId;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::async_ops::AsyncOpManager;
use crate::agents::prompts::PromptContext;
use crate::arguments::{merge_argument_bindings, scan_argument_selectors};
use crate::config::AgentConfig;
use crate::hooks::HookRuntime;
use crate::input::{AgentRun, AgentRunKind, render_context_from_agent_run};
use crate::manifest::{AgentManifest, ModelManifest, PromptConfig};
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::slug::Slug;
use crate::tools::{Tool, ToolSecurity, ToolSpec};
use crate::types::DelegationContext;

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
    pub(crate) model_manifest: ModelManifest,
    pub(crate) model: AgentModel<P>,
    pub(crate) prompt: AgentPromptState,
    pub(crate) runtime: AgentRuntime<P>,
}

/// Model provider binding selected for an agent instance.
pub(crate) struct AgentModel<P: ProviderRuntime = ErasedProvider> {
    pub(crate) model_name: String,
    pub(crate) model_slug: Slug,
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
    pub(crate) sub_agent_ctx: Option<DelegationContext>,
    pub(crate) async_ops: AsyncOpManager,
    pub(crate) execution_cancel: CancellationToken,
    pub(crate) execution_mode: AgentExecutionMode,
    pub(crate) hook_runtime: Option<Arc<HookRuntime>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentExecutionMode {
    Parent,
    EphemeralChild,
    DelegatedChild,
}

impl AgentExecutionMode {
    pub(crate) fn has_own_capability_surface(self) -> bool {
        matches!(self, Self::Parent | Self::DelegatedChild)
    }

    pub(crate) fn can_use_abilities(self) -> bool {
        matches!(self, Self::Parent | Self::DelegatedChild)
    }

    pub(crate) fn can_orchestrate(self) -> bool {
        matches!(self, Self::Parent)
    }

    pub(crate) fn can_respond_to_user(self) -> bool {
        matches!(self, Self::Parent)
    }

    pub(crate) fn strips_prompt_capabilities(self) -> bool {
        matches!(self, Self::EphemeralChild | Self::DelegatedChild)
    }

    fn delegation_prompt_guard(self) -> Option<&'static str> {
        match self {
            Self::DelegatedChild => Some(DELEGATED_CHILD_PROMPT_GUARD),
            Self::Parent | Self::EphemeralChild => None,
        }
    }
}

const DELEGATED_CHILD_PROMPT_GUARD: &str = r#"Delegated work boundary:
You are receiving a delegated task from another agent. Before doing the work, decide whether the task fits your agent role, description, prompt instructions, and assigned capability surface. If it does not fit, do not improvise as a generic assistant and do not call tools. Return a brief refusal explaining that the delegated task is outside your role and name the kind of agent or capability that should handle it. If it does fit, complete only the delegated task and report a focused result back to the parent agent."#;

impl<P: ProviderRuntime> Clone for AgentModel<P> {
    fn clone(&self) -> Self {
        Self {
            model_name: self.model_name.clone(),
            model_slug: self.model_slug.clone(),
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
            sub_agent_ctx: self.sub_agent_ctx.clone(),
            async_ops: self.async_ops.clone(),
            execution_cancel: self.execution_cancel.clone(),
            execution_mode: self.execution_mode,
            hook_runtime: self.hook_runtime.clone(),
        }
    }
}

impl<P: ProviderRuntime> Clone for AgentInstance<P> {
    fn clone(&self) -> Self {
        Self {
            manifest: self.manifest.clone(),
            model_manifest: self.model_manifest.clone(),
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
            .field("model_slug", &self.model.model_slug)
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

    /// Agent manifest slug.
    pub fn agent_slug(&self) -> &Slug {
        &self.manifest.slug
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

    /// Model manifest slug selected for this instance.
    pub fn model_slug(&self) -> &Slug {
        &self.model.model_slug
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

    /// Attach the active hook runtime for this execution.
    pub fn set_hook_runtime(&mut self, hook_runtime: Option<Arc<HookRuntime>>) {
        self.runtime.hook_runtime = hook_runtime;
    }

    /// Get tool specs for LLM function calling registration.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.local_tool_specs();
        specs.extend(native_model_tool_specs(&self.model_manifest.native_tools));
        specs
    }

    /// Get executable local tool specs for provider function-calling registration.
    ///
    /// Provider-native tools are intentionally excluded here. They are passed
    /// through `ChatRequest::native_tools` and executed by the provider, not by
    /// the local tool runtime.
    pub(crate) fn local_tool_specs(&self) -> Vec<ToolSpec> {
        self.runtime
            .tools
            .iter()
            .filter(|tool| {
                !native_model_tool_shadows_local_tool(
                    &self.model_manifest.native_tools,
                    tool.name(),
                )
            })
            .map(|t| t.spec())
            .collect()
    }

    /// Build the system, developer, and user prompts for an execution.
    ///
    /// All three prompts are Jinja templates rendered with the same
    /// `HashMap<String, String>` of template variables. Context blocks
    /// (from the DB) are rendered first, then merged into the vars so
    /// `{{ context.* }}` references resolve in the final prompts.
    pub fn build_prompts(&self, run: &AgentRun) -> BuiltPrompts {
        self.try_build_prompts(run)
            .expect("prompt argument bindings should be valid")
    }

    /// Fallible prompt builder used by the execution path so missing/conflicting
    /// runtime arguments fail before the model call.
    pub fn try_build_prompts(&self, run: &AgentRun) -> anyhow::Result<BuiltPrompts> {
        self.build_prompts_with_vars(run, None, None)
    }

    pub(crate) fn build_prompts_with_vars(
        &self,
        run: &AgentRun,
        memory_vars: Option<&HashMap<String, String>>,
        artifact_vars: Option<&HashMap<String, String>>,
    ) -> anyhow::Result<BuiltPrompts> {
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
        ctx._self.slug = self.manifest.slug().to_string();
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
        let argument_vars = merge_argument_bindings(
            &self.prompt.context.argument_bindings,
            &run.execution.argument_bindings,
        )?;
        vars.extend(argument_vars);

        // 4. Render context blocks and merge into vars
        validate_argument_references(
            &vars,
            [
                prompt_config.system_prompt.as_str(),
                prompt_config.developer_prompt.as_str(),
                prompt_config.templates.chat_task.as_str(),
                prompt_config.templates.task_execution.as_str(),
                prompt_config.templates.gate_eval.as_str(),
                prompt_config.templates.heartbeat_task.as_str(),
            ],
            self.prompt.renderer.argument_selectors(),
        )?;
        let rendered_blocks = self.prompt.renderer.render_all(&vars);
        vars.extend(rendered_blocks);

        if !ctx.project.context.is_empty() {
            let mut project_context_vars = vars.clone();
            project_context_vars.remove("project");
            project_context_vars.remove("project.context");
            let rendered_context = self
                .prompt
                .renderer
                .render_template(&ctx.project.context, &project_context_vars);
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
        if let Some(guard) = self.runtime.execution_mode.delegation_prompt_guard() {
            if !developer.is_empty() {
                developer.push_str("\n\n");
            }
            developer.push_str(guard);
        }

        // 6. Select the user message template based on task type
        let (task_type_name, task_template) = match &run.kind {
            AgentRunKind::Task { .. } => ("Task", prompt_config.templates.task_execution.as_str()),
            AgentRunKind::Chat(chat) => (
                "Chat",
                chat.template_override
                    .as_deref()
                    .unwrap_or(prompt_config.templates.chat_task.as_str()),
            ),
            AgentRunKind::FollowUp { .. } => ("FollowUp", ""),
            AgentRunKind::Gate { .. } => ("Gate", prompt_config.templates.gate_eval.as_str()),
            AgentRunKind::Heartbeat { .. } => {
                ("Heartbeat", prompt_config.templates.heartbeat_task.as_str())
            }
        };
        tracing::debug!(
            agent = %self.name(),
            task_type = task_type_name,
            template_len = task_template.len(),
            "Selected task template"
        );

        // 7. Render all three prompts with the same vars
        let system = self
            .prompt
            .renderer
            .render_template(&prompt_config.system_prompt, &vars);
        let developer = self.prompt.renderer.render_template(&developer, &vars);
        let user_message = self.prompt.renderer.render_template(task_template, &vars);

        Ok(BuiltPrompts {
            system,
            developer,
            user_message,
        })
    }
}

fn validate_argument_references<'a>(
    vars: &HashMap<String, String>,
    prompt_templates: impl IntoIterator<Item = &'a str>,
    context_selectors: Vec<String>,
) -> anyhow::Result<()> {
    let mut missing = Vec::new();
    for selector in prompt_templates
        .into_iter()
        .flat_map(scan_argument_selectors)
        .chain(context_selectors)
    {
        if !vars.contains_key(&selector) && !missing.contains(&selector) {
            missing.push(selector);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("missing runtime argument bindings: {}", missing.join(", "))
    }
}

fn native_model_tool_shadows_local_tool(
    native_tools: &[NativeModelToolId],
    local_tool_name: &str,
) -> bool {
    native_tools
        .iter()
        .any(|tool| tool.as_str() == "web_search")
        && local_tool_name == "web_search_tool"
}

fn native_model_tool_specs(native_tools: &[NativeModelToolId]) -> Vec<ToolSpec> {
    native_tools
        .iter()
        .map(|tool| ToolSpec {
            name: tool.as_str().to_string(),
            description: format!(
                "Provider-native model tool '{}' executed by the configured model provider.",
                tool.as_str()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: crate::tools::ToolCategory::Read,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_native_web_search_shadows_local_web_search_tool() {
        assert!(native_model_tool_shadows_local_tool(
            &[NativeModelToolId::from("web_search")],
            "web_search_tool",
        ));
        assert!(!native_model_tool_shadows_local_tool(
            &[NativeModelToolId::from("x_search")],
            "web_search_tool",
        ));
        assert!(!native_model_tool_shadows_local_tool(
            &[NativeModelToolId::from("web_search")],
            "shell",
        ));
    }

    #[test]
    fn native_model_tool_specs_are_visible_tool_belt_entries() {
        let specs = native_model_tool_specs(&[
            NativeModelToolId::from("web_search"),
            NativeModelToolId::from("x_search"),
        ]);
        let names = specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["web_search", "x_search"]);
        assert!(
            specs
                .iter()
                .all(|spec| spec.category == crate::tools::ToolCategory::Read)
        );
    }
}
