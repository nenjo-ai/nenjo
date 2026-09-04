//! AgentRunner — executes agent tasks through the turn loop.
pub mod chat;
pub(crate) mod compaction;
mod tool_calls;
pub(crate) mod turn_loop;
pub mod types;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use nenjo_models::{
    ArtifactInput, ArtifactInputSource, ChatMessage, ConversationMessage, RuntimeContextScope,
};
use tracing::{debug, info, trace};
use uuid::Uuid;

use super::abilities::{build_ability_tools, build_async_operation_tools, is_ability_tool};
use super::async_ops::AsyncOpChildHandle;
use super::delegation::{DELEGATE_TO_TOOL_NAME, build_delegation_tools, delegation_child_tools};
use super::sub_agents::{
    ChildRuntimeHandle, PARENT_TOOL_NAMES, SubAgentLimits, SubAgentRuntime, SubAgentRuntimeOptions,
    child_tools, parent_tools,
};
use anyhow::Context;
use nenjo_models::ModelProvider;

use super::instance::AgentInstance;
use crate::Slug;
use crate::input::{AgentRun, AgentRunKind, ChatInput, TaskInput};
use crate::manifest::{AbilityManifest, DomainManifest, Manifest};
use crate::memory::{self, MemoryScope};
use crate::provider::{
    ErasedProvider, ProviderRuntime, ToolContext, ToolFactory, knowledge_read_scope_granted,
};
use crate::types::ActiveDomain;
use chat::{ChatDelivery, ChatHandle, ProviderResponseDelivery};
use types::{TurnEvent, TurnOutput};

/// Handle to a running agent execution.
///
/// Provides a stream of [`TurnEvent`]s as the agent works, plus access
/// to the final [`TurnOutput`] when done.
pub struct ExecutionHandle {
    events_rx: mpsc::UnboundedReceiver<TurnEvent>,
    join: Option<tokio::task::JoinHandle<Result<TurnOutput>>>,
    pause_token: types::PauseToken,
    turn_input: types::TurnInputSender,
    cancel: CancellationToken,
}

enum ParentHandle<P: ProviderRuntime> {
    EphemeralSubAgent(ChildRuntimeHandle<P>),
    Delegation(AsyncOpChildHandle),
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

    pub fn turn_input(&self) -> types::TurnInputSender {
        self.turn_input.clone()
    }

