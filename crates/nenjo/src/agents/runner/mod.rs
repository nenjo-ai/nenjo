//! AgentRunner — executes agent tasks through the turn loop.
pub(crate) mod compaction;
pub(crate) mod turn_loop;
pub mod types;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use nenjo_models::ChatMessage;
use tracing::{info, trace};
use uuid::Uuid;

use super::abilities::{build_ability_tools, is_ability_tool};
use super::delegation::DelegateToTool;
use anyhow::Context;
use nenjo_models::ModelProvider;

use super::instance::AgentInstance;
use crate::input::{AgentRun, AgentRunKind, ChatInput, TaskInput};
use crate::manifest::{AbilityManifest, DomainManifest, Manifest};
use crate::memory::{self, MemoryScope};
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::types::{ActiveDomain, DelegationContext};
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

/// Provider handle needed to construct DelegateToTool in the runner.
///
/// Passed from AgentBuilder when the Provider sets up delegation support.
pub(crate) struct DelegationSupport<P: ProviderRuntime = ErasedProvider> {
    pub(crate) provider: P,
    pub(crate) max_delegation_depth: u32,
    /// Pre-built delegation context from a parent delegation. When set,
    /// the runner uses this instead of creating a fresh one — this is how
    /// depth decrements across nested delegations.
    pub(crate) delegation_ctx: Option<DelegationContext>,
}

/// Wraps an [`AgentInstance`] and provides the execution API.
///
/// Created via [`AgentBuilder::build()`](super::builder::AgentBuilder::build).
pub struct AgentRunner<P: ProviderRuntime = ErasedProvider> {
    instance: Arc<AgentInstance<P>>,
    memory: Option<Arc<P::Memory<'static>>>,
    memory_scope: Option<MemoryScope>,
    manifest: Option<Arc<Manifest>>,
}

impl<P: ProviderRuntime> AgentRunner<P> {
    pub(crate) async fn new(
        mut instance: AgentInstance<P>,
        memory: Option<Arc<P::Memory<'static>>>,
        memory_scope: Option<MemoryScope>,
        delegation: Option<DelegationSupport<P>>,
    ) -> Result<Self, super::error::AgentError> {
        // Extract manifest before delegation is consumed so domain_expansion can
        // pass it to sub-runners.
        let manifest = delegation
            .as_ref()
            .map(|ds| ds.provider.manifest_snapshot());

        // If the agent has abilities, register one tool per assigned ability
        // resolved from the canonical manifest.
        if let Some(active_abilities) = manifest
            .as_ref()
            .map(|manifest| resolve_active_abilities(manifest, Some(instance.agent_id()), None))
            .filter(|abilities| !abilities.is_empty())
        {
            let m = manifest
                .clone()
                .ok_or_else(|| super::error::AgentError::MissingManifest(instance.name().into()))?;
            let base_instance = Arc::new(instance.clone());
            instance
                .runtime
                .tools
                .extend(build_ability_tools(&active_abilities, base_instance, m));
        }

        // If delegation is enabled (other agents exist + max_depth > 0), add delegate_to.
        if let Some(ds) = delegation {
            let other_agents = instance
                .prompt
                .context
                .available_agents
                .iter()
                .any(|a| a.id != instance.agent_id());

            // Use pre-built context from parent delegation, or create fresh.
            let ctx = ds
                .delegation_ctx
                .unwrap_or_else(|| DelegationContext::new(ds.max_delegation_depth));

            // Only inject if there's remaining depth and other agents exist.
            if other_agents && ctx.max_depth > ctx.current_depth {
                let delegate_tool = DelegateToTool::new(super::delegation::DelegateToToolParams {
                    provider: ds.provider,
                    caller_agent_id: instance.agent_id(),
                    delegation_ctx: ctx,
                });
                instance.runtime.tools.push(Arc::new(delegate_tool));
            }
        }

        let instance = Arc::new(instance);

        Ok(Self {
            instance,
            memory,
            memory_scope,
            manifest,
        })
    }

    /// Read-only access to the underlying agent instance.
    pub fn instance(&self) -> &AgentInstance<P> {
        &self.instance
    }

    /// The agent's name.
    pub fn agent_name(&self) -> &str {
        self.instance.name()
    }

    /// The agent's manifest ID.
    pub fn agent_id(&self) -> Uuid {
        self.instance.agent_id()
    }

    /// Create a runner from a pre-built instance.
    ///
    /// Used by the harness to re-use a domain-expanded instance across
    /// multiple chat turns without rebuilding from the Provider each time.
    /// Pass memory/scope to preserve memory and artifact loading.
    pub fn from_instance(
        instance: AgentInstance<P>,
        memory: Option<Arc<P::Memory<'static>>>,
        memory_scope: Option<MemoryScope>,
    ) -> Self {
        Self {
            instance: Arc::new(instance),
            memory,
            memory_scope,
            manifest: None,
        }
    }

