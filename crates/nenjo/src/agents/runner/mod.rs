//! AgentRunner — executes agent tasks through the turn loop.
pub(crate) mod turn_loop;
pub mod types;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use nenjo_models::ChatMessage;
use tracing::{debug, info};
use uuid::Uuid;

use super::abilities::AbilityTool;
use super::delegation::DelegateToTool;
use super::instance::{AgentInstance, build_document_listing};
use anyhow::Context;

use crate::config::AgentConfig;
use crate::manifest::Manifest;
use crate::memory::{self, Memory, MemoryScope};
use crate::provider::{ModelProviderFactory, ToolFactory};
use crate::routines::LambdaRunner;
use crate::types::{ActiveDomain, DelegationContext, DomainSessionManifest, TaskType};
use types::{TurnEvent, TurnOutput};

/// Handle to a running agent execution.
///
/// Provides a stream of [`TurnEvent`]s as the agent works, plus access
/// to the final [`TurnOutput`] when done.
pub struct ExecutionHandle {
    events_rx: mpsc::UnboundedReceiver<TurnEvent>,
    join: tokio::task::JoinHandle<Result<TurnOutput>>,
    pause_token: types::PauseToken,
}

impl ExecutionHandle {
    /// Receive the next event. Returns `None` when the turn loop finishes.
    pub async fn recv(&mut self) -> Option<TurnEvent> {
        self.events_rx.recv().await
    }

    /// Get a mutable reference to the underlying event receiver.
    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<TurnEvent> {
        &mut self.events_rx
    }

    /// Get a clone of the pause token for external control (e.g. execution registry).
    pub fn pause_token(&self) -> types::PauseToken {
        self.pause_token.clone()
    }

    /// Abort the running execution. The spawned task is cancelled immediately.
    pub fn abort(&self) {
        self.join.abort();
    }

    /// Pause execution. The turn loop will block before the next LLM call.
    ///
    /// The caller will receive a `TurnEvent::Paused` event once the loop
    /// reaches the pause point. In-flight tool executions finish first.
    pub fn pause(&self) {
        self.pause_token.pause();
    }

    /// Resume a paused execution. The turn loop continues from where it stopped.
    ///
    /// The caller will receive a `TurnEvent::Resumed` event.
    pub fn resume(&self) {
        self.pause_token.resume();
    }

    /// Check if the execution is currently paused.
    pub fn is_paused(&self) -> bool {
        self.pause_token.is_paused()
    }

    /// Wait for the final output.
    pub async fn output(self) -> Result<TurnOutput> {
        self.join
            .await
            .map_err(|e| anyhow::anyhow!("execution task panicked: {e}"))?
    }
}

/// Factory Arcs needed to construct DelegateToTool in the runner.
///
/// Passed from AgentBuilder when the Provider sets up delegation support.
pub struct DelegationSupport {
    pub manifest: Arc<Manifest>,
    pub model_factory: Arc<dyn ModelProviderFactory>,
    pub tool_factory: Arc<dyn ToolFactory>,
    pub memory: Option<Arc<dyn Memory>>,
    pub agent_config: AgentConfig,
    pub lambda_runner: Option<Arc<dyn LambdaRunner>>,
    pub platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
    /// Pre-built delegation context from a parent delegation. When set,
    /// the runner uses this instead of creating a fresh one — this is how
    /// depth decrements across nested delegations.
    pub delegation_ctx: Option<DelegationContext>,
}

/// Wraps an [`AgentInstance`] and provides the execution API.
///
/// Created via [`AgentBuilder::build()`](super::builder::AgentBuilder::build).
pub struct AgentRunner {
    instance: Arc<AgentInstance>,
    memory: Option<Arc<dyn Memory>>,
    memory_scope: Option<MemoryScope>,
    manifest: Option<Arc<Manifest>>,
    platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
}

