//! Fully configured agent instance ready for task execution.

use crate::context::{ContextRenderer, ProjectContext};
use nenjo_models::{
    ConversationMessage, NativeModelToolId, RuntimeContextAuthority, RuntimeContextMessage,
    RuntimeContextScope,
};
use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;
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

/// Whether a prompt plan created runtime context or reused its persisted snapshot.
#[derive(Debug)]
pub enum RuntimeContextPlan {
    Created(Vec<RuntimeContextMessage>),
    Reused(Vec<RuntimeContextMessage>),
}

impl RuntimeContextPlan {
    pub fn messages(&self) -> &[RuntimeContextMessage] {
        match self {
            Self::Created(messages) | Self::Reused(messages) => messages,
        }
    }

    pub fn created_messages(&self) -> Option<&[RuntimeContextMessage]> {
        match self {
            Self::Created(messages) => Some(messages),
            Self::Reused(_) => None,
        }
    }
}

/// Static instructions and runtime-owned context ready for the turn loop.
#[derive(Debug)]
pub struct BuiltPrompts {
    /// Compiled system prompt.
    pub system: String,
    /// Compiled developer prompt.
    pub developer: String,
    pub session_context: RuntimeContextPlan,
    pub turn_context: RuntimeContextPlan,
}

impl Display for BuiltPrompts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f)?;
        writeln!(f, "=== System Prompt ===")?;
        writeln!(f, "{}", self.system)?;
        writeln!(f)?;
        writeln!(f, "=== Developer Prompt ===")?;
        write!(f, "{}", self.developer)
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
    pub(crate) memory_context: String,
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
    pub(crate) current_session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentExecutionMode {
    Parent,
    Ability,
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

    pub(crate) fn strips_prompt_capabilities(self) -> bool {
        matches!(
            self,
            Self::Ability | Self::EphemeralChild | Self::DelegatedChild
        )
    }

    fn delegation_prompt_guard(self) -> Option<&'static str> {
        match self {
            Self::DelegatedChild => Some(DELEGATED_CHILD_PROMPT_GUARD),
            Self::Parent | Self::Ability | Self::EphemeralChild => None,
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
            current_session_id: self.current_session_id,
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

    /// Set the current transcript session for tools created by this instance.
    #[doc(hidden)]
    pub fn set_current_session_id(&mut self, session_id: Uuid) {
        self.runtime.current_session_id = Some(session_id);
    }

    /// Attach the active hook runtime for this execution.
    pub fn set_hook_runtime(&mut self, hook_runtime: Option<Arc<HookRuntime>>) {
        self.runtime.hook_runtime = hook_runtime;
    }

    /// Get the full registered tool specs for capability introspection.
    ///
    /// This includes dynamically hidden tools. Model requests use the
    /// visibility-filtered specs produced by the turn loop.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.local_tool_specs();
        specs.extend(native_model_tool_specs(&self.model_manifest.native_tools));
        sort_tool_specs(&mut specs);
        specs
    }

    /// Get the tool specs currently visible to the model, including native tools.
    pub(crate) async fn visible_tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.visible_local_tool_specs().await;
        specs.extend(native_model_tool_specs(&self.model_manifest.native_tools));
        sort_tool_specs(&mut specs);
        specs
    }

    /// Get executable local tool specs for provider function-calling registration.
    ///
    /// Provider-native tools are intentionally excluded here. They are passed
    /// through `ChatRequest::native_tools` and executed by the provider, not by
    /// the local tool runtime.
    pub(crate) fn local_tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .runtime
            .tools
            .iter()
            .filter(|tool| {
                !native_model_tool_shadows_local_tool(
                    &self.model_manifest.native_tools,
                    tool.name(),
                )
            })
            .map(|t| t.spec())
            .collect::<Vec<_>>();
        sort_tool_specs(&mut specs);
        specs
    }

    /// Get executable local tool specs currently visible to the model.
    pub(crate) async fn visible_local_tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        for tool in &self.runtime.tools {
            if native_model_tool_shadows_local_tool(&self.model_manifest.native_tools, tool.name())
                || !tool.is_available_to_model().await
            {
                continue;
            }
            specs.push(tool.spec());
        }
        sort_tool_specs(&mut specs);
        specs
    }

    /// Compile static instructions and build runtime-owned context for an execution.
    pub fn build_prompts(&self, run: &AgentRun) -> anyhow::Result<BuiltPrompts> {
        self.build_prompts_with_memory_context(run, None)
    }

    /// Fallible prompt builder used by the execution path so missing/conflicting
    /// runtime arguments fail before the model call.
    pub fn try_build_prompts(&self, run: &AgentRun) -> anyhow::Result<BuiltPrompts> {
        self.build_prompts(run)
    }

    pub(crate) fn build_prompts_with_memory_context(
        &self,
        run: &AgentRun,
        memory_context: Option<&str>,
    ) -> anyhow::Result<BuiltPrompts> {
        // Build runtime context inputs from the run and executor-owned extras.
        let mut ctx = render_context_from_agent_run(run);
        let ex = &self.prompt.context.render_ctx_extra;

        // Project — merge from extras and preserve an explicitly scoped worktree.
        if !ex.project.name.is_empty() {
            ctx.project = ex.project.clone();
        }
        populate_project_working_directory(&mut ctx.project, &self.runtime.security.workspace_dir);

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

        let agent_context = crate::context::AgentContext {
            slug: self.manifest.slug().to_string(),
            name: self.name().to_string(),
            description: (!self.description().is_empty()).then(|| self.description().to_string()),
        };
        let prompt_config = self.prompt_config();
        let mut static_vars = merge_argument_bindings(
            &self.prompt.context.argument_bindings,
            &run.execution.argument_bindings,
        )?;
        static_vars.extend(ex.knowledge_vars.clone());

        // Static context fragments and package arguments are compiled without turn data.
        validate_argument_references(
            &static_vars,
            [
                prompt_config.system_prompt.as_str(),
                prompt_config.developer_prompt.as_str(),
            ],
            self.prompt.renderer.argument_selectors(),
        )?;
        let renderer =
            self.prompt
                .renderer
                .with_policy(crate::package_resolve::policy_from_agent_metadata(
                    self.manifest.source_type.as_deref(),
                    Some(&self.manifest.metadata),
                ));
        static_vars.extend(renderer.render_all(&static_vars));

        // Domain and delegation overlays are static for this instruction epoch.
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

        validate_static_prompt_sources(
            [prompt_config.system_prompt.as_str(), developer.as_str()],
            self.prompt.renderer.runtime_selectors(),
        )?;

        let system = renderer.render_template(&prompt_config.system_prompt, &static_vars);
        let developer = renderer.render_template(&developer, &static_vars);
        let resolved_memory = memory_context.unwrap_or(&self.prompt.memory_context);
        let session_context = match existing_session_contexts(run) {
            Some(messages) => RuntimeContextPlan::Reused(messages),
            None => RuntimeContextPlan::Created(crate::context::runtime::session_contexts(
                &agent_context,
                &ctx,
                resolved_memory,
            )),
        };
        let turn_context = match replayed_turn_contexts(run) {
            Some(messages) => RuntimeContextPlan::Reused(messages),
            None => RuntimeContextPlan::Created(crate::context::runtime::turn_contexts(&ctx, run)),
        };

        Ok(BuiltPrompts {
            system,
            developer,
            session_context,
            turn_context,
        })
    }
}