    /// Request cooperative cancellation of the running execution.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Abort the running execution after signalling cooperative cancellation.
    pub fn abort(&self) {
        self.cancel();
        if let Some(join) = &self.join {
            join.abort();
        }
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
    pub async fn output(mut self) -> Result<TurnOutput> {
        let Some(join) = self.join.take() else {
            return Err(anyhow::anyhow!("execution output was already taken"));
        };
        join.await
            .map_err(|e| anyhow::anyhow!("execution task panicked: {e}"))?
    }
}

impl Drop for ExecutionHandle {
    fn drop(&mut self) {
        if let Some(join) = &self.join
            && !join.is_finished()
        {
            self.cancel.cancel();
            join.abort();
        }
    }
}

pub(crate) fn build_instruction_messages(
    system_prompt: &str,
    developer_prompt: &str,
    supports_developer_role: bool,
) -> Vec<ConversationMessage> {
    let combined = [
        system_prompt,
        developer_prompt,
        crate::context::runtime::RUNTIME_CONTEXT_PROTOCOL,
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");

    if combined.is_empty() {
        Vec::new()
    } else if supports_developer_role {
        vec![ConversationMessage::developer(combined)]
    } else {
        vec![ConversationMessage::system(combined)]
    }
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
    ) -> Result<Self, super::error::AgentError> {
        // Extract manifest before delegation is consumed so domain_expansion can
        // pass it to sub-runners.
        let manifest = instance
            .runtime
            .provider_runtime
            .as_ref()
            .map(|provider| provider.manifest_snapshot());

        // If the agent has abilities, register the ability discovery and
        // invocation broker tools resolved from the canonical manifest.
        if let Some(active_abilities) = instance
            .runtime
            .provider_runtime
            .as_ref()
            .filter(|_| instance.runtime.execution_mode.can_use_abilities())
            .map(|provider| resolve_active_abilities(provider, &instance.manifest, None))
            .filter(|abilities| !abilities.is_empty())
        {
            let base_instance = Arc::new(instance.clone());
            instance
                .runtime
                .tools
                .extend(build_ability_tools(&active_abilities, base_instance)?);
        }
        if instance.runtime.execution_mode.has_own_capability_surface() {
            instance.runtime.tools.extend(build_async_operation_tools(
                instance.runtime.async_ops.clone(),
            ));
        }
        if instance.runtime.execution_mode.can_orchestrate()
            && instance.runtime.provider_runtime.is_some()
            && instance.runtime.config.max_delegation_depth > 0
        {
            let base_instance = Arc::new(instance.clone());
            instance
                .runtime
                .tools
                .extend(build_delegation_tools(base_instance));
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

    /// The agent's manifest slug.
    pub fn agent_slug(&self) -> &crate::Slug {
        self.instance.agent_slug()
    }

    /// Create a runner from a pre-built instance.
    ///
    /// Used by the harness to re-use a domain-expanded instance across
    /// multiple chat turns without rebuilding from the Provider each time.
    /// Pass memory/scope to preserve memory loading.
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
    /// runner appends the domain's prompt addon and layers in any domain-scoped
    /// ability and MCP activations.
    ///
    /// ```ignore
    /// let domain_runner = runner.domain_expansion("prd")?;
    /// let output = domain_runner
    ///     .chat(ChatInput::new("Create a PRD for auth"), Buffered)
    ///     .await?
    ///     .output()
    ///     .await?;
    /// ```
    pub async fn domain_expansion(&self, domain_name: &str) -> Result<AgentRunner<P>> {
        let provider = self
            .instance
            .runtime
            .provider_runtime
            .as_ref()
            .ok_or_else(|| {
                super::error::AgentError::MissingManifest(self.instance.name().into())
            })?;
        let domain_policy = crate::package_resolve::policy_from_agent_metadata(
            self.instance.manifest.source_type.as_deref(),
            Some(&self.instance.manifest.metadata),
        );
        let domain = provider
            .find_domain_with_policy(domain_name, &domain_policy)
            .or_else(|| provider.find_domain(domain_name))
            .filter(|domain| domain_is_assigned(&self.instance.manifest.domains, domain))
            .with_context(|| {
                let available: Vec<&str> = self
                    .instance
                    .manifest
                    .domains
                    .iter()
                    .filter_map(|domain_slug| {
                        provider
                            .find_domain_with_policy(domain_slug.as_str(), &domain_policy)
                            .or_else(|| provider.find_domain(domain_slug.as_str()))
                    })
                    .map(|domain| domain.name.as_str())
                    .collect();
                format!("domain '{domain_name}' not found. Available: {available:?}")
            })?;

        let session_manifest: DomainManifest = domain.clone();

        // Build the active domain session state.
        let active_domain = ActiveDomain {
            session_id: Uuid::new_v4(),
            manifest: session_manifest.clone(),
        };

        // Clone the instance and apply domain expansion.
        let mut instance = (*self.instance).clone();
        instance.prompt.context.active_domain = Some(active_domain);
        extend_unique(
            &mut instance.manifest.platform_scopes,
            &session_manifest.platform_scopes,
        );
        extend_unique(
            &mut instance.manifest.mcp_servers,
            &session_manifest.mcp_servers,
        );
        extend_unique(&mut instance.manifest.media, &session_manifest.media);

        // Re-run host tool construction against the effective domain manifest.
        // Existing tools are retained, while newly authorized scope/MCP tools
        // are added by name.
        if instance.runtime.execution_mode.has_own_capability_surface() {
            let project_slug = active_project_slug(&instance);
            let mut domain_tools = provider
                .tool_factory()
                .create_tools_with_context(
                    &instance.manifest,
                    instance.runtime.security.clone(),
                    ToolContext {
                        project_slug,
                        current_session_id: instance.runtime.current_session_id,
                    },
                )
                .await;
            if knowledge_read_scope_granted(&instance.manifest.platform_scopes) {
                let knowledge_policy = crate::package_resolve::policy_from_agent_metadata(
                    instance.manifest.source_type.as_deref(),
                    Some(&instance.manifest.metadata),
                );
                domain_tools.extend(provider.create_knowledge_tools_with_policy(knowledge_policy));
            }
            let mut tool_names = instance
                .runtime
                .tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect::<std::collections::HashSet<_>>();
            instance.runtime.tools.extend(
                domain_tools
                    .into_iter()
                    .filter(|tool| tool_names.insert(tool.name().to_string())),
            );
        }

        let active_abilities =
            resolve_active_abilities(provider, &instance.manifest, Some(&session_manifest));

        // Rebuild ability broker tools so the visible set matches the
        // effective domain-expanded ability assignments.
        instance
            .runtime
            .tools
            .retain(|tool| !is_ability_tool(tool.name()));
        if !active_abilities.is_empty() {
            let base_instance = Arc::new(instance.clone());
            instance
                .runtime
                .tools
                .extend(build_ability_tools(&active_abilities, base_instance)?);
        }

        let session_id = instance
            .prompt
            .context
            .active_domain
            .as_ref()
            .map(|domain| domain.session_id);
        debug!(
            agent = instance.name(),
            domain = domain_name,
            ?session_id,
            "Domain expansion started"
        );

        Ok(Self {
            instance: Arc::new(instance),
            memory: self.memory.clone(),
            memory_scope: self.memory_scope.clone(),
            manifest: self.manifest.clone(),
        })
    }

    /// Execute one chat turn using the caller-selected response delivery mode.
    pub async fn chat<D: ChatDelivery>(
        &self,
        input: ChatInput,
        _delivery: D,
    ) -> Result<ChatHandle<D>> {
        let handle = self
            .execute_stream(
                AgentRun::chat(input),
                chat::provider_response_delivery::<D>(),
            )
            .await?;
        Ok(ChatHandle::new(handle))
    }

    /// Execute a task and stream events as the agent works.
    pub async fn task_stream(&self, task: TaskInput) -> Result<ExecutionHandle> {
        self.execute_stream(AgentRun::task(task), ProviderResponseDelivery::Buffered)
            .await
    }

    /// Execute a composed agent run and stream events as the agent works.
    pub async fn run_stream(&self, run: AgentRun) -> Result<ExecutionHandle> {
        self.execute_stream(run, ProviderResponseDelivery::Buffered)
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

    async fn execute_stream(
        &self,
        run: AgentRun,
        response_delivery: ProviderResponseDelivery,
    ) -> Result<ExecutionHandle> {
        self.execute_stream_with_parent_handle(run, None, response_delivery)
            .await
    }

    pub(crate) async fn task_stream_as_sub_agent(
        &self,
        task: TaskInput,
        child_handle: ChildRuntimeHandle<P>,
    ) -> Result<ExecutionHandle> {
        self.execute_stream_with_parent_handle(
            AgentRun::task(task),
            Some(ParentHandle::EphemeralSubAgent(child_handle)),
            ProviderResponseDelivery::Buffered,
        )
        .await
    }

    pub(crate) async fn task_stream_as_delegated_agent(
        &self,
        task: TaskInput,
        child_handle: AsyncOpChildHandle,
    ) -> Result<ExecutionHandle> {
        self.execute_stream_with_parent_handle(
            AgentRun::task(task),
            Some(ParentHandle::Delegation(child_handle)),
            ProviderResponseDelivery::Buffered,
        )
        .await
    }

    async fn execute_stream_with_parent_handle(
        &self,
        run: AgentRun,
        parent_handle: Option<ParentHandle<P>>,
        response_delivery: ProviderResponseDelivery,
    ) -> Result<ExecutionHandle> {
        let memory_context = if let (Some(mem), Some(scope)) = (&self.memory, &self.memory_scope)
            && self.instance.prompt.memory_context.is_empty()
            && !run_has_session_context(&run)
        {
            Some(memory::build_memory_context(mem.as_ref(), scope).await?)
        } else {
            None
        };
        // 5. Spawn the turn loop.
        let (events_tx, events_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let pause_token = types::PauseToken::new();
        let loop_pause = pause_token.clone();
        let (turn_input, turn_input_rx) = types::turn_input_channel();

        let mut inst = (*self.instance).clone();
        if let Some(parent_handle) = parent_handle {
            match parent_handle {
                ParentHandle::EphemeralSubAgent(child_handle) => {
                    inst.runtime.tools.extend(child_tools(child_handle));
                }
                ParentHandle::Delegation(child_handle) => {
                    inst.runtime
                        .tools
                        .extend(delegation_child_tools(child_handle));
                }
            }
        } else if inst.runtime.execution_mode.can_orchestrate()
            && let Some(provider) = inst.runtime.provider_runtime.clone()
            && inst.runtime.config.max_delegation_depth > 0
        {
            let inherited_host_tools = inst
                .runtime
                .tools
                .iter()
                .filter(|tool| {
                    !PARENT_TOOL_NAMES.contains(&tool.name())
                        && tool.name() != DELEGATE_TO_TOOL_NAME
                })
                .cloned()
                .collect();
            let runtime = SubAgentRuntime::new(
                provider,
                inst.agent_slug().clone(),
                inst.model_manifest.clone(),
                inherited_host_tools,
                SubAgentRuntimeOptions {
                    limits: SubAgentLimits {
                        max_depth: inst.runtime.config.max_delegation_depth,
                        max_per_spawn: inst.runtime.config.max_sub_agents_per_spawn,
                    },
                    delegation_ctx: inst.runtime.sub_agent_ctx.clone(),
                    async_ops: inst.runtime.async_ops.clone(),
                    events_tx: Some(events_tx.clone()),
                },
            );
            inst.runtime.tools.extend(parent_tools(runtime.handle()));
        }
        let inst = Arc::new(inst);

        let task_label = match &run.kind {
            AgentRunKind::Chat { .. } => "chat",
            AgentRunKind::FollowUp { .. } => "follow_up",
            AgentRunKind::Task(_) => "task",
            AgentRunKind::Gate { .. } => "gate",
        };
        let domain_label = inst
            .prompt
            .context
            .active_domain
            .as_ref()
            .map(|d| d.manifest.name.as_str());

        info!(
            agent = inst.name(),
            model = inst.model.model_name.as_str(),
            task_type = task_label,
            domain = ?domain_label,
            tool_count = inst.runtime.tools.len(),
            "Executing agent"
        );

        // 3. Build prompts.
        let prompts = inst
            .build_prompts_with_memory_context(&run, memory_context.as_deref())
            .context("failed to build prompts")?;

        let user_message = raw_user_message(&run);
        trace!(
            agent = inst.name(),
            system_prompt_len = prompts.system.len(),
            developer_prompt_len = prompts.developer.len(),
            user_message_len = user_message.len(),
            "Built agent prompt plan"
        );

        // 4. Build initial messages.
        let supports_developer_role = inst
            .model
            .model_provider
            .supports_developer_role(&inst.model.model_name);
        let mut messages: Vec<ConversationMessage> = build_instruction_messages(
            &prompts.system,
            &prompts.developer,
            supports_developer_role,
        );

        messages.extend(
            prompts
                .session_context
                .messages()
                .iter()
                .cloned()
                .map(ConversationMessage::runtime_context),
        );

        match &run.kind {
            AgentRunKind::Chat(chat) => messages.extend(
                chat.history
                    .iter()
                    .filter(|message| !is_session_context(message))
                    .cloned(),
            ),
            AgentRunKind::FollowUp(follow_up) => messages.extend(
                follow_up
                    .history
                    .iter()
                    .filter(|message| !is_session_context(message))
                    .cloned(),
            ),
            _ => {}
        }

        messages.extend(
            prompts
                .turn_context
                .messages()
                .iter()
                .cloned()
                .map(ConversationMessage::runtime_context),
        );

        let artifact_inputs = match &run.kind {
            AgentRunKind::Chat(chat) => chat
                .artifacts
                .iter()
                .cloned()
                .map(|artifact| ArtifactInput::new(artifact, ArtifactInputSource::UserAttachment))
                .collect(),
            AgentRunKind::Task(task) => task
                .artifacts
                .iter()
                .cloned()
                .map(|artifact| ArtifactInput::new(artifact, ArtifactInputSource::TaskInput))
                .collect(),
            AgentRunKind::Gate(crate::input::GateInput {
                task: Some(task), ..
            }) => task
                .artifacts
                .iter()
                .cloned()
                .map(|artifact| ArtifactInput::new(artifact, ArtifactInputSource::TaskInput))
                .collect(),
            AgentRunKind::FollowUp(_) | AgentRunKind::Gate(_) => Vec::new(),
        };
        messages.push(ConversationMessage::chat(
            ChatMessage::user(&user_message).with_artifacts(artifact_inputs),
        ));

        if let Some(session_contexts) = prompts.session_context.created_messages() {
            for context in session_contexts {
                let _ = events_tx.send(TurnEvent::TranscriptMessage {
                    message: ConversationMessage::runtime_context(context.clone()),
                });
            }
        }
        if let Some(turn_contexts) = prompts.turn_context.created_messages() {
            for context in turn_contexts {
                let _ = events_tx.send(TurnEvent::TranscriptMessage {
                    message: ConversationMessage::runtime_context(context.clone()),
                });
            }
        }

        let task_id = match &run.kind {
            AgentRunKind::Task(task) => Some(task.task_id),
            AgentRunKind::Gate(crate::input::GateInput {
                task: Some(task), ..
            }) => Some(task.task_id),
            _ => None,
        };

        let cancel = inst.runtime.execution_cancel.clone();
        let timezone = run.execution.timezone;
        let join = tokio::spawn(async move {
            let completion = turn_loop::TurnCompletion::Natural;
            let mut output = turn_loop::scope_current_execution_timezone(
                timezone,
                turn_loop::run(
                    &inst,
                    messages,
                    Some(events_tx),
                    Some(loop_pause),
                    Some(turn_input_rx),
                    completion,
                    response_delivery,
                ),
            )
            .await?;
            output.task_id = task_id;
            Ok(output)
        });

        Ok(ExecutionHandle {
            events_rx,
            join: Some(join),
            pause_token,
            turn_input,
            cancel,
        })
    }
}

fn raw_user_message(run: &AgentRun) -> String {
    match &run.kind {
        AgentRunKind::Chat(chat) => chat.message.clone(),
        AgentRunKind::FollowUp(follow_up) => follow_up.message.clone(),
        AgentRunKind::Task(task) => {
            if task.instructions.is_empty() {
                task.title.clone()
            } else {
                task.instructions.clone()
            }
        }
        AgentRunKind::Gate(gate) => gate.previous_result.output.clone(),
    }
}

fn run_has_session_context(run: &AgentRun) -> bool {
    match &run.kind {
        AgentRunKind::Chat(chat) => chat.history.iter().any(is_session_context),
        AgentRunKind::FollowUp(follow_up) => follow_up.history.iter().any(is_session_context),
        AgentRunKind::Task(_) | AgentRunKind::Gate(_) => false,
    }
}

fn is_session_context(message: &ConversationMessage) -> bool {
    matches!(
        message,
        ConversationMessage::RuntimeContext(context)
            if context.scope() == RuntimeContextScope::Session
    )
}

fn domain_is_assigned(assigned_domains: &[Slug], domain: &DomainManifest) -> bool {
    assigned_domains
        .iter()
        .any(|assigned| assigned == &domain.slug())
}

fn extend_unique<T: Clone + PartialEq>(target: &mut Vec<T>, additions: &[T]) {
    for addition in additions {
        if !target.contains(addition) {
            target.push(addition.clone());
        }
    }
}

fn active_project_slug<P: ProviderRuntime>(instance: &AgentInstance<P>) -> Option<String> {
    let slug = if instance
        .prompt
        .context
        .render_ctx_extra
        .project
        .slug
        .is_empty()
    {
        instance.prompt.context.current_project.slug.as_str()
    } else {
        instance
            .prompt
            .context
            .render_ctx_extra
            .project
            .slug
            .as_str()
    };
    (!slug.is_empty()).then(|| slug.to_string())
}

fn resolve_active_abilities<P: ProviderRuntime>(
    provider: &P,
    agent: &crate::manifest::AgentManifest,
    active_domain: Option<&DomainManifest>,
) -> Vec<AbilityManifest> {
    let policy = crate::package_resolve::policy_from_agent_metadata(
        agent.source_type.as_deref(),
        Some(&agent.metadata),
    );
    let mut ability_names = agent.abilities.clone();

    if let Some(domain) = active_domain {
        for ability_name in &domain.abilities {
            if !ability_names.contains(ability_name) {
                ability_names.push(ability_name.clone());
            }
        }
    }

    ability_names
        .into_iter()
        .filter_map(|ability_name| {
            provider
                .find_ability_with_policy(&ability_name, &policy)
                .or_else(|| provider.find_ability(&ability_name))
                .cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::build_instruction_messages;

    #[test]
    fn instruction_messages_use_developer_when_supported() {
        let messages = build_instruction_messages("root", "app rules", true);
        assert_eq!(messages.len(), 1);
        let message = messages[0].as_chat().unwrap();
        assert_eq!(message.role, nenjo_models::ChatRole::Developer);
        assert_eq!(
            message.content,
            format!(
                "root\n\napp rules\n\n{}",
                crate::context::runtime::RUNTIME_CONTEXT_PROTOCOL
            )
        );
    }

    #[test]
    fn instruction_messages_fallback_to_system_when_developer_unsupported() {
        let messages = build_instruction_messages("root", "app rules", false);
        assert_eq!(messages.len(), 1);
        let message = messages[0].as_chat().unwrap();
        assert_eq!(message.role, nenjo_models::ChatRole::System);
        assert_eq!(
            message.content,
            format!(
                "root\n\napp rules\n\n{}",
                crate::context::runtime::RUNTIME_CONTEXT_PROTOCOL
            )
        );
    }
}