impl AgentRunner {
    pub(crate) fn new(
        mut instance: AgentInstance,
        memory: Option<Arc<dyn Memory>>,
        memory_scope: Option<MemoryScope>,
        delegation: Option<DelegationSupport>,
    ) -> Result<Self, super::error::AgentError> {
        // Pre-compute documents XML (sync, from disk).
        if instance.documents_xml.is_empty()
            && let Some(ref dir) = instance.prompt_context.docs_base_dir
        {
            let slug = &instance.prompt_context.current_project.slug;
            instance.documents_xml = build_document_listing(dir, slug);
        }

        // Extract manifest and platform resolver before delegation is consumed —
        // stored on the runner so domain_expansion can pass them to sub-runners.
        let manifest = delegation.as_ref().map(|ds| ds.manifest.clone());
        let platform_resolver = delegation
            .as_ref()
            .and_then(|ds| ds.platform_resolver.clone());

        let has_abilities = !instance.prompt_context.available_abilities.is_empty();

        // If the agent has abilities, register each as a dedicated tool.
        // Uses the manifest from DelegationSupport to resolve the ability's
        // MCP servers the same way the Provider does for the base agent.
        if has_abilities {
            let m = manifest
                .clone()
                .ok_or_else(|| super::error::AgentError::MissingManifest(instance.name.clone()))?;
            let base_instance = Arc::new(instance.clone());
            for ability in &instance.prompt_context.available_abilities {
                let tool = AbilityTool::new(
                    ability.clone(),
                    base_instance.clone(),
                    m.clone(),
                    platform_resolver.clone(),
                );
                instance.tools.push(Arc::new(tool));
            }
        }

        // If delegation is enabled (other agents exist + max_depth > 0), add delegate_to.
        if let Some(ds) = delegation {
            let other_agents = instance
                .prompt_context
                .available_agents
                .iter()
                .any(|a| Some(a.id) != instance.agent_id);

            // Use pre-built context from parent delegation, or create fresh.
            let ctx = ds
                .delegation_ctx
                .unwrap_or_else(|| DelegationContext::new(ds.agent_config.max_delegation_depth));

            // Only inject if there's remaining depth and other agents exist.
            if other_agents && ctx.max_depth > ctx.current_depth {
                let delegate_tool = DelegateToTool::new(super::delegation::DelegateToToolParams {
                    manifest: ds.manifest,
                    model_factory: ds.model_factory,
                    tool_factory: ds.tool_factory,
                    memory: ds.memory,
                    agent_config: ds.agent_config,
                    lambda_runner: ds.lambda_runner,
                    platform_resolver: ds.platform_resolver,
                    caller_agent_id: instance.agent_id.unwrap_or_else(Uuid::nil),
                    delegation_ctx: ctx,
                });
                instance.tools.push(Arc::new(delegate_tool));
            }
        }

        let instance = Arc::new(instance);

        Ok(Self {
            instance,
            memory,
            memory_scope,
            manifest,
            platform_resolver,
        })
    }

    /// Read-only access to the underlying agent instance.
    pub fn instance(&self) -> &AgentInstance {
        &self.instance
    }

    /// The agent's name.
    pub fn agent_name(&self) -> &str {
        &self.instance.name
    }

    /// The agent's ID, if it was created from a manifest.
    pub fn agent_id(&self) -> Option<Uuid> {
        self.instance.agent_id
    }

    /// Create a runner from a pre-built instance.
    ///
    /// Used by the harness to re-use a domain-expanded instance across
    /// multiple chat turns without rebuilding from the Provider each time.
    /// Pass memory/scope to preserve memory and resource loading.
    pub fn from_instance(
        instance: AgentInstance,
        memory: Option<Arc<dyn Memory>>,
        memory_scope: Option<MemoryScope>,
    ) -> Self {
        Self {
            instance: Arc::new(instance),
            memory,
            memory_scope,
            manifest: None,
            platform_resolver: None,
        }
    }