fn replayed_turn_contexts(run: &AgentRun) -> Option<Vec<RuntimeContextMessage>> {
    match &run.kind {
        AgentRunKind::Chat(chat) if !chat.replayed_turn_contexts.is_empty() => {
            Some(chat.replayed_turn_contexts.clone())
        }
        AgentRunKind::Chat(_) => None,
        AgentRunKind::Task(_) | AgentRunKind::FollowUp(_) | AgentRunKind::Gate(_) => None,
    }
}

fn existing_session_contexts(run: &AgentRun) -> Option<Vec<RuntimeContextMessage>> {
    let history = match &run.kind {
        AgentRunKind::Chat(chat) => &chat.history,
        AgentRunKind::FollowUp(follow_up) => &follow_up.history,
        AgentRunKind::Task(_) | AgentRunKind::Gate(_) => return None,
    };
    let mut control = None;
    let mut data = None;
    for message in history.iter().rev() {
        let ConversationMessage::RuntimeContext(context) = message else {
            continue;
        };
        if context.scope() != RuntimeContextScope::Session {
            continue;
        }
        match context.authority() {
            RuntimeContextAuthority::Control if control.is_none() => {
                control = Some(context.clone());
            }
            RuntimeContextAuthority::Data if data.is_none() => data = Some(context.clone()),
            RuntimeContextAuthority::Control | RuntimeContextAuthority::Data => {}
        }
        if control.is_some() && data.is_some() {
            break;
        }
    }
    let messages = control.into_iter().chain(data).collect::<Vec<_>>();
    (!messages.is_empty()).then_some(messages)
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

fn validate_static_prompt_sources<'a>(
    prompt_sources: impl IntoIterator<Item = &'a str>,
    context_selectors: Vec<String>,
) -> anyhow::Result<()> {
    let mut selectors = prompt_sources
        .into_iter()
        .flat_map(crate::context::renderer::scan_runtime_selectors)
        .chain(context_selectors)
        .collect::<Vec<_>>();
    selectors.sort();
    selectors.dedup();
    if selectors.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "static prompts cannot reference runtime-owned selectors: {}",
        selectors.join(", ")
    )
}

fn native_model_tool_shadows_local_tool(
    native_tools: &[NativeModelToolId],
    local_tool_name: &str,
) -> bool {
    native_tools
        .iter()
        .any(|tool| tool.as_str() == "web_search")
        && local_tool_name == "search_web"
}

fn sort_tool_specs(specs: &mut [ToolSpec]) {
    specs.sort_by(|left, right| left.name.cmp(&right.name));
}

fn populate_project_working_directory(project: &mut ProjectContext, workspace_dir: &Path) {
    if !project.slug.is_empty() && project.working_dir.is_empty() {
        project.working_dir = workspace_dir
            .join(&project.slug)
            .to_string_lossy()
            .into_owned();
    }
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
    fn xai_native_web_search_shadows_local_search_web() {
        assert!(native_model_tool_shadows_local_tool(
            &[NativeModelToolId::from("web_search")],
            "search_web",
        ));
        assert!(!native_model_tool_shadows_local_tool(
            &[NativeModelToolId::from("x_search")],
            "search_web",
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

    #[test]
    fn prompt_context_preserves_an_explicit_worktree_directory() {
        let mut project = ProjectContext {
            slug: "project".to_string(),
            working_dir: "/workspace/project/worktrees/task-123".to_string(),
            ..ProjectContext::default()
        };

        populate_project_working_directory(&mut project, Path::new("/workspace"));

        assert_eq!(project.working_dir, "/workspace/project/worktrees/task-123");
    }

    #[test]
    fn prompt_context_derives_a_project_directory_without_an_explicit_scope() {
        let mut project = ProjectContext {
            slug: "project".to_string(),
            ..ProjectContext::default()
        };

        populate_project_working_directory(&mut project, Path::new("/workspace"));

        assert_eq!(project.working_dir, "/workspace/project");
    }
}