    /// The memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<P::Memory<'static>>> {
        self.memory.as_ref()
    }

    /// The memory scope, if configured.
    pub fn memory_scope(&self) -> Option<&MemoryScope> {
        self.memory_scope.as_ref()
    }

    /// Activate a domain by name, returning a new runner with expanded config.
    ///
    /// The domain is looked up from the agent's assigned domains. The returned
    /// runner appends the domain's `system_addon` to the developer prompt and
    /// layers in any domain-scoped ability, scope, and MCP activations.
    ///
    /// ```ignore
    /// let domain_runner = runner.domain_expansion("prd")?;
    /// let output = domain_runner.chat("Create a PRD for auth").await?;
    /// ```
    pub async fn domain_expansion(&self, domain_name: &str) -> Result<AgentRunner<P>> {
        let domain = self
            .instance
            .prompt
            .context
            .available_domains
            .iter()
            .find(|d| d.name == domain_name || d.command == domain_name)
            .with_context(|| {
                let available: Vec<&str> = self
                    .instance
                    .prompt
                    .context
                    .available_domains
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect();
                format!("domain '{domain_name}' not found. Available: {available:?}")
            })?;

        let session_manifest: DomainManifest = domain.clone();

        // Build the active domain session state.
        let active_domain = ActiveDomain {
            session_id: Uuid::new_v4(),
            domain_id: domain.id,
            domain_name: domain.name.clone(),
            manifest: session_manifest.clone(),
        };

        // Clone the instance and apply domain expansion.
        let mut instance = (*self.instance).clone();
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| super::error::AgentError::MissingManifest(instance.name().into()))?;

        merge_domain_scopes(
            &mut instance.prompt.context.platform_scopes,
            &session_manifest.platform_scopes,
        );
        merge_domain_abilities(
            &mut instance.prompt.context.available_abilities,
            manifest,
            &session_manifest.ability_ids,
        );
        merge_domain_mcp_servers(
            &mut instance.prompt.context.mcp_server_info,
            manifest,
            &session_manifest.mcp_server_ids,
        );
        instance.prompt.context.active_domain = Some(active_domain);

        let active_abilities =
            resolve_active_abilities(manifest, Some(instance.agent_id()), Some(&session_manifest));

        // Rebuild assigned ability tools so the visible set matches the
        // effective domain-expanded ability assignments.
        instance
            .runtime
            .tools
            .retain(|tool| !is_ability_tool(tool.name(), &active_abilities));
        if !active_abilities.is_empty() {
            let base_instance = Arc::new(instance.clone());
            instance.runtime.tools.extend(build_ability_tools(
                &active_abilities,
                base_instance,
                manifest.clone(),
            ));
        }

        info!(
            agent = instance.name(),
            domain = domain_name,
            session_id = %instance.prompt.context.active_domain.as_ref().unwrap().session_id,
            "Domain expansion started"
        );

        Ok(Self {
            instance: Arc::new(instance),
            memory: self.memory.clone(),
            memory_scope: self.memory_scope.clone(),
            manifest: self.manifest.clone(),
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
        self.run_stream(AgentRun::chat(ChatInput {
            message: message.to_string(),
            history,
            project_id: None,
        }))
        .await
    }

    /// Execute a task and stream events as the agent works.
    pub async fn task_stream(&self, task: TaskInput) -> Result<ExecutionHandle> {
        self.run_stream(AgentRun::task(task)).await
    }

    /// Execute a composed agent run and stream events as the agent works.
    pub async fn run_stream(&self, run: AgentRun) -> Result<ExecutionHandle> {
        self.execute_stream(run).await
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
    pub async fn task(&self, task: TaskInput) -> Result<TurnOutput> {
        self.task_stream(task).await?.output().await
    }

    /// Execute a composed agent run and wait for the final output.
    pub async fn run(&self, run: AgentRun) -> Result<TurnOutput> {
        self.run_stream(run).await?.output().await
    }

    // -- Internal --

    async fn execute_stream(&self, run: AgentRun) -> Result<ExecutionHandle> {
        let memory_vars = if let (Some(mem), Some(scope)) = (&self.memory, &self.memory_scope)
            && self.instance.prompt.memory_vars.is_empty()
        {
            Some(memory::build_memory_vars(mem.as_ref(), scope).await?)
        } else {
            None
        };
        let artifact_vars = if let (Some(mem), Some(scope)) = (&self.memory, &self.memory_scope)
            && self.instance.prompt.artifact_vars.is_empty()
        {
            Some(memory::build_artifact_vars(mem.as_ref(), scope).await?)
        } else {
            None
        };

        let inst = self.instance.clone();

        // 3. Build prompts.
        let prompts =
            inst.build_prompts_with_vars(&run, memory_vars.as_ref(), artifact_vars.as_ref());

        let task_label = match &run.kind {
            AgentRunKind::Chat { .. } => "chat",
            AgentRunKind::Task(_) => "task",
            AgentRunKind::Cron { .. } => "cron",
            AgentRunKind::Heartbeat { .. } => "heartbeat",
            AgentRunKind::Gate { .. } => "gate",
            AgentRunKind::CouncilSubtask { .. } => "council_subtask",
        };
        let domain_label = inst
            .prompt
            .context
            .active_domain
            .as_ref()
            .map(|d| d.domain_name.as_str());

        info!(
            agent = inst.name(),
            model = inst.model.model_name.as_str(),
            task_type = task_label,
            domain = ?domain_label,
            tool_count = inst.runtime.tools.len(),
            "Executing agent"
        );

        let system_prompt = prompts.system;
        let developer_prompt = prompts.developer;
        let templated_user_message = prompts.user_message;
        trace!(
            agent = inst.name(),
            "\nRendered prompts for {}\n\n=== System Prompt ===\n{}\n\n=== Developer Prompt ===\n{}\n\n=== User Message ===\n{}",
            inst.name(),
            system_prompt,
            developer_prompt,
            templated_user_message,
        );

        // 4. Build initial messages.
        let mut messages: Vec<ChatMessage> = Vec::new();

        if inst
            .model
            .model_provider
            .supports_developer_role(&inst.model.model_name)
            && !developer_prompt.is_empty()
        {
            messages.push(ChatMessage::system(&system_prompt));
            messages.push(ChatMessage::developer(&developer_prompt));
        } else {
            let combined = if developer_prompt.is_empty() {
                system_prompt.clone()
            } else {
                format!("{}\n\n{}", system_prompt, developer_prompt)
            };
            messages.push(ChatMessage::system(&combined));
        }

        if let AgentRunKind::Chat(ref chat) = run.kind {
            for msg in &chat.history {
                messages.push(msg.clone());
            }
        }

        let user_message = if !templated_user_message.is_empty() {
            templated_user_message
        } else {
            // Template rendered empty — fall back to the raw task content.
            match &run.kind {
                AgentRunKind::Chat(chat) => chat.message.clone(),
                AgentRunKind::Task(task) => {
                    if task.description.is_empty() {
                        task.title.clone()
                    } else {
                        task.description.clone()
                    }
                }
                AgentRunKind::Cron(crate::input::CronInput {
                    task: Some(task), ..
                }) => {
                    if task.description.is_empty() {
                        task.title.clone()
                    } else {
                        task.description.clone()
                    }
                }
                AgentRunKind::Cron(_) => String::new(),
                AgentRunKind::Heartbeat(_) => String::new(),
                AgentRunKind::Gate(gate) => gate.criteria.clone(),
                AgentRunKind::CouncilSubtask(subtask) => subtask.subtask_description.clone(),
            }
        };
        messages.push(ChatMessage::user(&user_message));

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

fn resolve_active_abilities(
    manifest: &Manifest,
    agent_id: Option<Uuid>,
    active_domain: Option<&DomainManifest>,
) -> Vec<AbilityManifest> {
    let mut ability_ids = Vec::new();

    if let Some(agent_id) = agent_id
        && let Some(agent) = manifest.agents.iter().find(|agent| agent.id == agent_id)
    {
        ability_ids.extend(agent.ability_ids.iter().copied());
    }

    if let Some(domain) = active_domain {
        for ability_id in &domain.ability_ids {
            if !ability_ids.contains(ability_id) {
                ability_ids.push(*ability_id);
            }
        }
    }

    ability_ids
        .into_iter()
        .filter_map(|ability_id| {
            manifest
                .abilities
                .iter()
                .find(|ability| ability.id == ability_id)
                .cloned()
        })
        .collect()
}

fn merge_domain_scopes(target: &mut Vec<String>, additional_scopes: &[String]) {
    for scope in additional_scopes {
        if !target.iter().any(|existing| existing == scope) {
            target.push(scope.clone());
        }
    }
}

fn merge_domain_abilities(
    target: &mut Vec<AbilityManifest>,
    manifest: &Manifest,
    activated_ids: &[Uuid],
) {
    for ability_id in activated_ids {
        if let Some(ability) = manifest
            .abilities
            .iter()
            .find(|candidate| &candidate.id == ability_id)
            .cloned()
            && !target.iter().any(|existing| existing.id == ability.id)
        {
            target.push(ability);
        }
    }
}

fn merge_domain_mcp_servers(
    target: &mut Vec<(String, String)>,
    manifest: &Manifest,
    activated_ids: &[Uuid],
) {
    for server_id in activated_ids {
        if let Some(server) = manifest
            .mcp_servers
            .iter()
            .find(|candidate| &candidate.id == server_id)
        {
            let entry = (
                server.display_name.clone(),
                server.description.clone().unwrap_or_default(),
            );
            if !target.iter().any(|existing| existing.0 == entry.0) {
                target.push(entry);
            }
        }
    }
}