    /// The memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.memory.as_ref()
    }

    /// The memory scope, if configured.
    pub fn memory_scope(&self) -> Option<&MemoryScope> {
        self.memory_scope.as_ref()
    }

    /// Activate a domain by name, returning a new runner with expanded config.
    ///
    /// The domain is looked up from the agent's assigned domains. The returned
    /// runner has:
    /// - The domain's `system_addon` appended to the developer prompt
    /// - Tools filtered/expanded per the domain's `DomainToolConfig`
    /// - Domain context (guidelines, artifact schema) injected into prompts
    /// - A fresh session with turn counter at 0
    ///
    /// ```ignore
    /// let domain_runner = runner.domain_expansion("prd")?;
    /// let output = domain_runner.chat("Create a PRD for auth").await?;
    /// ```
    pub async fn domain_expansion(&self, domain_name: &str) -> Result<AgentRunner> {
        let domain = self
            .instance
            .prompt_context
            .available_domains
            .iter()
            .find(|d| d.name == domain_name || d.command == domain_name)
            .with_context(|| {
                let available: Vec<&str> = self
                    .instance
                    .prompt_context
                    .available_domains
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect();
                format!("domain '{domain_name}' not found. Available: {available:?}")
            })?;

        // Parse the domain's manifest JSON into the session config.
        let session_manifest: DomainSessionManifest =
            serde_json::from_value(domain.manifest.clone())
                .with_context(|| format!("failed to parse manifest for domain '{domain_name}'"))?;

        // Build the active domain session state.
        let active_domain = ActiveDomain {
            session_id: Uuid::new_v4(),
            domain_id: domain.id,
            domain_name: domain.name.clone(),
            manifest: session_manifest.clone(),
            turn_number: 0,
            artifact_draft: serde_json::Value::Object(Default::default()),
        };

        // Clone the instance and apply domain expansion.
        // Domains are additive — they add context, scopes, tools, and abilities.
        let mut instance = (*self.instance).clone();
        instance.prompt_context.active_domain = Some(active_domain);

        info!(
            agent = instance.name,
            domain = domain_name,
            session_id = %instance.prompt_context.active_domain.as_ref().unwrap().session_id,
            "Domain expansion started"
        );

        let tool_config = &session_manifest.tools;

        // Merge additional_scopes into the agent's platform_scopes.
        if !tool_config.additional_scopes.is_empty() {
            debug!(
                agent = instance.name,
                domain = domain_name,
                scopes = ?tool_config.additional_scopes,
                "Merging domain scopes"
            );
        }
        for scope in &tool_config.additional_scopes {
            if !instance.prompt_context.platform_scopes.contains(scope) {
                instance.prompt_context.platform_scopes.push(scope.clone());
            }
        }

        // Resolve and add platform tools for the expanded scopes.
        if !tool_config.additional_scopes.is_empty()
            && let Some(ref resolver) = self.platform_resolver
        {
            let scope_tools = resolver.resolve_tools(&tool_config.additional_scopes).await;
            for tool in scope_tools {
                let name = tool.name().to_string();
                if !instance.tools.iter().any(|t| t.name() == name) {
                    instance.tools.push(tool);
                }
            }
        }

        // Activate abilities listed in the domain config.
        if !tool_config.activate_abilities.is_empty() {
            debug!(
                agent = instance.name,
                domain = domain_name,
                abilities = ?tool_config.activate_abilities,
                "Activating domain abilities"
            );
            if let Some(ref manifest) = self.manifest {
                for ability_name in &tool_config.activate_abilities {
                    if let Some(ability) =
                        manifest.abilities.iter().find(|a| a.name == *ability_name)
                        && !instance
                            .prompt_context
                            .available_abilities
                            .iter()
                            .any(|a| a.id == ability.id)
                    {
                        instance
                            .prompt_context
                            .available_abilities
                            .push(ability.clone());
                    }
                }
            }
        }

        // If abilities are now available (either pre-existing or domain-activated),
        // register any missing dedicated ability tools.
        let has_abilities = !instance.prompt_context.available_abilities.is_empty();
        if has_abilities && let Some(ref m) = self.manifest {
            let base_instance = Arc::new(instance.clone());
            for ability in &instance.prompt_context.available_abilities {
                let tool_name = super::abilities::ability_tool_name(ability);
                if !instance.tools.iter().any(|t| t.name() == tool_name) {
                    let tool = AbilityTool::new(
                        ability.clone(),
                        base_instance.clone(),
                        m.clone(),
                        self.platform_resolver.clone(),
                    );
                    instance.tools.push(Arc::new(tool));
                }
            }
        }

        Ok(Self {
            instance: Arc::new(instance),
            memory: self.memory.clone(),
            memory_scope: self.memory_scope.clone(),
            manifest: self.manifest.clone(),
            platform_resolver: self.platform_resolver.clone(),
        })
    }

    /// Send a chat message and stream events as the agent works.
    pub async fn chat_stream(&self, message: &str) -> Result<ExecutionHandle> {
        self.chat_with_history_stream(message, Vec::new()).await
    }

    /// Send a chat message with prior conversation history and stream events.
    pub async fn chat_with_history_stream(
        &self,
        message: &str,
        history: Vec<ChatMessage>,
    ) -> Result<ExecutionHandle> {
        let task = TaskType::Chat {
            user_message: message.to_string(),
            history,
            project_id: Uuid::nil(),
        };
        self.execute_stream(task).await
    }

    /// Execute a task and stream events as the agent works.
    pub async fn task_stream(&self, task: TaskType) -> Result<ExecutionHandle> {
        self.execute_stream(task).await
    }

    /// Send a chat message and wait for the final output.
    pub async fn chat(&self, message: &str) -> Result<TurnOutput> {
        self.chat_stream(message).await?.output().await
    }

    /// Send a chat message with prior conversation history and wait for the final output.
    pub async fn chat_with_history(
        &self,
        message: &str,
        history: Vec<ChatMessage>,
    ) -> Result<TurnOutput> {
        self.chat_with_history_stream(message, history)
            .await?
            .output()
            .await
    }

    /// Execute a task and wait for the final output.
    pub async fn task(&self, task: TaskType) -> Result<TurnOutput> {
        self.task_stream(task).await?.output().await
    }

    // -- Internal --

    async fn execute_stream(&self, task: TaskType) -> Result<ExecutionHandle> {
        // Load memory + resource vars if configured (async).
        let (memory_vars, resource_vars) =
            if let (Some(mem), Some(scope)) = (&self.memory, &self.memory_scope) {
                let mv = if self.instance.memory_vars.is_empty() {
                    memory::build_memory_vars(mem.as_ref(), scope).await?
                } else {
                    self.instance.memory_vars.clone()
                };
                let rv = if self.instance.resource_vars.is_empty() {
                    memory::build_resource_vars(mem.as_ref(), scope).await?
                } else {
                    self.instance.resource_vars.clone()
                };
                (mv, rv)
            } else {
                (
                    self.instance.memory_vars.clone(),
                    self.instance.resource_vars.clone(),
                )
            };

        // Temporarily set vars on instance for prompt building.
        let needs_clone = (!memory_vars.is_empty() && self.instance.memory_vars.is_empty())
            || (!resource_vars.is_empty() && self.instance.resource_vars.is_empty());
        let inst = if needs_clone {
            let mut cloned = (*self.instance).clone();
            cloned.memory_vars = memory_vars;
            cloned.resource_vars = resource_vars;
            Arc::new(cloned)
        } else {
            self.instance.clone()
        };

        // 3. Build prompts.
        let prompts = inst.build_prompts(&task);

        let task_label = match &task {
            TaskType::Chat { .. } => "chat",
            TaskType::Task(_) => "task",
            TaskType::Cron { .. } => "cron",
            TaskType::Gate { .. } => "gate",
            TaskType::CouncilSubtask { .. } => "council_subtask",
        };
        let domain_label = inst
            .prompt_context
            .active_domain
            .as_ref()
            .map(|d| d.domain_name.as_str());

        info!(
            agent = inst.name,
            model = inst.model.as_str(),
            task_type = task_label,
            domain = ?domain_label,
            tool_count = inst.tools.len(),
            "Executing agent"
        );

        debug!(
            agent = inst.name,
            "--- System Prompt ---\n{}\n--- Developer Prompt ---\n{}\n--- User Message ---\n{}",
            prompts.system,
            prompts.developer,
            prompts.user_message,
        );

        // 4. Build initial messages.
        let mut messages: Vec<ChatMessage> = Vec::new();

        if inst.provider.supports_developer_role(&inst.model) && !prompts.developer.is_empty() {
            messages.push(ChatMessage::system(&prompts.system));
            messages.push(ChatMessage::developer(&prompts.developer));
        } else {
            let combined = if prompts.developer.is_empty() {
                prompts.system
            } else {
                format!("{}\n\n{}", prompts.system, prompts.developer)
            };
            messages.push(ChatMessage::system(&combined));
        }

        if let TaskType::Chat { ref history, .. } = task {
            for msg in history {
                messages.push(msg.clone());
            }
        }

        let user_message = if !prompts.user_message.is_empty() {
            prompts.user_message
        } else {
            // Template rendered empty — fall back to the raw task content.
            match &task {
                TaskType::Chat { user_message, .. } => user_message.clone(),
                TaskType::Task(t) => {
                    if t.description.is_empty() {
                        t.title.clone()
                    } else {
                        t.description.clone()
                    }
                }
                TaskType::Cron { task: Some(t), .. } => {
                    if t.description.is_empty() {
                        t.title.clone()
                    } else {
                        t.description.clone()
                    }
                }
                TaskType::Cron { task: None, .. } => String::new(),
                TaskType::Gate { criteria, .. } => criteria.clone(),
                TaskType::CouncilSubtask {
                    subtask_description,
                    ..
                } => subtask_description.clone(),
            }
        };

        if !user_message.is_empty() {
            debug!(agent = inst.name, user_message = %user_message, "Agent user message");
            messages.push(ChatMessage::user(&user_message));
        }

        // 5. Spawn the turn loop.
        let (events_tx, events_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let pause_token = types::PauseToken::new();
        let loop_pause = pause_token.clone();

        let join = tokio::spawn(async move {
            turn_loop::run(&inst, messages, Some(events_tx), Some(loop_pause)).await
        });

        Ok(ExecutionHandle {
            events_rx,
            join,
            pause_token,
        })
    }
}
